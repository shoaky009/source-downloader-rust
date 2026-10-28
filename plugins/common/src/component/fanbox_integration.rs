use crate::http;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, EMPTY_POINTER, ItemFileResolver,
    ItemPointer, PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source,
    SourceFile, SourceItemStream, SourcePointer, deserialize_component_config,
    format_error_chain, source_item_stream,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use time::PrimitiveDateTime;
use time::format_description::well_known::{Iso8601, Rfc2822};
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep_until};
use tracing::{debug, warn};

const CREATOR_PAGE_SIZE: usize = 25;
const ERROR_RESPONSE_BODY_LIMIT: usize = 16 * 1024;
const DEFAULT_RATE_LIMIT_DELAY: Duration = Duration::from_secs(60);
const LOCAL_DATETIME_FORMAT: &[time::format_description::BorrowedFormatItem<'static>] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 ",
    "(KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36"
);

pub struct FanboxIntegrationSupplier;
pub const SUPPLIER: FanboxIntegrationSupplier = FanboxIntegrationSupplier;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct FanboxRequestConfig {
    headers: Option<HashMap<String, String>>,
    request_interval_ms: Option<u32>,
    post_info_interval_ms: Option<u32>,
}

impl ComponentSupplier for FanboxIntegrationSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::source("fanbox".into()),
            ComponentType::file_resolver("fanbox".into()),
        ]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let cookie = props.get("cookie").ok_or_else(|| {
            ComponentError::new(
                "Invalid configuration at 'cookie': missing field `cookie`",
            )
        })?;
        let cookie =
            serde_json::from_value::<String>(cookie.clone()).map_err(|error| {
                ComponentError::new(format!("Invalid configuration at 'cookie': {error}"))
            })?;
        let mode = props.get("mode").and_then(Value::as_str).unwrap_or("all");
        if !matches!(mode, "all" | "latestOnly") {
            return Err(ComponentError::new(
                "Invalid configuration at 'mode': expected either `all` or `latestOnly`",
            ));
        }
        let config = deserialize_component_config::<FanboxRequestConfig>(props)?;
        let headers = config.headers.unwrap_or_else(|| default_headers(cookie));
        let base = props
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://api.fanbox.cc")
            .trim_end_matches('/')
            .to_string();
        let client = http::build_client()?;
        let now = Instant::now();
        Ok(Arc::new(FanboxIntegration {
            client,
            base,
            headers,
            latest_only: mode == "latestOnly",
            request_interval: Duration::from_millis(u64::from(
                config.request_interval_ms.unwrap_or(3000),
            )),
            post_info_interval: Duration::from_millis(u64::from(
                config.post_info_interval_ms.unwrap_or(5000),
            )),
            request_state: Mutex::new(FanboxRequestState {
                request_not_before: now,
                post_info_not_before: now,
            }),
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: concat!(
                "Reads accessible posts from currently supported Fanbox creators and ",
                "resolves covers, images, attachments, text and embedded HTML. ",
                "Source and resolver can share one named component and request pacing. ",
                "Passes authentication headers to the HTTP downloader; no creator-list property.",
            ).into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "cookie":{"type":"string","description":"Complete Cookie request-header value for an account with access to the supported creators. Forwarded to file downloads."},
                    "mode":{
                        "type":"string",
                        "enum":["all","latestOnly"],
                        "default":"all",
                        "description":"all paginates accessible history for currently supported creators using saved cursors; latestOnly reads only the latest 30 supporting posts per fetch."
                    },
                    "headers":{
                        "type":"object",
                        "description":"Completely replaces default API and download headers, including Cookie, Origin and Referer. Supply every required header when overriding.",
                        "additionalProperties":{"type":"string"}
                    },
                    "request-interval-ms":{
                        "type":"integer",
                        "minimum":0,
                        "default":3000,
                        "description":"Minimum interval between Fanbox API requests in milliseconds. HTTP 429 cooldowns always apply."
                    },
                    "post-info-interval-ms":{
                        "type":"integer",
                        "minimum":0,
                        "default":5000,
                        "description":"Minimum interval between post.info requests on this client in milliseconds. Set to 0 to disable this additional interval. API request intervals and HTTP 429 cooldowns still apply."
                    },
                    "base-url":{"type":"string","default":"https://api.fanbox.cc","description":"Fanbox API base URL; normally keep the default."}
                },
                "required":["cookie"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            #[rustfmt::skip]
            source_pointer_json_schema: Some(source_downloader_sdk::serde_json::json!({
                "type": "object",
                "title": "Fanbox 抓取进度",
                "description": "按创作者保存历史补抓位置和最新已处理文章，用于中断续抓及后续增量同步。",
                "properties": {
                    "creatorPointers": {
                        "type": "object",
                        "title": "创作者",
                        "description": "键为创作者 ID；省略时为空映射。",
                        "additionalProperties": {
                            "type": "object",
                            "title": "创作者进度",
                            "description": "Fanbox 按创作者保存历史分页及最新已处理文章。",
                            "properties": {
                                "creatorId": {"type": "string", "title": "创作者 ID", "description": "与外层键对应。"},
                                "nextMaxId": {"type": ["integer", "null"], "title": "下一页文章上界", "description": "历史续抓的文章 ID 上界；完成历史后清空为 null。"},
                                "nextMaxDate": {"type": ["string", "null"], "title": "下一页时间上界", "description": "本地日期时间 YYYY-MM-DDTHH:mm:ss，无时区；null 表示未设置。"},
                                "topId": {"type": ["integer", "null"], "title": "最新文章 ID", "description": "已处理的最新文章 ID；初始为 null。"},
                                "topDate": {"type": ["string", "null"], "title": "最新文章时间", "description": "本地日期时间 YYYY-MM-DDTHH:mm:ss，无时区；初始为 null。"},
                                "touchBottom": {"type": "boolean", "title": "历史已完成", "description": "是否已抓取至该创作者历史末尾；省略时为 false。"}
                            },
                            "additionalProperties": false,
                            "required": ["creatorId"]
                        }
                    }
                },
                "additionalProperties": false
            })),
            #[rustfmt::skip]
            output_schema: Some(source_downloader_sdk::serde_json::json!(
            {
                "type": "object",
                "properties": {
                    "itemAttrs": {
                        "type": "object",
                        "properties": {
                            "likes": {"type": "integer"},
                            "comments": {"type": "integer"},
                            "adult": {"type": "boolean"},
                            "fee": {"type": "integer"},
                            "postId": {"type": "integer"},
                            "username": {"type": "string"},
                            "creatorId": {"type": "string"}
                        },
                        "additionalProperties": false,
                        "description": "Metadata from each fetched Fanbox post."
                    },
                    "itemTags": {"type": "array", "items": {"type": "string"}, "description": "Deduplicated post tags from Fanbox."},
                    "fileAttrs": {
                        "type": "object",
                        "properties": {
                            "type": {"type": "string", "enum": ["cover", "image", "file", "text", "html"]},
                            "height": {"type": "integer", "description": "Images only."},
                            "width": {"type": "integer", "description": "Images only."},
                            "size": {"type": "integer", "description": "Attached files only; byte size."}
                        },
                        "additionalProperties": false,
                        "description": "Resolver outputs vary by post body: optional cover, images, attachments, generated nonempty text, and embed HTML."
                    },
                    "fileTags": {"type": "array", "items": {"type": "string", "enum": ["thumbnail"]}, "description": "thumbnail only when original image returns HTTP 400 and a usable thumbnail fallback is selected. Other resolved files have no initial tags."}
                },
                "additionalProperties": false
            })),
        }))
    }
}

fn default_headers(cookie: String) -> HashMap<String, String> {
    HashMap::from_iter([
        ("Cookie".into(), cookie),
        ("Origin".into(), "https://www.fanbox.cc".into()),
        ("Referer".into(), "https://www.fanbox.cc/".into()),
        ("sec-ch-ua-mobile".into(), "?0".into()),
        ("sec-ch-ua-platform".into(), "\"Windows\"".into()),
        ("sec-fetch-site".into(), "same-site".into()),
        ("sec-fetch-mode".into(), "cors".into()),
        ("sec-fetch-dest".into(), "empty".into()),
        ("accept".into(), "application/json".into()),
        ("accept-language".into(), "en,zh-CN;q=0.9,zh;q=0.8".into()),
        ("priority".into(), "u=1, i".into()),
        ("User-Agent".into(), USER_AGENT.into()),
    ])
}

fn retry_after_delay(headers: &reqwest::header::HeaderMap) -> Duration {
    let Some(value) =
        headers.get(reqwest::header::RETRY_AFTER).and_then(|value| value.to_str().ok())
    else {
        return DEFAULT_RATE_LIMIT_DELAY;
    };
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Duration::from_secs(seconds);
    }
    let Ok(date) = OffsetDateTime::parse(value.trim(), &Rfc2822) else {
        return DEFAULT_RATE_LIMIT_DELAY;
    };
    Duration::try_from(date - OffsetDateTime::now_utc()).unwrap_or_default()
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Source, ItemFileResolver)]
struct FanboxIntegration {
    client: reqwest::Client,
    base: String,
    headers: HashMap<String, String>,
    latest_only: bool,
    request_interval: Duration,
    post_info_interval: Duration,
    request_state: Mutex<FanboxRequestState>,
}

#[derive(Debug)]
struct FanboxRequestState {
    request_not_before: Instant,
    post_info_not_before: Instant,
}

impl Display for FanboxIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "fanbox")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FanboxPointer {
    #[serde(default, rename = "creatorPointers", alias = "creators")]
    creators: HashMap<String, CreatorPointer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatorPointer {
    #[serde(alias = "creator_id")]
    creator_id: String,
    #[serde(alias = "next_max_id")]
    next_max_id: Option<i64>,
    #[serde(alias = "next_max_date")]
    next_max_date: Option<String>,
    #[serde(alias = "top_id")]
    top_id: Option<i64>,
    #[serde(alias = "top_date")]
    top_date: Option<String>,
    #[serde(default, alias = "touch_bottom")]
    touch_bottom: bool,
}
impl CreatorPointer {
    fn new(creator_id: String) -> Self {
        Self {
            creator_id,
            next_max_id: None,
            next_max_date: None,
            top_id: None,
            top_date: None,
            touch_bottom: false,
        }
    }
    fn update(&self, post: &Post, bottom: bool) -> Result<Self, ProcessingError> {
        let newer = post.id > self.top_id.unwrap_or(0);
        let date = format_pointer_date(parse_pointer_date(&post.published_datetime)?)?;
        let touch_bottom = self.touch_bottom || bottom;
        Ok(Self {
            creator_id: self.creator_id.clone(),
            next_max_id: (!touch_bottom).then_some(post.id),
            next_max_date: (!touch_bottom).then(|| date.clone()),
            top_id: if newer { Some(post.id) } else { self.top_id },
            top_date: if newer { Some(date) } else { self.top_date.clone() },
            touch_bottom,
        })
    }

    fn reset_completed_cursor(&mut self) {
        if self.touch_bottom {
            self.next_max_id = None;
            self.next_max_date = None;
        }
    }
}
impl ItemPointer for CreatorPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl SourcePointer for FanboxPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn update(&mut self, _: &SourceItem, pointer: &dyn ItemPointer) {
        if let Some(pointer) = pointer.as_any().downcast_ref::<CreatorPointer>() {
            self.creators.insert(pointer.creator_id.clone(), pointer.clone());
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    body: T,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Supporting {
    creator_id: String,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum SupportingPlans {
    Wrapped { plans: Vec<Supporting> },
    List(Vec<Supporting>),
}
#[derive(Deserialize)]
#[serde(untagged)]
enum CreatorPosts {
    Wrapped { posts: Vec<Post> },
    List(Vec<Post>),
}
#[derive(Deserialize)]
struct Posts {
    #[serde(default)]
    items: Vec<Post>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Post {
    #[serde(deserialize_with = "deserialize_post_id")]
    id: i64,
    title: String,
    creator_id: String,
    is_restricted: bool,
    #[serde(default)]
    like_count: i64,
    #[serde(default)]
    comment_count: i64,
    user: User,
    #[serde(default)]
    fee_required: i64,
    published_datetime: String,
    #[serde(default)]
    has_adult_content: bool,
    #[serde(default)]
    tags: Vec<String>,
}
#[derive(Deserialize)]
struct User {
    name: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum FanboxId {
    Number(i64),
    String(String),
}

fn deserialize_post_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<i64, D::Error> {
    match FanboxId::deserialize(deserializer)? {
        FanboxId::Number(id) => Ok(id),
        FanboxId::String(id) => id.parse().map_err(serde::de::Error::custom),
    }
}

fn deserialize_detail_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    Ok(match FanboxId::deserialize(deserializer)? {
        FanboxId::Number(id) => id.to_string(),
        FanboxId::String(id) => id,
    })
}

fn parse_pointer_date(value: &str) -> Result<PrimitiveDateTime, ProcessingError> {
    // Legacy pointers have local dates; existing Rust pointers include an offset.
    PrimitiveDateTime::parse(value, &Iso8601::DEFAULT).map_err(|error| {
        ProcessingError::non_retryable(format!("Invalid Fanbox cursor date: {error}"))
    })
}

fn format_pointer_date(date: PrimitiveDateTime) -> Result<String, ProcessingError> {
    date.format(LOCAL_DATETIME_FORMAT).map_err(|error| {
        ProcessingError::non_retryable(format!("Invalid Fanbox cursor date: {error}"))
    })
}

impl FanboxIntegration {
    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.headers.iter().fold(
            self.client.get(format!("{}{}", self.base, path)),
            |request, (name, value)| request.header(name, value),
        )
    }

    async fn image_status(
        &self,
        url: &str,
    ) -> Result<reqwest::StatusCode, ProcessingError> {
        let request =
            self.headers.iter().fold(self.client.head(url), |request, (name, value)| {
                request.header(name, value)
            });
        request.send().await.map(|response| response.status()).map_err(|error| {
            http::map_error(error, &format!("Check Fanbox image: HEAD {url}"))
        })
    }

    async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<T, ProcessingError> {
        let request =
            request.build().map_err(|error| http::map_error(error, operation))?;
        let method = request.method().clone();
        let context = format!("{operation}: {method} {}", request.url());
        let is_post_info = request.url().path().ends_with("/post.info");
        // Fetching pages and resolving posts share pacing and rate-limit cooldowns.
        let mut request_state = self.request_state.lock().await;
        let deadline = if is_post_info {
            request_state.request_not_before.max(request_state.post_info_not_before)
        } else {
            request_state.request_not_before
        };
        let wait = deadline.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            debug!(operation, wait_seconds = wait.as_secs_f64(), "Fanbox[request-wait]");
            sleep_until(deadline).await;
        }
        let now = Instant::now();
        request_state.request_not_before = now + self.request_interval;
        if is_post_info {
            request_state.post_info_not_before = now + self.post_info_interval;
        }
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|error| http::map_error(error, &context))?;
        if let Err(error) = response.error_for_status_ref() {
            let status = response.status();
            let url = response.url().clone();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let now = Instant::now();
                let deadline = now
                    .checked_add(retry_after_delay(response.headers()))
                    .unwrap_or(now + DEFAULT_RATE_LIMIT_DELAY);
                request_state.request_not_before =
                    request_state.request_not_before.max(deadline);
                warn!(
                    operation,
                    %url,
                    cooldown_seconds = request_state.request_not_before
                        .saturating_duration_since(now)
                        .as_secs_f64(),
                    "Fanbox[rate-limited] API requests paused until cooldown ends"
                );
            }
            let mut headers = response.headers().clone();
            for (name, value) in headers.iter_mut() {
                if value.is_sensitive()
                    || matches!(
                        name.as_str(),
                        "set-cookie" | "cookie" | "authorization" | "proxy-authorization"
                    )
                {
                    *value = reqwest::header::HeaderValue::from_static("[REDACTED]");
                }
            }
            let body = response.text().await.unwrap_or_else(|error| {
                format!("<failed to read response body: {}>", format_error_chain(&error))
            });
            let truncated = body.len() > ERROR_RESPONSE_BODY_LIMIT;
            let body = &body[..body.floor_char_boundary(ERROR_RESPONSE_BODY_LIMIT)];
            warn!(
                operation,
                %method,
                %url,
                status = status.as_u16(),
                response_headers = ?headers,
                response_body = ?body,
                response_body_truncated = truncated,
                "Fanbox[http-response-error]"
            );
            return Err(http::map_error(error, &context));
        }
        response.json::<ApiResponse<T>>().await.map(|response| response.body).map_err(
            |error| {
                ProcessingError::non_retryable(format!(
                    "Invalid Fanbox response: {}",
                    format_error_chain(&error)
                ))
            },
        )
    }

    async fn creator_posts(
        &self,
        pointer: &CreatorPointer,
    ) -> Result<Vec<Post>, ProcessingError> {
        let mut request = self
            .request("/post.listCreator")
            .query(&[("creatorId", pointer.creator_id.as_str()), ("limit", "25")]);
        if let Some(id) = pointer.next_max_id {
            request = request.query(&[("firstId", id.to_string())]);
        }
        if let Some(date) = pointer.next_max_date.as_deref() {
            let date = parse_pointer_date(date)?
                .checked_sub(time::Duration::SECOND)
                .ok_or_else(|| {
                    ProcessingError::non_retryable("Fanbox cursor date is out of range")
                })?;
            let mut date_string = format_pointer_date(date)?;
            // LocalDateTime.toString() omits zero seconds in the legacy request.
            if date.second() == 0 && date.nanosecond() == 0 {
                date_string.truncate(date_string.len() - 3);
            }
            request = request.query(&[("firstPublishedDatetime", date_string)]);
        }
        let posts = self.json(request, "Fetch Fanbox creator posts").await?;
        Ok(match posts {
            CreatorPosts::Wrapped { posts } | CreatorPosts::List(posts) => posts,
        })
    }

    fn source_item(&self, post: &Post) -> Result<SourceItem, ProcessingError> {
        let uri = Uri::from_str(&format!("{}/posts/{}", self.base, post.id))
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
        let datetime = OffsetDateTime::parse(
            &post.published_datetime,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|error| {
            ProcessingError::non_retryable(format!("Invalid Fanbox date: {error}"))
        })?;
        let attrs = Map::from_iter([
            ("likes".into(), Value::from(post.like_count)),
            ("comments".into(), Value::from(post.comment_count)),
            ("adult".into(), Value::from(post.has_adult_content)),
            ("fee".into(), Value::from(post.fee_required)),
            ("postId".into(), Value::from(post.id)),
            ("username".into(), Value::String(post.user.name.clone())),
            ("creatorId".into(), Value::String(post.creator_id.clone())),
        ]);
        let mut seen_tags = HashSet::new();
        Ok(SourceItem {
            title: post.title.clone(),
            link: uri.clone(),
            datetime,
            content_type: "fanbox".into(),
            download_uri: uri,
            attrs,
            tags: post
                .tags
                .iter()
                .filter(|tag| seen_tags.insert(*tag))
                .cloned()
                .collect(),
            identity: None,
        })
    }
}

#[async_trait]
impl Source for FanboxIntegration {
    async fn fetch(
        &self,
        pointer: &dyn SourcePointer,
        limit: u32,
    ) -> Result<SourceItemStream, ProcessingError> {
        if self.latest_only {
            let posts: Posts = self
                .json(
                    self.request("/post.listSupporting").query(&[("limit", "30")]),
                    "Fetch Fanbox supporting posts",
                )
                .await?;
            let items = posts
                .items
                .into_iter()
                .filter(|post| !post.is_restricted)
                .map(|post| {
                    Ok(PointedItem {
                        source_item: self.source_item(&post)?,
                        item_pointer: EMPTY_POINTER.clone(),
                    })
                })
                .collect::<Result<Vec<_>, ProcessingError>>()?;
            return Ok(source_item_stream(items));
        }
        let pointer =
            pointer.as_any().downcast_ref::<FanboxPointer>().ok_or_else(|| {
                ProcessingError::non_retryable("Invalid Fanbox source pointer")
            })?;
        let supportings = self
            .json(self.request("/plan.listSupporting"), "Fetch Fanbox supportings")
            .await?;
        let supportings = match supportings {
            SupportingPlans::Wrapped { plans } | SupportingPlans::List(plans) => plans,
        };
        let mut output = Vec::new();
        for supporting in supportings {
            let mut state =
                pointer.creators.get(&supporting.creator_id).cloned().unwrap_or_else(
                    || CreatorPointer::new(supporting.creator_id.clone()),
                );
            state.reset_completed_cursor();
            if let Some(date) = &mut state.next_max_date {
                *date = format_pointer_date(parse_pointer_date(date)?)?;
            }
            let touch_bottom = state.touch_bottom;
            let last_top_id = state.top_id.unwrap_or(0);
            loop {
                let posts = self.creator_posts(&state).await?;
                let Some(last_post) = posts.last() else {
                    break;
                };
                let last_id = last_post.id;
                let last_date = format_pointer_date(parse_pointer_date(
                    &last_post.published_datetime,
                )?)?;
                if state.next_max_id == Some(last_id)
                    && state.next_max_date.as_deref() == Some(last_date.as_str())
                {
                    warn!(
                        "Fanbox[pagination-stalled] creator_id={}",
                        supporting.creator_id
                    );
                    break;
                }
                let bottom = posts.len() < CREATOR_PAGE_SIZE;
                let previous_len = output.len();
                for post in posts.iter().filter(|post| !post.is_restricted) {
                    state = state.update(post, bottom)?;
                    if touch_bottom && post.id <= last_top_id {
                        continue;
                    }
                    output.push(PointedItem {
                        source_item: self.source_item(post)?,
                        item_pointer: Arc::new(state.clone()),
                    });
                }
                debug!(
                    "Fanbox[creator-page] creator_id={} items={} touch_bottom={}",
                    supporting.creator_id,
                    output.len() - previous_len,
                    state.touch_bottom
                );
                // Keep pages intact and still visit every supported creator.
                if output.len() >= limit as usize || bottom || touch_bottom {
                    break;
                }
                // Restricted posts still advance pagination, but emit no item pointer.
                state.next_max_id = Some(last_id);
                state.next_max_date = Some(last_date);
            }
        }
        Ok(source_item_stream(output))
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(FanboxPointer::default())
    }

    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
        let mut pointer =
            serde_json::from_value::<FanboxPointer>(value).unwrap_or_default();
        for creator in pointer.creators.values_mut() {
            creator.reset_completed_cursor();
        }
        Box::new(pointer)
    }

    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        Some(self.headers.clone())
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PostInfo {
    Wrapped { post: PostDetail },
    Direct(PostDetail),
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostDetail {
    #[serde(deserialize_with = "deserialize_detail_id")]
    id: String,
    #[serde(default)]
    is_restricted: bool,
    cover_image_url: Option<String>,
    body: Option<Media>,
}
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Media {
    #[serde(default)]
    blocks: Vec<Block>,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    files: Vec<FanboxFile>,
    #[serde(default)]
    image_map: HashMap<String, Image>,
    #[serde(default)]
    file_map: HashMap<String, FanboxFile>,
    #[serde(default)]
    url_embed_map: HashMap<String, UrlEmbed>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    image_id: Option<String>,
    file_id: Option<String>,
    url_embed_id: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Image {
    id: String,
    extension: String,
    height: i64,
    width: i64,
    original_url: String,
    thumbnail_url: Option<String>,
}
#[derive(Clone, Deserialize)]
struct FanboxFile {
    extension: String,
    name: String,
    size: i64,
    url: String,
}
#[derive(Deserialize)]
struct UrlEmbed {
    id: String,
    html: Option<String>,
}

#[async_trait]
impl ItemFileResolver for FanboxIntegration {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let post_id =
            item.link.path().rsplit('/').next().filter(|id| !id.is_empty()).ok_or_else(
                || ProcessingError::non_retryable("Invalid Fanbox post URL"),
            )?;
        let post = self
            .json(
                self.request("/post.info").query(&[("postId", post_id)]),
                "Fetch Fanbox post",
            )
            .await?;
        let post = match post {
            PostInfo::Wrapped { post } | PostInfo::Direct(post) => post,
        };
        if post.is_restricted {
            return Err(ProcessingError::skip(format!(
                "Fanbox post {} is restricted",
                post.id
            )));
        }
        let body = post.body.ok_or_else(|| {
            ProcessingError::non_retryable(format!(
                "Fanbox post {} has no media body",
                post.id
            ))
        })?;
        let mut files = Vec::new();
        if let Some(url) = post.cover_image_url {
            files.push(remote_file(
                format!("cover_{}.jpeg", post.id),
                url,
                Map::from_iter([("type".into(), Value::String("cover".into()))]),
            )?);
        }
        let mut ordered_images = Vec::new();
        let mut ordered_files = Vec::new();
        let mut text = Vec::new();
        let mut embeds = Vec::new();
        for block in &body.blocks {
            if let Some(value) = &block.text {
                text.push(value.as_str());
            }
            match block.kind.as_str() {
                "image" => {
                    if let Some(image) =
                        block.image_id.as_ref().and_then(|id| body.image_map.get(id))
                    {
                        ordered_images.push(image.clone());
                    }
                }
                "file" => {
                    if let Some(file) =
                        block.file_id.as_ref().and_then(|id| body.file_map.get(id))
                    {
                        ordered_files.push(file.clone());
                    }
                }
                "url_embed" => {
                    if let Some(embed) = block
                        .url_embed_id
                        .as_ref()
                        .and_then(|id| body.url_embed_map.get(id))
                    {
                        embeds.push(embed);
                    }
                }
                _ => {}
            }
        }
        ordered_images.extend(body.images);
        for image in ordered_images {
            let mut url = image.original_url;
            let mut tags = Vec::new();
            if self.image_status(&url).await? == reqwest::StatusCode::BAD_REQUEST {
                let Some(thumbnail_url) = image.thumbnail_url else {
                    warn!(
                        post_id = %post.id,
                        image_id = %image.id,
                        original_url = %url,
                        "Fanbox[image-skipped] originalUrl returned HTTP 400 and thumbnailUrl is missing"
                    );
                    continue;
                };
                if self.image_status(&thumbnail_url).await?
                    == reqwest::StatusCode::BAD_REQUEST
                {
                    warn!(
                        post_id = %post.id,
                        image_id = %image.id,
                        original_url = %url,
                        %thumbnail_url,
                        "Fanbox[image-skipped] originalUrl and thumbnailUrl both returned HTTP 400"
                    );
                    continue;
                }
                debug!(
                    post_id = %post.id,
                    image_id = %image.id,
                    original_url = %url,
                    %thumbnail_url,
                    "Fanbox[image-thumbnail-fallback] originalUrl returned HTTP 400; using thumbnailUrl"
                );
                url = thumbnail_url;
                tags.push("thumbnail".into());
            }
            let mut file = remote_file(
                format!("{}.{}", image.id, image.extension),
                url,
                Map::from_iter([
                    ("height".into(), Value::from(image.height)),
                    ("width".into(), Value::from(image.width)),
                    ("type".into(), Value::String("image".into())),
                ]),
            )?;
            file.tags = tags;
            files.push(file);
        }
        ordered_files.extend(body.files);
        let original_names = ordered_files
            .iter()
            .map(|file| format!("{}.{}", file.name, file.extension))
            .collect::<HashSet<_>>();
        let mut used_names = HashSet::new();
        for file in ordered_files {
            let mut filename = format!("{}.{}", file.name, file.extension);
            let mut suffix = 0;
            while used_names.contains(&filename)
                || (suffix > 0 && original_names.contains(&filename))
            {
                suffix += 1;
                filename = format!("{}_{suffix}.{}", file.name, file.extension);
            }
            used_names.insert(filename.clone());
            files.push(remote_file(
                filename,
                file.url,
                Map::from_iter([
                    ("size".into(), Value::from(file.size)),
                    ("type".into(), Value::String("file".into())),
                ]),
            )?);
        }
        let joined = text.join("\n");
        if !joined.trim().is_empty() {
            files.push(SourceFile {
                path: PathBuf::from(format!("text_{}.txt", post.id)),
                attrs: Map::from_iter([("type".into(), Value::String("text".into()))]),
                download_uri: None,
                tags: Vec::new(),
                data: Some(Arc::from(joined.into_bytes())),
            });
        }
        for embed in embeds {
            if let Some(html) = &embed.html {
                files.push(SourceFile {
                    path: PathBuf::from(format!("{}.html", embed.id)),
                    attrs: Map::from_iter([(
                        "type".into(),
                        Value::String("html".into()),
                    )]),
                    download_uri: None,
                    tags: Vec::new(),
                    data: Some(Arc::from(html.as_bytes())),
                });
            }
        }
        Ok(files)
    }
}

fn remote_file(
    path: String,
    url: String,
    attrs: Map<String, Value>,
) -> Result<SourceFile, ProcessingError> {
    Ok(SourceFile {
        path: PathBuf::from(path),
        attrs,
        download_uri: Some(Uri::from_str(&url).map_err(|error| {
            ProcessingError::non_retryable(format!("Invalid Fanbox file URL: {error}"))
        })?),
        tags: Vec::new(),
        data: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::TryStreamExt;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn props(server: &MockServer) -> Map<String, Value> {
        Map::from_iter([
            ("cookie".into(), Value::String("FANBOXSESSID=x".into())),
            ("base-url".into(), Value::String(server.uri())),
            ("request-interval-ms".into(), json!(0)),
            ("post-info-interval-ms".into(), json!(0)),
        ])
    }

    fn post(id: i64, creator: &str, restricted: bool) -> Value {
        json!({
            "id": id.to_string(), "title": format!("Post {id}"),
            "creatorId": creator, "isRestricted": restricted,
            "likeCount": 1, "commentCount": 2, "user": {"name": "Alice"},
            "feeRequired": 0, "publishedDatetime": "2026-08-07T12:00:00+09:00",
            "hasAdultContent": false, "tags": ["tag", "tag", "other"]
        })
    }

    fn component(server: &MockServer) -> Arc<dyn SdComponent> {
        SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props(server),
            )
            .unwrap()
    }

    async fn supportings(server: &MockServer, creators: &[&str], requests: u64) {
        let creators =
            creators.iter().map(|id| json!({"creatorId": id})).collect::<Vec<_>>();
        Mock::given(method("GET"))
            .and(path("/plan.listSupporting"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"plans": creators}
            })))
            .expect(requests)
            .mount(server)
            .await;
    }

    async fn post_info(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/post.info"))
            .and(query_param("postId", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"body": body})))
            .expect(1)
            .mount(server)
            .await;
    }

    fn no_creator_cursor(request: &wiremock::Request) -> bool {
        !request
            .url
            .query_pairs()
            .any(|(key, _)| key == "firstId" || key == "firstPublishedDatetime")
    }

    fn item(server: &MockServer) -> SourceItem {
        SourceItem {
            title: "Post".into(),
            link: format!("{}/posts/10", server.uri()).parse().unwrap(),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: "fanbox".into(),
            download_uri: format!("{}/posts/10", server.uri()).parse().unwrap(),
            attrs: Map::new(),
            tags: Vec::new(),
            identity: None,
        }
    }

    #[tokio::test]
    async fn rejects_supporting_response_without_plans() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/plan.listSupporting"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"error": "authentication_required"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let result = source.fetch(source.default_pointer().as_ref(), 1).await;
        assert!(matches!(result, Err(ProcessingError::NonRetryable { skip: false, .. })));
    }

    #[tokio::test]
    async fn rejects_creator_response_without_posts() {
        let server = MockServer::start().await;
        supportings(&server, &["alice"], 1).await;
        Mock::given(method("GET"))
            .and(path("/post.listCreator"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"body": {}})))
            .expect(1)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let result = source.fetch(source.default_pointer().as_ref(), 1).await;
        assert!(matches!(result, Err(ProcessingError::NonRetryable { skip: false, .. })));
    }

    #[tokio::test]
    async fn fetches_full_pages_and_visits_every_creator() {
        let server = MockServer::start().await;
        supportings(&server, &["alice", "bob"], 1).await;
        let first_page =
            (36..=60).rev().map(|id| post(id, "alice", false)).collect::<Vec<_>>();
        let last_page =
            (21..=35).rev().map(|id| post(id, "alice", false)).collect::<Vec<_>>();
        Mock::given(path("/post.listCreator"))
            .and(query_param("creatorId", "alice"))
            .and(query_param("limit", "25"))
            .and(no_creator_cursor)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"posts": first_page}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/post.listCreator"))
            .and(query_param("creatorId", "alice"))
            .and(query_param("firstId", "36"))
            .and(query_param("firstPublishedDatetime", "2026-08-07T11:59:59"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"posts": last_page}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/post.listCreator"))
            .and(query_param("creatorId", "bob"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"posts": [post(200, "bob", false), post(199, "bob", true)]}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let mut pointer = source.default_pointer();
        let items = source
            .fetch(pointer.as_ref(), 30)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        let ids = items
            .iter()
            .map(|item| item.source_item.attrs["postId"].as_i64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, (21..=60).rev().chain([200]).collect::<Vec<_>>());
        for item in &items {
            pointer.update(&item.source_item, item.item_pointer.as_ref());
        }
        let state = pointer.dump();
        assert_eq!(state["creatorPointers"]["alice"]["topId"], 60);
        assert_eq!(state["creatorPointers"]["alice"]["topDate"], "2026-08-07T12:00:00");
        assert_eq!(state["creatorPointers"]["alice"]["touchBottom"], true);
        assert!(state["creatorPointers"]["alice"]["nextMaxId"].is_null());
        assert_eq!(state["creatorPointers"]["bob"]["topId"], 200);
        let first_state =
            items[24].item_pointer.as_any().downcast_ref::<CreatorPointer>().unwrap();
        assert_eq!(first_state.next_max_id, Some(36));
        assert_eq!(first_state.top_id, Some(60));
        assert!(!first_state.touch_bottom);
    }

    #[tokio::test]
    async fn keeps_an_entire_creator_page_when_limit_is_one() {
        let server = MockServer::start().await;
        supportings(&server, &["alice", "bob"], 1).await;
        Mock::given(path("/post.listCreator"))
            .and(query_param("creatorId", "alice"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": [post(10, "alice", false), post(9, "alice", false)]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/post.listCreator"))
            .and(query_param("creatorId", "bob"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": [post(20, "bob", false), post(19, "bob", false)]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let items = source
            .fetch(source.default_pointer().as_ref(), 1)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(items.len(), 4);
    }

    #[tokio::test]
    async fn resumes_legacy_and_existing_rust_pointers() {
        let server = MockServer::start().await;
        supportings(&server, &["alice"], 2).await;
        Mock::given(path("/post.listCreator"))
            .and(query_param("creatorId", "alice"))
            .and(query_param("firstId", "95"))
            .and(query_param("firstPublishedDatetime", "2026-08-07T12:00"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": [post(94, "alice", false)]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        for raw in [
            json!({"creatorPointers": {"alice": {
                "creatorId": "alice", "nextMaxId": 95,
                "nextMaxDate": "2026-08-07T12:00:01", "topId": 100,
                "topDate": "2026-08-08T12:00:00", "touchBottom": false
            }}}),
            json!({"creators": {"alice": {
                "creator_id": "alice", "next_max_id": 95,
                "next_max_date": "2026-08-07T12:00:01+09:00", "top_id": 100,
                "top_date": "2026-08-08T12:00:00+09:00", "touch_bottom": false
            }}}),
        ] {
            let mut pointer = source.parse_raw_pointer(raw);
            let items = source
                .fetch(pointer.as_ref(), 1)
                .await
                .unwrap()
                .try_collect::<Vec<_>>()
                .await
                .unwrap();
            assert_eq!(items[0].source_item.attrs["postId"], 94);
            pointer.update(&items[0].source_item, items[0].item_pointer.as_ref());
            assert_eq!(pointer.dump()["creatorPointers"]["alice"]["topId"], 100);
            assert_eq!(pointer.dump()["creatorPointers"]["alice"]["touchBottom"], true);
        }
    }

    #[tokio::test]
    async fn completed_creator_fetches_latest_posts_and_preserves_top_id() {
        let server = MockServer::start().await;
        supportings(&server, &["alice"], 2).await;
        Mock::given(path("/post.listCreator"))
            .and(no_creator_cursor)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": [post(12, "alice", false), post(11, "alice", true),
                    post(10, "alice", false), post(9, "alice", false)]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let mut pointer = source.parse_raw_pointer(json!({"creators": {"alice": {
            "creator_id": "alice", "next_max_id": 3,
            "next_max_date": "2026-08-01T12:00:00+09:00",
            "top_id": 10, "touch_bottom": true
        }}}));
        assert!(pointer.dump()["creatorPointers"]["alice"]["nextMaxId"].is_null());
        let items = source
            .fetch(pointer.as_ref(), 10)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source_item.attrs["postId"], 12);
        pointer.update(&items[0].source_item, items[0].item_pointer.as_ref());
        assert_eq!(pointer.dump()["creatorPointers"]["alice"]["topId"], 12);
        let next = source
            .fetch(pointer.as_ref(), 10)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(next.is_empty());
    }

    #[tokio::test]
    async fn latest_only_returns_the_available_page_without_updating_pointer() {
        let server = MockServer::start().await;
        Mock::given(path("/post.listSupporting"))
            .and(query_param("limit", "30"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"body": {
                "items": [post(12, "alice", false), post(11, "alice", true),
                    post(10, "alice", false)]
            }})))
            .expect(1)
            .mount(&server)
            .await;
        let mut config = props(&server);
        config.insert("mode".into(), json!("latestOnly"));
        let source = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &config,
            )
            .unwrap()
            .as_source()
            .unwrap();
        let mut pointer = source.parse_raw_pointer(json!({"creatorPointers": {"alice": {
            "creatorId": "alice", "topId": 1, "touchBottom": true
        }}}));
        let before = pointer.dump();
        let items = source
            .fetch(pointer.as_ref(), 1)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].source_item.tags, ["tag", "other"]);
        assert_eq!(
            items[0].source_item.link.to_string(),
            format!("{}/posts/12", server.uri())
        );
        assert_eq!(
            items[0].source_item.attrs,
            json!({
                "likes": 1, "comments": 2, "adult": false, "fee": 0,
                "postId": 12, "username": "Alice", "creatorId": "alice"
            })
            .as_object()
            .unwrap()
            .clone()
        );
        for item in &items {
            pointer.update(&item.source_item, item.item_pointer.as_ref());
        }
        assert_eq!(pointer.dump(), before);
    }

    #[tokio::test]
    async fn restricted_pages_still_advance_pagination() {
        let server = MockServer::start().await;
        supportings(&server, &["alice"], 1).await;
        let restricted =
            (6..=30).rev().map(|id| post(id, "alice", true)).collect::<Vec<_>>();
        Mock::given(path("/post.listCreator"))
            .and(no_creator_cursor)
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"body": restricted})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/post.listCreator"))
            .and(query_param("firstId", "6"))
            .and(query_param("firstPublishedDatetime", "2026-08-07T11:59:59"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": [post(5, "alice", false)]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let items = source
            .fetch(source.default_pointer().as_ref(), 10)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].source_item.attrs["postId"], 5);
    }

    #[tokio::test]
    async fn stops_on_an_empty_creator_page() {
        let server = MockServer::start().await;
        supportings(&server, &["alice"], 1).await;
        Mock::given(path("/post.listCreator"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"posts": []}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let items = source
            .fetch(source.default_pointer().as_ref(), 10)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn stops_on_a_repeated_creator_page() {
        let server = MockServer::start().await;
        supportings(&server, &["alice"], 1).await;
        let posts =
            (36..=60).rev().map(|id| post(id, "alice", false)).collect::<Vec<_>>();
        Mock::given(path("/post.listCreator"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"body": posts})),
            )
            .expect(2)
            .mount(&server)
            .await;
        let source = component(&server).as_source().unwrap();
        let items = source
            .fetch(source.default_pointer().as_ref(), 50)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(items.len(), 25);
    }

    #[tokio::test]
    async fn sends_and_exposes_legacy_default_headers() {
        let server = MockServer::start().await;
        supportings(&server, &[], 1).await;
        let source = component(&server).as_source().unwrap();
        let items = source
            .fetch(source.default_pointer().as_ref(), 1)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(items.is_empty());
        let headers = source.headers(&item(&server)).unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(headers.len(), 12);
        for (name, value) in &headers {
            assert_eq!(requests[0].headers.get(name).unwrap().to_str().unwrap(), value);
        }
        assert_eq!(headers["Cookie"], "FANBOXSESSID=x");
        assert_eq!(headers["User-Agent"], USER_AGENT);
        assert_eq!(headers["sec-fetch-site"], "same-site");
        assert_eq!(headers["accept-language"], "en,zh-CN;q=0.9,zh;q=0.8");
    }

    #[tokio::test]
    async fn custom_headers_replace_the_defaults() {
        let server = MockServer::start().await;
        supportings(&server, &[], 1).await;
        let mut config = props(&server);
        config.insert(
            "headers".into(),
            json!({"cookie": "custom", "user-agent": "browser"}),
        );
        let source = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &config,
            )
            .unwrap()
            .as_source()
            .unwrap();
        let items = source
            .fetch(source.default_pointer().as_ref(), 1)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(items.is_empty());
        assert_eq!(
            source.headers(&item(&server)).unwrap(),
            HashMap::from_iter([
                ("cookie".into(), "custom".into()),
                ("user-agent".into(), "browser".into()),
            ])
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests[0].headers.get("cookie").unwrap(), "custom");
        assert_eq!(requests[0].headers.get("user-agent").unwrap(), "browser");
        assert!(!requests[0].headers.contains_key("origin"));
        assert!(!requests[0].headers.contains_key("referer"));
    }

    #[tokio::test]
    async fn fetches_creator_and_resumes_from_pointer() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/plan.listSupporting"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"body":[{"creatorId":"alice"}]})),
            )
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/post.listCreator"))
            .and(query_param("creatorId", "alice"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "body":[{"id":10,"title":"Post","creatorId":"alice","isRestricted":false,
                "likeCount":1,"commentCount":2,"user":{"name":"Alice"},"feeRequired":0,
                "publishedDatetime":"2026-08-07T12:00:00+09:00","hasAdultContent":false,"tags":["tag"]}]
            })))
            .expect(2)
            .mount(&server)
            .await;
        let source = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props(&server),
            )
            .unwrap()
            .as_source()
            .unwrap();
        let mut pointer = source.default_pointer();
        let first = source
            .fetch(pointer.as_ref(), 10)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert_eq!(1, first.len());
        pointer.update(&first[0].source_item, first[0].item_pointer.as_ref());
        let second = source
            .fetch(pointer.as_ref(), 10)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn resolves_wrapped_post_details() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/i1"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        post_info(
            &server,
            json!({"post": {
                "id": "10", "isRestricted": false,
                "coverImageUrl": "https://cdn/cover",
                "body": {"images": [{
                    "id": "i1", "extension": "png", "height": 2, "width": 3,
                    "originalUrl": format!("{}/i1", server.uri())
                }]}
            }}),
        )
        .await;
        let resolver = component(&server).as_item_file_resolver().unwrap();
        let files = resolver.resolve_files(&item(&server)).await.unwrap();
        assert_eq!(
            files.iter().map(|file| file.path.to_str().unwrap()).collect::<Vec<_>>(),
            ["cover_10.jpeg", "i1.png"]
        );
        assert_eq!(
            files
                .iter()
                .map(|file| file.download_uri.as_ref().unwrap().to_string())
                .collect::<Vec<_>>(),
            ["https://cdn/cover".to_string(), format!("{}/i1", server.uri())]
        );
    }

    #[tokio::test]
    async fn falls_back_to_thumbnails_and_skips_images_with_both_urls_unavailable() {
        let server = MockServer::start().await;
        let cases = [
            ("normal", 200, None),
            ("fallback", 400, Some(200)),
            ("unavailable", 400, Some(400)),
            ("after", 200, None),
        ];
        let mut images = Vec::new();
        for (id, original_status, thumbnail_status) in cases {
            Mock::given(method("HEAD"))
                .and(path(format!("/original/{id}")))
                .and(header("Cookie", "FANBOXSESSID=x"))
                .and(header("Referer", "https://www.fanbox.cc/"))
                .respond_with(ResponseTemplate::new(original_status))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("HEAD"))
                .and(path(format!("/thumbnail/{id}")))
                .and(header("Cookie", "FANBOXSESSID=x"))
                .and(header("Referer", "https://www.fanbox.cc/"))
                .respond_with(ResponseTemplate::new(thumbnail_status.unwrap_or(200)))
                .expect(u64::from(thumbnail_status.is_some()))
                .mount(&server)
                .await;
            images.push(json!({
                "id": id, "extension": "png", "height": 2, "width": 3,
                "originalUrl": format!("{}/original/{id}", server.uri()),
                "thumbnailUrl": format!("{}/thumbnail/{id}", server.uri())
            }));
        }
        post_info(&server, json!({"id": "10", "body": {"images": images}})).await;
        let resolver = component(&server).as_item_file_resolver().unwrap();

        let files = resolver.resolve_files(&item(&server)).await.unwrap();

        assert_eq!(
            files.iter().map(|file| file.path.to_str().unwrap()).collect::<Vec<_>>(),
            ["normal.png", "fallback.png", "after.png"]
        );
        assert_eq!(
            files
                .iter()
                .map(|file| file.download_uri.as_ref().unwrap().to_string())
                .collect::<Vec<_>>(),
            ["/original/normal", "/thumbnail/fallback", "/original/after"]
                .map(|path| format!("{}{path}", server.uri()))
        );
    }

    #[tokio::test]
    async fn skips_restricted_posts_with_null_media() {
        let server = MockServer::start().await;
        post_info(
            &server,
            json!({"post": {
                "id": "10", "isRestricted": true,
                "coverImageUrl": "https://cdn/cover", "body": null
            }}),
        )
        .await;
        let resolver = component(&server).as_item_file_resolver().unwrap();
        let result = resolver.resolve_files(&item(&server)).await;
        assert!(matches!(result, Err(ProcessingError::NonRetryable { skip: true, .. })));
    }

    #[tokio::test]
    async fn rejects_unrestricted_posts_with_null_media() {
        let server = MockServer::start().await;
        post_info(
            &server,
            json!({"post": {"id": "10", "isRestricted": false, "body": null}}),
        )
        .await;
        let resolver = component(&server).as_item_file_resolver().unwrap();
        let result = resolver.resolve_files(&item(&server)).await;
        assert!(matches!(
            result,
            Err(ProcessingError::NonRetryable { skip: false, message })
                if message.contains("has no media body")
        ));
    }

    #[tokio::test]
    async fn disambiguates_duplicate_attachment_names() {
        let server = MockServer::start().await;
        let attachments = [
            ("PNG出力まとめ", "zip"),
            ("PNG出力まとめ", "zip"),
            ("PNG出力まとめ", "zip"),
            ("PNG出力まとめ", "pdf"),
            ("archive", "zip"),
            ("archive", "zip"),
        ];
        let mut blocks = Vec::new();
        let mut file_map = Map::new();
        for (index, (name, extension)) in attachments.into_iter().enumerate() {
            let id = format!("f{index}");
            blocks.push(json!({"type": "file", "fileId": id}));
            file_map.insert(
                id.clone(),
                json!({
                    "id": id, "name": name, "extension": extension, "size": 7863522,
                    "url": format!("https://cdn/f{index}")
                }),
            );
        }
        post_info(
            &server,
            json!({"id": "10", "body": {
                "blocks": blocks,
                "fileMap": file_map,
                "files": [{
                    "id": "f6", "name": "archive_1", "extension": "zip",
                    "size": 42, "url": "https://cdn/f6"
                }]
            }}),
        )
        .await;
        let resolver = component(&server).as_item_file_resolver().unwrap();

        let files = resolver.resolve_files(&item(&server)).await.unwrap();

        assert_eq!(
            files.iter().map(|file| file.path.to_str().unwrap()).collect::<Vec<_>>(),
            [
                "PNG出力まとめ.zip",
                "PNG出力まとめ_1.zip",
                "PNG出力まとめ_2.zip",
                "PNG出力まとめ.pdf",
                "archive.zip",
                "archive_2.zip",
                "archive_1.zip",
            ]
        );
        assert_eq!(files[1].download_uri.as_ref().unwrap().to_string(), "https://cdn/f1");
        assert_eq!(files[1].attrs["size"], 7863522);
    }

    #[tokio::test]
    async fn resolves_images_files_text_and_embeds() {
        let server = MockServer::start().await;
        for image_path in ["/i1", "/i2", "/i3"] {
            Mock::given(method("HEAD"))
                .and(path(image_path))
                .respond_with(ResponseTemplate::new(200))
                .expect(1)
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/post.info"))
            .and(query_param("postId", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "body": {"id": 10, "coverImageUrl": "https://cdn/cover", "body": {
                    "blocks": [
                        {"type": "text", "text": "hello"},
                        {"type": "image", "imageId": "i2", "text": "caption"},
                        {"type": "file", "fileId": "f", "text": "attachment"},
                        {"type": "url_embed", "urlEmbedId": "u2", "text": "embed"},
                        {"type": "image", "imageId": "i1"},
                        {"type": "url_embed", "urlEmbedId": "u1"},
                        {"type": "url_embed", "urlEmbedId": "u3"},
                        {"type": "image", "imageId": "missing"},
                        {"type": "text", "text": "end"}
                    ],
                    "imageMap": {
                        "i1": {"id": "i1", "extension": "png", "height": 2, "width": 3,
                            "originalUrl": format!("{}/i1", server.uri())},
                        "i2": {"id": "i2", "extension": "jpg", "height": 4, "width": 5,
                            "originalUrl": format!("{}/i2", server.uri())}
                    },
                    "images": [{"id": "i3", "extension": "webp", "height": 6, "width": 7,
                        "originalUrl": format!("{}/i3", server.uri())}],
                    "fileMap": {"f": {"id": "f", "name": "archive", "extension": "zip",
                        "size": 42, "url": "https://cdn/archive"}},
                    "files": [{"id": "f2", "name": "notes", "extension": "pdf",
                        "size": 50, "url": "https://cdn/notes"}],
                    "urlEmbedMap": {
                        "u1": {"id": "u1", "html": null},
                        "u2": {"id": "u2", "html": "<b>x</b>"},
                        "u3": {"id": "u3", "html": "<b>y</b>"}
                    }
                }}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let resolver = component(&server).as_item_file_resolver().unwrap();
        let files = resolver.resolve_files(&item(&server)).await.unwrap();
        assert_eq!(
            files.iter().map(|file| file.path.clone()).collect::<Vec<_>>(),
            [
                "cover_10.jpeg",
                "i2.jpg",
                "i1.png",
                "i3.webp",
                "archive.zip",
                "notes.pdf",
                "text_10.txt",
                "u2.html",
                "u3.html",
            ]
            .map(PathBuf::from)
        );
        assert_eq!(files[0].attrs["type"], "cover");
        assert_eq!(
            files[1].attrs,
            json!({"type": "image", "height": 4, "width": 5})
                .as_object()
                .unwrap()
                .clone()
        );
        assert_eq!(
            files[4].attrs,
            json!({"type": "file", "size": 42}).as_object().unwrap().clone()
        );
        assert_eq!(
            files[4].download_uri.as_ref().unwrap().to_string(),
            "https://cdn/archive"
        );
        assert_eq!(files[6].attrs["type"], "text");
        assert_eq!(
            files[6].data.as_deref().unwrap(),
            b"hello\ncaption\nattachment\nembed\nend"
        );
        assert!(files[6].download_uri.is_none());
        assert_eq!(files[7].attrs["type"], "html");
        assert_eq!(files[7].data.as_deref().unwrap(), b"<b>x</b>");
        assert_eq!(files[8].data.as_deref().unwrap(), b"<b>y</b>");
    }
}

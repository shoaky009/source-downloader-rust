use crate::http;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, ItemPointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source, SourceFile,
    SourcePointer,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

pub struct FanboxIntegrationSupplier;
pub const SUPPLIER: FanboxIntegrationSupplier = FanboxIntegrationSupplier;

impl ComponentSupplier for FanboxIntegrationSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::source("fanbox".into()),
            ComponentType::file_resolver("fanbox".into()),
        ]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let cookie = props
            .get("cookie")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::new("Missing or invalid 'cookie' property"))?
            .to_string();
        let mode = props.get("mode").and_then(Value::as_str).unwrap_or("all");
        if !matches!(mode, "all" | "latestOnly") {
            return Err(ComponentError::new("Invalid 'mode' property"));
        }
        let mut headers = props
            .get("headers")
            .map(|value| {
                serde_json::from_value::<HashMap<String, String>>(value.clone()).map_err(
                    |error| ComponentError::new(format!("Invalid 'headers': {error}")),
                )
            })
            .transpose()?
            .unwrap_or_default();
        headers.entry("Cookie".into()).or_insert(cookie);
        headers.entry("Origin".into()).or_insert("https://www.fanbox.cc".into());
        headers.entry("Referer".into()).or_insert("https://www.fanbox.cc/".into());
        headers.entry("Accept".into()).or_insert("application/json".into());
        let base = props
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://api.fanbox.cc")
            .trim_end_matches('/')
            .to_string();
        let client = if base.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|error| ComponentError::new(error.to_string()))?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(FanboxIntegration {
            client,
            base,
            headers,
            latest_only: mode == "latestOnly",
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Source, ItemFileResolver)]
struct FanboxIntegration {
    client: reqwest::Client,
    base: String,
    headers: HashMap<String, String>,
    latest_only: bool,
}

impl Display for FanboxIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "fanbox")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FanboxPointer {
    #[serde(default)]
    creators: HashMap<String, CreatorPointer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CreatorPointer {
    creator_id: String,
    next_max_id: Option<i64>,
    next_max_date: Option<String>,
    top_id: Option<i64>,
    top_date: Option<String>,
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
    fn update(&self, post: &Post, bottom: bool) -> Self {
        let newer = post.id > self.top_id.unwrap_or(0);
        Self {
            creator_id: self.creator_id.clone(),
            next_max_id: Some(post.id),
            next_max_date: Some(post.published_datetime.clone()),
            top_id: if newer { Some(post.id) } else { self.top_id },
            top_date: if newer {
                Some(post.published_datetime.clone())
            } else {
                self.top_date.clone()
            },
            touch_bottom: self.touch_bottom || bottom,
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
struct Posts {
    #[serde(default)]
    items: Vec<Post>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Post {
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

impl FanboxIntegration {
    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.headers.iter().fold(
            self.client.get(format!("{}{}", self.base, path)),
            |request, (name, value)| request.header(name, value),
        )
    }

    async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<T, ProcessingError> {
        http::execute(&self.client, request, operation)
            .await?
            .json::<ApiResponse<T>>()
            .await
            .map(|response| response.body)
            .map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Invalid Fanbox response: {error}"
                ))
            })
    }

    async fn creator_posts(
        &self,
        creator: &str,
        pointer: &CreatorPointer,
    ) -> Result<Vec<Post>, ProcessingError> {
        let mut request = self
            .request("/post.listCreator")
            .query(&[("creatorId", creator), ("limit", "25")]);
        if let Some(id) = pointer.next_max_id.filter(|_| !pointer.touch_bottom) {
            request = request.query(&[("firstId", id.to_string())]);
        }
        if let Some(date) =
            pointer.next_max_date.as_deref().filter(|_| !pointer.touch_bottom)
        {
            request = request.query(&[("firstPublishedDatetime", date)]);
        }
        self.json(request, "Fetch Fanbox creator posts").await
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
        Ok(SourceItem {
            title: post.title.clone(),
            link: uri.clone(),
            datetime,
            content_type: "fanbox".into(),
            download_uri: uri,
            attrs,
            tags: post.tags.clone(),
            identity: None,
        })
    }
}

#[async_trait]
impl Source for FanboxIntegration {
    async fn fetch<'pointer>(
        &self,
        pointer: &'pointer dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        if self.latest_only {
            let posts: Posts = self
                .json(
                    self.request("/post.listSupporting").query(&[("limit", "30")]),
                    "Fetch Fanbox supporting posts",
                )
                .await?;
            return posts
                .items
                .into_iter()
                .filter(|post| !post.is_restricted)
                .take(limit as usize)
                .map(|post| {
                    Ok(PointedItem {
                        source_item: self.source_item(&post)?,
                        item_pointer: Arc::new(CreatorPointer::new(post.creator_id)),
                    })
                })
                .collect();
        }
        let pointer =
            pointer.as_any().downcast_ref::<FanboxPointer>().ok_or_else(|| {
                ProcessingError::non_retryable("Invalid Fanbox source pointer")
            })?;
        let supportings: Vec<Supporting> = self
            .json(self.request("/plan.listSupporting"), "Fetch Fanbox supportings")
            .await?;
        let mut output = Vec::new();
        for supporting in supportings {
            let state =
                pointer.creators.get(&supporting.creator_id).cloned().unwrap_or_else(
                    || CreatorPointer::new(supporting.creator_id.clone()),
                );
            let posts = self.creator_posts(&supporting.creator_id, &state).await?;
            let bottom = posts.len() < 25;
            for post in posts.into_iter().filter(|post| !post.is_restricted) {
                if state.touch_bottom && post.id <= state.top_id.unwrap_or(0) {
                    continue;
                }
                let next = state.update(&post, bottom);
                output.push(PointedItem {
                    source_item: self.source_item(&post)?,
                    item_pointer: Arc::new(next),
                });
                if output.len() >= limit as usize {
                    return Ok(output);
                }
            }
        }
        Ok(output)
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(FanboxPointer::default())
    }

    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<FanboxPointer>(value).unwrap_or_default())
    }

    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        Some(self.headers.clone())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PostDetail {
    id: String,
    cover_image_url: Option<String>,
    body: Media,
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
        let post: PostDetail = self
            .json(
                self.request("/post.info").query(&[("postId", post_id)]),
                "Fetch Fanbox post",
            )
            .await?;
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
        for block in &post.body.blocks {
            match block.kind.as_str() {
                "image" => {
                    if let Some(image) =
                        block.image_id.as_ref().and_then(|id| post.body.image_map.get(id))
                    {
                        ordered_images.push(image.clone());
                    }
                }
                "file" => {
                    if let Some(file) =
                        block.file_id.as_ref().and_then(|id| post.body.file_map.get(id))
                    {
                        ordered_files.push(file.clone());
                    }
                }
                "url_embed" => {
                    if let Some(embed) = block
                        .url_embed_id
                        .as_ref()
                        .and_then(|id| post.body.url_embed_map.get(id))
                    {
                        embeds.push(embed);
                    }
                }
                _ => {
                    if let Some(value) = &block.text {
                        text.push(value.as_str());
                    }
                }
            }
        }
        ordered_images.extend(post.body.images);
        for image in ordered_images {
            files.push(remote_file(
                format!("{}.{}", image.id, image.extension),
                image.original_url,
                Map::from_iter([
                    ("height".into(), Value::from(image.height)),
                    ("width".into(), Value::from(image.width)),
                    ("type".into(), Value::String("image".into())),
                ]),
            )?);
        }
        ordered_files.extend(post.body.files);
        for file in ordered_files {
            files.push(remote_file(
                format!("{}.{}", file.name, file.extension),
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
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn props(server: &MockServer) -> Map<String, Value> {
        Map::from_iter([
            ("cookie".into(), Value::String("FANBOXSESSID=x".into())),
            ("base-url".into(), Value::String(server.uri())),
        ])
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
        let source = SUPPLIER.apply(&props(&server)).unwrap().as_source().unwrap();
        let mut pointer = source.default_pointer();
        let first = source.fetch(pointer.as_ref(), 10).await.unwrap();
        assert_eq!(1, first.len());
        pointer.update(&first[0].source_item, first[0].item_pointer.as_ref());
        let second = source.fetch(pointer.as_ref(), 10).await.unwrap();
        assert!(second.is_empty());
    }

    #[tokio::test]
    async fn resolves_images_files_text_and_embeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/post.info"))
            .and(query_param("postId", "10"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "body":{"id":"10","coverImageUrl":"https://cdn/cover","body":{
                    "blocks":[{"type":"text","text":"hello"},{"type":"image","imageId":"i"},{"type":"url_embed","urlEmbedId":"u"}],
                    "imageMap":{"i":{"id":"i","extension":"png","height":2,"width":3,"originalUrl":"https://cdn/i","thumbnailUrl":"https://cdn/t"}},
                    "urlEmbedMap":{"u":{"id":"u","html":"<b>x</b>"}}
                }}
            })))
            .mount(&server)
            .await;
        let component = SUPPLIER.apply(&props(&server)).unwrap();
        let resolver = component.as_item_file_resolver().unwrap();
        let item = SourceItem {
            title: "Post".into(),
            link: format!("{}/posts/10", server.uri()).parse().unwrap(),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: "fanbox".into(),
            download_uri: format!("{}/posts/10", server.uri()).parse().unwrap(),
            attrs: Map::new(),
            tags: Vec::new(),
            identity: None,
        };
        let files = resolver.resolve_files(&item).await.unwrap();
        assert_eq!(4, files.len());
        assert!(files.iter().any(|file| file.path == *"i.png"));
        assert!(files.iter().any(|file| file.path == *"text_10.txt"));
    }
}

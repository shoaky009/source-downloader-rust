use crate::http;
use regex::Regex;
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
use std::sync::{Arc, LazyLock};
static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)_").unwrap());
pub struct PixivIntegrationSupplier;
pub const SUPPLIER: PixivIntegrationSupplier = PixivIntegrationSupplier;
impl ComponentSupplier for PixivIntegrationSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::source("pixiv".into()),
            ComponentType::file_resolver("pixiv".into()),
        ]
    }
    fn apply(
        &self,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let sid = p.get("session-id").and_then(Value::as_str).ok_or_else(|| {
            ComponentError::new("Missing or invalid 'session-id' property")
        })?;
        let user = p
            .get("user-id")
            .and_then(Value::as_i64)
            .or_else(|| USER.captures(sid)?.get(1)?.as_str().parse().ok())
            .ok_or_else(|| {
                ComponentError::new(
                    "session-id is invalid because user-id is not provided",
                )
            })?;
        let mode = p.get("mode").and_then(Value::as_str).unwrap_or("bookmark");
        if !matches!(mode, "bookmark" | "following") {
            return Err(ComponentError::new("Invalid 'mode' property"));
        }
        let base = p
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://www.pixiv.net")
            .trim_end_matches('/')
            .to_string();
        let client = if base.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|e| ComponentError::new(e.to_string()))?
        } else {
            http::build_client()?
        };
        let headers = HashMap::from([
            ("Cookie".into(), format!("PHPSESSID={sid}; ")),
            ("Referer".into(), "https://www.pixiv.net/".into()),
        ]);
        Ok(Arc::new(PixivIntegration {
            user,
            bookmark: mode == "bookmark",
            base,
            client,
            headers,
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
struct PixivIntegration {
    user: i64,
    bookmark: bool,
    base: String,
    client: reqwest::Client,
    headers: HashMap<String, String>,
}
impl Debug for PixivIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PixivIntegration")
            .field("user", &self.user)
            .field("bookmark", &self.bookmark)
            .finish()
    }
}
impl Display for PixivIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "pixiv")
    }
}
impl SdComponent for PixivIntegration {
    fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
        Ok(self)
    }
    fn as_item_file_resolver(
        self: Arc<Self>,
    ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
        Ok(self)
    }
}
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct PixivPointer {
    #[serde(default)]
    last_illustrations: HashMap<i64, i64>,
    #[serde(default = "zero")]
    top_bookmark_id: String,
    current_bookmark_id: Option<String>,
    touch_bottom: bool,
}
fn zero() -> String {
    "0".into()
}
#[derive(Debug)]
struct IllustrationPointer {
    user: i64,
    id: i64,
}
impl ItemPointer for IllustrationPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[derive(Debug)]
struct BookmarkPointer {
    id: String,
    bottom: bool,
}
impl ItemPointer for BookmarkPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl SourcePointer for PixivPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn update(&mut self, _: &SourceItem, p: &dyn ItemPointer) {
        if let Some(p) = p.as_any().downcast_ref::<IllustrationPointer>() {
            self.last_illustrations.insert(p.user, p.id);
        }
        if let Some(p) = p.as_any().downcast_ref::<BookmarkPointer>() {
            if self.top_bookmark_id < p.id {
                self.top_bookmark_id = p.id.clone()
            }
            self.current_bookmark_id = Some(p.id.clone());
            self.touch_bottom |= p.bottom;
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[derive(Deserialize)]
struct Response<T> {
    body: T,
    error: bool,
    message: Option<String>,
}
#[derive(Deserialize)]
struct Bookmarks {
    #[serde(default)]
    works: Vec<Illustration>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Illustration {
    id: i64,
    title: String,
    illust_type: i64,
    #[serde(default)]
    tags: Vec<String>,
    user_id: i64,
    user_name: String,
    url: String,
    x_restrict: i64,
    create_date: String,
    bookmark_data: Option<Bookmark>,
    #[serde(default)]
    is_masked: bool,
}
#[derive(Deserialize)]
struct Bookmark {
    id: String,
}
#[derive(Deserialize)]
struct Page {
    urls: HashMap<String, String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ugoira {
    original_src: String,
}
impl PixivIntegration {
    fn req(&self, path: &str) -> reqwest::RequestBuilder {
        self.headers
            .iter()
            .fold(self.client.get(format!("{}{}", self.base, path)), |r, (k, v)| {
                r.header(k, v)
            })
    }
    async fn json<T: for<'de> Deserialize<'de>>(
        &self,
        r: reqwest::RequestBuilder,
        op: &str,
    ) -> Result<T, ProcessingError> {
        let v = http::execute(&self.client, r, op)
            .await?
            .json::<Response<T>>()
            .await
            .map_err(|e| {
                ProcessingError::non_retryable(format!("Invalid Pixiv response: {e}"))
            })?;
        if v.error {
            Err(ProcessingError::non_retryable(
                v.message.unwrap_or_else(|| "Pixiv API error".into()),
            ))
        } else {
            Ok(v.body)
        }
    }
    fn item(i: &Illustration) -> Result<SourceItem, ProcessingError> {
        let link = Uri::from_str(&format!("https://www.pixiv.net/artworks/{}", i.id))
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let download = Uri::from_str(&i.url)
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let datetime = OffsetDateTime::parse(
            &i.create_date,
            &time::format_description::well_known::Rfc3339,
        )
        .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        Ok(SourceItem {
            title: i.title.clone(),
            link,
            datetime,
            content_type: "illustration".into(),
            download_uri: download,
            attrs: Map::from_iter([
                ("userId".into(), Value::from(i.user_id)),
                ("username".into(), Value::String(i.user_name.clone())),
                ("illustrationId".into(), Value::from(i.id)),
                ("nsfw".into(), Value::Bool(i.x_restrict == 1)),
                ("illustrationType".into(), Value::from(i.illust_type)),
            ]),
            tags: i.tags.clone(),
            identity: None,
        })
    }
}
#[async_trait]
impl Source for PixivIntegration {
    async fn fetch<'p>(
        &self,
        p: &'p dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let p = p.as_any().downcast_ref::<PixivPointer>().ok_or_else(|| {
            ProcessingError::non_retryable("Invalid Pixiv source pointer")
        })?;
        if !self.bookmark {
            return self.fetch_following(p, limit).await;
        }
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let body: Bookmarks = self
                .json(
                    self.req(&format!("/ajax/user/{}/illusts/bookmarks", self.user))
                        .query(&[
                            ("tag", ""),
                            ("offset", &offset.to_string()),
                            ("limit", "50"),
                            ("rest", "show"),
                            ("lang", "zh"),
                        ]),
                    "Fetch Pixiv bookmarks",
                )
                .await?;
            let bottom = body.works.len() < 50;
            if body.works.is_empty() {
                break;
            }
            for i in body.works.into_iter().filter(|i| !i.is_masked) {
                let Some(bookmark) = &i.bookmark_data else { continue };
                let include = if p.touch_bottom {
                    bookmark.id > p.top_bookmark_id
                } else {
                    p.current_bookmark_id.as_ref().is_none_or(|last| bookmark.id < *last)
                };
                if include {
                    out.push(PointedItem {
                        source_item: Self::item(&i)?,
                        item_pointer: Arc::new(BookmarkPointer {
                            id: bookmark.id.clone(),
                            bottom,
                        }),
                    });
                    if out.len() >= limit as usize {
                        return Ok(out);
                    }
                }
            }
            if bottom || p.touch_bottom && !out.is_empty() {
                break;
            }
            offset += 50;
        }
        Ok(out)
    }
    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(PixivPointer::default())
    }
    fn parse_raw_pointer(&self, v: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<PixivPointer>(v).unwrap_or_default())
    }
    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        Some(self.headers.clone())
    }
}
impl PixivIntegration {
    async fn fetch_following(
        &self,
        p: &PixivPointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let value: Value = self
            .json(
                self.req(&format!("/ajax/user/{}/following", self.user)).query(&[
                    ("offset", "0"),
                    ("limit", "50"),
                    ("rest", "show"),
                ]),
                "Fetch Pixiv followings",
            )
            .await?;
        let users =
            value.get("users").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut out = Vec::new();
        for user in users {
            let uid = user
                .get("userId")
                .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                .unwrap_or(0);
            let illustrations = user
                .get("illusts")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for raw in illustrations {
                let i: Illustration = serde_json::from_value(raw)
                    .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
                if i.id > p.last_illustrations.get(&uid).copied().unwrap_or(0) {
                    out.push(PointedItem {
                        source_item: Self::item(&i)?,
                        item_pointer: Arc::new(IllustrationPointer {
                            user: uid,
                            id: i.id,
                        }),
                    });
                    if out.len() >= limit as usize {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }
}
#[async_trait]
impl ItemFileResolver for PixivIntegration {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let id = item.attrs.get("illustrationId").and_then(Value::as_i64).ok_or_else(
            || ProcessingError::non_retryable("Pixiv illustrationId missing"),
        )?;
        let kind =
            item.attrs.get("illustrationType").and_then(Value::as_i64).ok_or_else(
                || ProcessingError::non_retryable("Pixiv illustrationType missing"),
            )?;
        if kind != 2 {
            let pages: Vec<Page> = self
                .json(self.req(&format!("/ajax/illust/{id}/pages")), "Fetch Pixiv pages")
                .await?;
            return pages
                .into_iter()
                .filter_map(|p| p.urls.get("original").cloned())
                .map(remote)
                .collect();
        }
        let meta: Ugoira = self
            .json(
                self.req(&format!("/ajax/illust/{id}/ugoira_meta")),
                "Fetch Pixiv ugoira metadata",
            )
            .await?;
        let request = self
            .headers
            .iter()
            .fold(self.client.get(&meta.original_src), |r, (k, v)| r.header(k, v));
        let response = http::execute(&self.client, request, "Fetch Pixiv ugoira").await?;
        let size = response.content_length();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| http::map_error(e, "Read Pixiv ugoira"))?;
        let mut attrs = Map::new();
        if let Some(size) = size {
            attrs.insert("size".into(), Value::from(size));
        }
        let name = reqwest::Url::parse(&meta.original_src)
            .ok()
            .and_then(|u| u.path_segments()?.next_back().map(str::to_string))
            .unwrap_or_else(|| format!("{id}.zip"));
        Ok(vec![SourceFile {
            path: PathBuf::from(name),
            attrs,
            download_uri: None,
            tags: vec![],
            data: Some(Arc::from(bytes.as_ref())),
        }])
    }
}
fn remote(url: String) -> Result<SourceFile, ProcessingError> {
    let uri =
        Uri::from_str(&url).map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
    let name = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.path_segments()?.next_back().map(str::to_string))
        .ok_or_else(|| ProcessingError::non_retryable("Pixiv image filename missing"))?;
    Ok(SourceFile {
        path: PathBuf::from(name),
        attrs: Map::new(),
        download_uri: Some(uri),
        tags: vec![],
        data: None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn derives_user_from_session() {
        let p =
            Map::from_iter([("session-id".into(), Value::String("12345_token".into()))]);
        assert!(SUPPLIER.apply(&p).is_ok());
    }
    #[test]
    fn rejects_invalid_session() {
        let p = Map::from_iter([("session-id".into(), Value::String("bad".into()))]);
        assert!(SUPPLIER.apply(&p).is_err());
    }
}

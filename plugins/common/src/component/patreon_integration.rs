use crate::http;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, ItemPointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source, SourceFile,
    SourcePointer,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value};
use source_downloader_sdk::time::OffsetDateTime;
use source_downloader_sdk::{SdComponent, SourceItem};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

pub struct PatreonIntegrationSupplier;
pub const SUPPLIER: PatreonIntegrationSupplier = PatreonIntegrationSupplier;
impl ComponentSupplier for PatreonIntegrationSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::source("patreon".into()),
            ComponentType::file_resolver("patreon".into()),
        ]
    }
    fn apply(
        &self,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let sid = p.get("session-id").and_then(Value::as_str).ok_or_else(|| {
            ComponentError::new("Missing or invalid 'session-id' property")
        })?;
        let mut headers = p
            .get("headers")
            .map(|v| {
                serde_json::from_value::<HashMap<String, String>>(v.clone())
                    .map_err(|e| ComponentError::new(format!("Invalid 'headers': {e}")))
            })
            .transpose()?
            .unwrap_or_default();
        headers.entry("Cookie".into()).or_insert(format!("session_id={sid}; patreon_location_country_code=CN; patreon_locale_code=zh-CN;"));
        let base = p
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://www.patreon.com")
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
        Ok(Arc::new(PatreonIntegration { client, base, headers }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
struct PatreonIntegration {
    client: reqwest::Client,
    base: String,
    headers: HashMap<String, String>,
}
impl Debug for PatreonIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PatreonIntegration").field("base", &self.base).finish()
    }
}
impl Display for PatreonIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "patreon")
    }
}
impl SdComponent for PatreonIntegration {
    fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
        Ok(self)
    }
    fn as_item_file_resolver(
        self: Arc<Self>,
    ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
        Ok(self)
    }
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PatreonPointer {
    #[serde(default)]
    campaigns: HashMap<i64, CampaignPointer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CampaignPointer {
    campaign_id: i64,
    last_year_month: Option<String>,
    last_of_month: bool,
    last_post_id: i64,
}
impl ItemPointer for CampaignPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl SourcePointer for PatreonPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn update(&mut self, _: &SourceItem, p: &dyn ItemPointer) {
        if let Some(p) = p.as_any().downcast_ref::<CampaignPointer>() {
            self.campaigns.insert(p.campaign_id, p.clone());
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl PatreonIntegration {
    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.headers
            .iter()
            .fold(self.client.get(format!("{}{}", self.base, path)), |r, (k, v)| {
                r.header(k, v)
            })
    }
    async fn value(
        &self,
        r: reqwest::RequestBuilder,
        op: &str,
    ) -> Result<Value, ProcessingError> {
        http::execute(&self.client, r, op).await?.json().await.map_err(|e| {
            ProcessingError::non_retryable(format!("Invalid Patreon response: {e}"))
        })
    }
    fn campaign_ids(v: &Value) -> Vec<i64> {
        v.get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|p| p.pointer("/relationships/campaign/data/id"))
            .filter_map(|id| id.as_i64().or_else(|| id.as_str()?.parse().ok()))
            .collect()
    }
    fn months(v: &Value) -> Vec<String> {
        let mut m = v
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|x| {
                x.pointer("/attributes/tag_type").and_then(Value::as_str)
                    == Some("year_month")
            })
            .filter_map(|x| {
                x.pointer("/attributes/value").and_then(Value::as_str).map(str::to_string)
            })
            .collect::<Vec<_>>();
        m.sort();
        m
    }
    fn item(
        campaign: i64,
        post: &Value,
        user: Option<&str>,
        month: &str,
        last: bool,
    ) -> Result<PointedItem, ProcessingError> {
        let id = post
            .get("id")
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
            .ok_or_else(|| ProcessingError::non_retryable("Patreon post id missing"))?;
        let a = post.get("attributes").ok_or_else(|| {
            ProcessingError::non_retryable("Patreon post attributes missing")
        })?;
        let title = a.get("title").and_then(Value::as_str).unwrap_or("").to_string();
        let url = a
            .get("url")
            .or_else(|| a.get("patreon_url"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
            ProcessingError::non_retryable("Patreon post URL missing")
        })?;
        let uri = Uri::from_str(url)
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let date = a
            .get("published_at")
            .and_then(Value::as_str)
            .ok_or_else(|| ProcessingError::non_retryable("Patreon post date missing"))?;
        let datetime =
            OffsetDateTime::parse(date, &time::format_description::well_known::Rfc3339)
                .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let mut attrs = Map::from_iter([
            ("campaignId".into(), Value::from(campaign)),
            ("postId".into(), Value::from(id)),
        ]);
        if let Some(u) = user {
            attrs.insert("username".into(), Value::String(u.into()));
        }
        Ok(PointedItem {
            source_item: SourceItem {
                title,
                link: uri.clone(),
                datetime,
                content_type: a
                    .get("post_type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
                download_uri: uri,
                attrs,
                tags: vec![],
                identity: None,
            },
            item_pointer: Arc::new(CampaignPointer {
                campaign_id: campaign,
                last_year_month: Some(month.into()),
                last_of_month: last,
                last_post_id: id,
            }),
        })
    }
}
#[async_trait]
impl Source for PatreonIntegration {
    async fn fetch<'p>(
        &self,
        p: &'p dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let p = p.as_any().downcast_ref::<PatreonPointer>().ok_or_else(|| {
            ProcessingError::non_retryable("Invalid Patreon source pointer")
        })?;
        let pledges =
            self.value(self.request("/api/pledges"), "Fetch Patreon pledges").await?;
        let mut out = Vec::new();
        for campaign in Self::campaign_ids(&pledges) {
            let tags = self
                .value(
                    self.request(&format!("/api/campaigns/{campaign}/post-tags")),
                    "Fetch Patreon post tags",
                )
                .await?;
            let months = Self::months(&tags);
            let state = p.campaigns.get(&campaign);
            let start = state
                .and_then(|s| s.last_year_month.as_ref())
                .and_then(|last| {
                    months.iter().position(|m| m == last).map(|i| {
                        if state.is_some_and(|s| s.last_of_month) { i + 1 } else { i }
                    })
                })
                .unwrap_or(0);
            for month in months.into_iter().skip(start) {
                let response = self
                    .value(
                        self.request("/api/posts").query(&[
                            ("filter[campaign_id]", campaign.to_string()),
                            ("filter[month]", month.clone()),
                            ("sort", "published_at".into()),
                        ]),
                        "Fetch Patreon posts",
                    )
                    .await?;
                let posts = response
                    .get("data")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let user = response
                    .get("included")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .find(|x| x.get("type").and_then(Value::as_str) == Some("user"))
                    .and_then(|x| x.pointer("/attributes/full_name"))
                    .and_then(Value::as_str);
                let last_id = state.map(|s| s.last_post_id).unwrap_or(-1);
                let selected = posts
                    .iter()
                    .filter(|x| {
                        x.get("id")
                            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                            .unwrap_or(-1)
                            > last_id
                    })
                    .collect::<Vec<_>>();
                for (index, post) in selected.iter().enumerate() {
                    out.push(Self::item(
                        campaign,
                        post,
                        user,
                        &month,
                        index + 1 == selected.len(),
                    )?);
                    if out.len() >= limit as usize {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }
    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(PatreonPointer::default())
    }
    fn parse_raw_pointer(&self, v: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<PatreonPointer>(v).unwrap_or_default())
    }
    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        Some(self.headers.clone())
    }
}
#[async_trait]
impl ItemFileResolver for PatreonIntegration {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let id = item
            .link
            .path()
            .rsplit('/')
            .next()
            .filter(|x| x.chars().all(|c| c.is_ascii_digit()))
            .ok_or_else(|| ProcessingError::non_retryable("Patreon post ID missing"))?;
        let response = self
            .value(self.request(&format!("/api/posts/{id}")), "Fetch Patreon post")
            .await?;
        let relationships = response
            .pointer("/data/relationships/media/data")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let included = response
            .get("included")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut files = Vec::new();
        for rel in relationships {
            let mid = rel.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(media) = included.iter().find(|x| {
                x.get("type").and_then(Value::as_str) == Some("media")
                    && x.get("id").and_then(Value::as_str) == Some(mid)
            }) {
                let a = &media["attributes"];
                let url = a.get("download_url").and_then(Value::as_str).or_else(|| {
                    a.pointer("/image_urls/original").and_then(Value::as_str)
                });
                if let Some(url) = url {
                    let name =
                        a.get("file_name").and_then(Value::as_str).unwrap_or("file");
                    let mut attrs = Map::from_iter([
                        ("mediaId".into(), Value::String(mid.into())),
                        ("filename".into(), Value::String(name.into())),
                    ]);
                    for key in ["mimetype", "media_type", "size_bytes"] {
                        if let Some(v) = a.get(key) {
                            attrs.insert(key.into(), v.clone());
                        }
                    }
                    files.push(SourceFile {
                        path: PathBuf::from(format!("{mid}_{name}")),
                        attrs,
                        download_uri: Some(Uri::from_str(url).map_err(|e| {
                            ProcessingError::non_retryable(e.to_string())
                        })?),
                        tags: vec![],
                        data: None,
                    });
                }
            }
        }
        if let Some(content) = response
            .pointer("/data/attributes/content")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        {
            files.push(SourceFile {
                path: PathBuf::from(format!("{id}_content.html")),
                attrs: Map::from_iter([(
                    "mimetype".into(),
                    Value::String("text".into()),
                )]),
                download_uri: None,
                tags: vec![],
                data: Some(Arc::from(content.as_bytes())),
            });
        }
        Ok(files)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pointer_updates_campaign_independently() {
        let mut p = PatreonPointer::default();
        let item = SourceItem {
            title: String::new(),
            link: Uri::from_static("https://e"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: String::new(),
            download_uri: Uri::from_static("https://e"),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        };
        p.update(
            &item,
            &CampaignPointer {
                campaign_id: 1,
                last_year_month: Some("2026-01".into()),
                last_of_month: true,
                last_post_id: 2,
            },
        );
        assert_eq!(2, p.campaigns[&1].last_post_id);
    }
    #[test]
    fn validates_session() {
        assert!(SUPPLIER.apply(&Map::new()).is_err());
    }
}

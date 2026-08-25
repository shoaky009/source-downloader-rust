use crate::http;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemPointer, PointedItem,
    ProcessingError, SdComponent, SdComponentMetadata, Source, SourcePointer,
    format_error_chain,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;
pub struct BilibiliSourceSupplier;
pub const SUPPLIER: BilibiliSourceSupplier = BilibiliSourceSupplier;
impl ComponentSupplier for BilibiliSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("bilibili".into())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let favorites = p.get("favorites").ok_or_else(|| {
            ComponentError::new(
                "Invalid configuration at 'favorites': missing field `favorites`",
            )
        })?;
        let favorites =
            serde_json::from_value::<Vec<i64>>(favorites.clone()).map_err(|error| {
                ComponentError::new(format!(
                    "Invalid configuration at 'favorites': {error}"
                ))
            })?;
        let cookie = p.get("cookie").and_then(Value::as_str).map(str::to_string);
        let base = p
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://api.bilibili.com")
            .trim_end_matches('/')
            .to_string();
        let client = http::build_client()?;
        Ok(Arc::new(BilibiliSource { favorites, cookie, base, client }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Provides Bilibili favorites as a source.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "favorites":{
                        "type":"array",
                        "items":{"type":"integer"},
                        "minItems":1
                    },
                    "cookie":{"type":"string"},
                    "base-url":{"type":"string","default":"https://api.bilibili.com"}
                },
                "required":["favorites"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            #[rustfmt::skip]
            source_pointer_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "favorites":{
                        "type":"object",
                        "additionalProperties":{
                            "type":"object",
                            "properties":{
                                "favorite_id":{"type":"integer"},
                                "min_fav_time":{"type":"integer"},
                                "max_fav_time":{"type":"integer"},
                                "touch_bottom":{"type":"boolean"}
                            },
                            "required":["favorite_id","min_fav_time","max_fav_time","touch_bottom"]
                        }
                    }
                }
            })),
        }))
    }
}
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Source)]
struct BilibiliSource {
    favorites: Vec<i64>,
    cookie: Option<String>,
    base: String,
    client: reqwest::Client,
}

impl Display for BilibiliSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "bilibili")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BilibiliPointer {
    #[serde(default)]
    favorites: HashMap<i64, FavoritePointer>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FavoritePointer {
    favorite_id: i64,
    min_fav_time: i64,
    max_fav_time: i64,
    touch_bottom: bool,
}
#[derive(Debug)]
struct MediaPointer {
    favorite_id: i64,
    time: i64,
    touch_bottom: bool,
}
impl ItemPointer for MediaPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl SourcePointer for BilibiliPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }
    fn update(&mut self, _: &SourceItem, p: &dyn ItemPointer) {
        if let Some(p) = p.as_any().downcast_ref::<MediaPointer>() {
            self.favorites
                .entry(p.favorite_id)
                .and_modify(|v| {
                    v.min_fav_time = v.min_fav_time.min(p.time);
                    v.max_fav_time = v.max_fav_time.max(p.time);
                    v.touch_bottom |= p.touch_bottom
                })
                .or_insert(FavoritePointer {
                    favorite_id: p.favorite_id,
                    min_fav_time: p.time,
                    max_fav_time: p.time,
                    touch_bottom: p.touch_bottom,
                });
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[derive(Deserialize)]
struct Response {
    code: i64,
    message: Option<String>,
    data: Option<Data>,
}
#[derive(Deserialize)]
struct Data {
    #[serde(default)]
    medias: Vec<Media>,
    has_more: bool,
}
#[derive(Deserialize)]
struct Media {
    #[serde(rename = "type")]
    kind: i64,
    title: String,
    page: i64,
    duration: i64,
    upper: Upper,
    attr: i64,
    link: String,
    pubtime: i64,
    fav_time: i64,
    bv_id: String,
}
#[derive(Deserialize)]
struct Upper {
    name: String,
}
impl BilibiliSource {
    async fn page(&self, id: i64, pn: u32) -> Result<Data, ProcessingError> {
        let mut req =
            self.client.get(format!("{}/x/v3/fav/resource/list", self.base)).query(&[
                ("media_id", id.to_string()),
                ("pn", pn.to_string()),
                ("ps", "20".into()),
                ("type", "0".into()),
                ("order", "mtime".into()),
            ]);
        if let Some(c) = &self.cookie {
            req = req.header("Cookie", c)
        }
        let r = http::execute(&self.client, req, "Fetch Bilibili favorites").await?;
        let body = r.json::<Response>().await.map_err(|error| {
            ProcessingError::non_retryable(format!(
                "Invalid Bilibili response: {}",
                format_error_chain(&error)
            ))
        })?;
        if body.code != 0 {
            return Err(ProcessingError::non_retryable(format!(
                "Bilibili API error {}: {}",
                body.code,
                body.message.unwrap_or_default()
            )));
        }
        body.data.ok_or_else(|| {
            ProcessingError::non_retryable("Bilibili response data is missing")
        })
    }
    fn convert(id: i64, m: Media, touch: bool) -> Result<PointedItem, ProcessingError> {
        let link = Uri::from_str(&m.link).map_err(|e| {
            ProcessingError::non_retryable(format!("Invalid Bilibili link: {e}"))
        })?;
        let download =
            Uri::from_str(&format!("https://www.bilibili.com/video/{}", m.bv_id))
                .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let datetime = OffsetDateTime::from_unix_timestamp(m.pubtime)
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let attrs = Map::from_iter([
            ("upper".into(), Value::String(m.upper.name)),
            ("type".into(), Value::from(m.kind)),
            ("page".into(), Value::from(m.page)),
            ("bv".into(), Value::String(m.bv_id)),
            ("duration".into(), Value::from(m.duration)),
        ]);
        Ok(PointedItem {
            source_item: SourceItem {
                title: m.title,
                link,
                datetime,
                content_type: "video".into(),
                download_uri: download,
                attrs,
                tags: vec![],
                identity: None,
            },
            item_pointer: Arc::new(MediaPointer {
                favorite_id: id,
                time: m.fav_time,
                touch_bottom: touch,
            }),
        })
    }
}
#[async_trait]
impl Source for BilibiliSource {
    async fn fetch<'p>(
        &self,
        p: &'p dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let p = p.as_any().downcast_ref::<BilibiliPointer>().ok_or_else(|| {
            ProcessingError::non_retryable("Invalid Bilibili source pointer")
        })?;
        let mut out = Vec::new();
        for id in &self.favorites {
            let state = p.favorites.get(id);
            let mut pn = 1;
            loop {
                if out.len() >= limit as usize {
                    return Ok(out);
                }
                let data = self.page(*id, pn).await?;
                let has_more = data.has_more;
                let touched = state.is_some_and(|s| s.touch_bottom);
                let min = state.map(|s| s.min_fav_time).unwrap_or(i64::MAX);
                let max = state.map(|s| s.max_fav_time).unwrap_or(i64::MIN);
                let filtered = data
                    .medias
                    .into_iter()
                    .filter(
                        |m| if touched { m.fav_time > max } else { m.fav_time <= min },
                    )
                    .collect::<Vec<_>>();
                let last_time = filtered.last().map(|m| m.fav_time);
                for m in filtered {
                    if m.attr == 0 {
                        let touch_bottom = !has_more && last_time == Some(m.fav_time);
                        out.push(Self::convert(*id, m, touch_bottom)?);
                        if out.len() >= limit as usize {
                            return Ok(out);
                        }
                    }
                }
                if !has_more || (touched && out.is_empty()) {
                    break;
                }
                pn += 1;
            }
        }
        Ok(out)
    }
    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(BilibiliPointer::default())
    }
    fn parse_raw_pointer(&self, v: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<BilibiliPointer>(v).unwrap_or_default())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    #[tokio::test]
    async fn fetches_filters_updates_and_resumes() {
        let s = MockServer::start().await;
        Mock::given(method("GET")).and(path("/x/v3/fav/resource/list")).and(query_param("media_id","7")).and(query_param("pn","1")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"code":0,"data":{"has_more":false,"medias":[{"type":2,"title":"A","page":1,"duration":10,"upper":{"name":"U"},"attr":0,"link":"https://b/a","pubtime":100,"fav_time":20,"bv_id":"BV1"}]}}))).expect(2).mount(&s).await;
        let props = Map::from_iter([
            ("favorites".into(), serde_json::json!([7])),
            ("base-url".into(), Value::String(s.uri())),
        ]);
        let source = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap()
            .as_source()
            .unwrap();
        let mut pointer = source.default_pointer();
        let first = source.fetch(pointer.as_ref(), 10).await.unwrap();
        assert_eq!(1, first.len());
        pointer.update(&first[0].source_item, first[0].item_pointer.as_ref());
        let second = source.fetch(pointer.as_ref(), 10).await.unwrap();
        assert!(second.is_empty());
    }
    #[test]
    fn pointer_tracks_bounds() {
        let mut p = BilibiliPointer::default();
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
        p.update(&item, &MediaPointer { favorite_id: 1, time: 3, touch_bottom: false });
        p.update(&item, &MediaPointer { favorite_id: 1, time: 8, touch_bottom: true });
        assert_eq!(3, p.favorites[&1].min_fav_time);
        assert!(p.favorites[&1].touch_bottom);
    }
}

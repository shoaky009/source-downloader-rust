use crate::api::pixiv::{Illustration, PixivClient, PixivUser};
use crate::http::HttpClient;
use regex::Regex;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, ItemPointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source, SourceFile,
    SourceItemStream, SourcePointer, deserialize_component_config, source_item_stream,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::Any;
use std::cmp::Ordering;
use std::collections::{HashMap, VecDeque};
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
static USER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d+)_").unwrap());
#[derive(Deserialize)]
enum PixivMode {
    #[serde(rename = "bookmark")]
    Bookmark,
    #[serde(rename = "following")]
    Following,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct PixivIntegrationConfig {
    session_id: String,
    user_id: Option<i64>,
    mode: Option<PixivMode>,
    base_url: Option<String>,
}
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
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<PixivIntegrationConfig>(p)?;
        let sid = config.session_id;
        let user = config
            .user_id
            .or_else(|| USER.captures(&sid)?.get(1)?.as_str().parse().ok())
            .ok_or_else(|| {
                ComponentError::new(
                    "Invalid configuration at 'session-id': \
                     session-id is invalid because user-id is not provided",
                )
            })?;
        let bookmark =
            matches!(config.mode.unwrap_or(PixivMode::Bookmark), PixivMode::Bookmark);
        let base = config
            .base_url
            .unwrap_or_else(|| "https://www.pixiv.net".to_string())
            .trim_end_matches('/')
            .to_string();
        let http = HttpClient::new()?;
        Ok(Arc::new(PixivIntegration {
            user,
            bookmark,
            client: PixivClient::new(http, base, &sid)?,
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Provides Pixiv bookmarks or followed-user illustrations and resolves media files.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "session-id":{"type":"string"},
                    "user-id":{"type":"integer"},
                    "mode":{
                        "type":"string",
                        "enum":["bookmark","following"],
                        "default":"bookmark"
                    },
                    "base-url":{"type":"string","default":"https://www.pixiv.net"}
                },
                "required":["session-id"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            #[rustfmt::skip]
            source_pointer_json_schema: Some(source_downloader_sdk::serde_json::json!({
                "type": "object",
                "title": "数据源游标",
                "properties": {
                    "lastIllustrationRecord": {
                        "type": "object",
                        "title": "作者作品",
                        "propertyNames": {"title": "作者 ID"},
                        "additionalProperties": {"type": "integer", "title": "作品 ID"},
                        "default": {}
                    },
                    "topBookmarkId": {"type": "string", "title": "最新收藏 ID", "default": "0"},
                    "currentBookmarkId": {"type": "string", "title": "当前收藏 ID"},
                    "touchBottom": {"type": "boolean", "title": "已到底", "default": false}
                },
                "additionalProperties": false
            })),
            output_schema: None,
        }))
    }
}
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Source, ItemFileResolver)]
struct PixivIntegration {
    user: i64,
    bookmark: bool,
    client: PixivClient,
}

impl Display for PixivIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "pixiv")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct PixivPointer {
    #[serde(alias = "last_illustrations")]
    last_illustration_record: HashMap<i64, i64>,
    #[serde(alias = "top_bookmark_id")]
    top_bookmark_id: String,
    #[serde(alias = "current_bookmark_id", skip_serializing_if = "Option::is_none")]
    current_bookmark_id: Option<String>,
    #[serde(alias = "touch_bottom")]
    touch_bottom: bool,
}
impl Default for PixivPointer {
    fn default() -> Self {
        Self {
            last_illustration_record: HashMap::new(),
            top_bookmark_id: "0".into(),
            current_bookmark_id: None,
            touch_bottom: false,
        }
    }
}

fn compare_bookmark_ids(left: &str, right: &str) -> Ordering {
    // Decimal IDs remain strings on the wire; lexical order alone fails at 9 -> 10.
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
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
            let last = self.last_illustration_record.entry(p.user).or_default();
            *last = (*last).max(p.id);
        }
        if let Some(p) = p.as_any().downcast_ref::<BookmarkPointer>() {
            if compare_bookmark_ids(&self.top_bookmark_id, &p.id).is_lt() {
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
impl PixivIntegration {
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
    async fn fetch(
        &self,
        p: &dyn SourcePointer,
        limit: u32,
    ) -> Result<SourceItemStream, ProcessingError> {
        let p = p.as_any().downcast_ref::<PixivPointer>().ok_or_else(|| {
            ProcessingError::non_retryable("Invalid Pixiv source pointer")
        })?;
        if limit == 0 {
            return Ok(source_item_stream(Vec::new()));
        }
        let items = if !self.bookmark {
            self.fetch_following(p, limit).await?
        } else if p.touch_bottom {
            self.fetch_new_bookmarks(p, limit as usize).await?
        } else {
            self.fetch_bookmark_history(p, limit as usize).await?
        };
        Ok(source_item_stream(items))
    }
    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(PixivPointer::default())
    }
    fn parse_raw_pointer(&self, v: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<PixivPointer>(v).unwrap_or_default())
    }
    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        Some(self.client.headers())
    }
}
impl PixivIntegration {
    fn bookmark_item(
        illustration: Illustration,
        id: String,
        bottom: bool,
    ) -> Result<PointedItem, ProcessingError> {
        Ok(PointedItem {
            source_item: Self::item(&illustration)?,
            item_pointer: Arc::new(BookmarkPointer { id, bottom }),
        })
    }

    async fn fetch_new_bookmarks(
        &self,
        p: &PixivPointer,
        limit: usize,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let mut works = VecDeque::new();
        let mut offset = 0;
        loop {
            let body = self.client.bookmarks(self.user, offset, 50).await?;
            let mut finished = body.works.len() < 50;
            for mut illustration in body.works {
                let Some(bookmark) = illustration.bookmark_data.take() else {
                    continue;
                };
                if compare_bookmark_ids(&bookmark.id, &p.top_bookmark_id).is_le() {
                    finished = true;
                    continue;
                }
                if illustration.is_masked {
                    continue;
                }
                if works.len() == limit {
                    works.pop_front();
                }
                works.push_back((illustration, bookmark.id));
            }
            if finished {
                break;
            }
            offset += 50;
        }
        // The API returns newest first. Advance from the oldest unseen bookmark
        // so limits and interruptions cannot jump over unprocessed bookmarks.
        works
            .into_iter()
            .rev()
            .map(|(illustration, id)| Self::bookmark_item(illustration, id, true))
            .collect()
    }

    async fn fetch_bookmark_history(
        &self,
        p: &PixivPointer,
        limit: usize,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let mut works = Vec::new();
        let mut resume_work = None;
        let mut offset = 0;
        let complete = 'pages: loop {
            let body = self.client.bookmarks(self.user, offset, 50).await?;
            let bottom = body.works.len() < 50;
            for mut illustration in body.works.into_iter().filter(|i| !i.is_masked) {
                let Some(bookmark) = illustration.bookmark_data.take() else {
                    continue;
                };
                if p.current_bookmark_id
                    .as_ref()
                    .is_some_and(|last| compare_bookmark_ids(&bookmark.id, last).is_ge())
                {
                    if compare_bookmark_ids(&bookmark.id, &p.top_bookmark_id).is_le() {
                        resume_work = Some((illustration, bookmark.id));
                    }
                    continue;
                }
                // Look ahead one visible item, including across page boundaries,
                // before deciding whether the last returned item finishes history.
                if works.len() == limit {
                    break 'pages false;
                }
                works.push((illustration, bookmark.id));
            }
            if bottom {
                break true;
            }
            offset += 50;
        };
        if works.is_empty() {
            if let Some(work) = resume_work {
                // Persist completion through update if the remaining tail vanished.
                // Normal item deduplication handles the already processed work.
                works.push(work);
            } else {
                // All previously processed bookmarks may have been deleted.
                return self.fetch_new_bookmarks(p, limit).await;
            }
        }
        let count = works.len();
        works
            .into_iter()
            .enumerate()
            .map(|(index, (illustration, id))| {
                Self::bookmark_item(illustration, id, complete && index + 1 == count)
            })
            .collect()
    }

    async fn fetch_following(
        &self,
        p: &PixivPointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let mut out = Vec::new();
        let mut offset = 0;
        let mut updated_users = 0;
        loop {
            let body = self.client.following(self.user, offset, 50).await?;
            if body.users.is_empty() {
                break;
            }
            for user in body.users {
                let last =
                    p.last_illustration_record.get(&user.user_id).copied().unwrap_or(0);
                if !user.illusts.iter().any(|i| i.id > last) {
                    continue;
                }
                updated_users += 1;
                let uid = user.user_id;
                for i in self.following_illustrations(user, last).await? {
                    out.push(PointedItem {
                        source_item: Self::item(&i)?,
                        item_pointer: Arc::new(IllustrationPointer {
                            user: uid,
                            id: i.id,
                        }),
                    });
                }
                // Like Kotlin's ExpandIterator, finish the current author's works
                // before applying the soft fetch limit.
                if out.len() >= limit as usize || updated_users >= 50 {
                    return Ok(out);
                }
            }
            offset += 50;
        }
        Ok(out)
    }

    async fn following_illustrations(
        &self,
        user: PixivUser,
        last: i64,
    ) -> Result<Vec<Illustration>, ProcessingError> {
        let mut works = if user.illusts.iter().any(|i| i.id == last) {
            user.illusts
        } else {
            let mut ids = self.client.user_illustration_ids(user.user_id).await?;
            ids.retain(|id| *id > last);
            ids.sort_unstable();
            ids.dedup();
            let mut works = Vec::new();
            for chunk in ids.chunks(50) {
                works.extend(self.client.illustrations(user.user_id, chunk).await?);
            }
            works
        };
        works.retain(|i| !i.is_masked && i.id > last);
        works.sort_unstable_by_key(|i| i.id);
        Ok(works)
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
            return self
                .client
                .pages(id)
                .await?
                .into_iter()
                .filter_map(|page| page.urls.get("original").cloned())
                .map(remote)
                .collect();
        }
        let metadata = self.client.ugoira_metadata(id).await?;
        let file = self.client.download(&metadata.original_src).await?;
        let mut attrs = Map::new();
        if let Some(size) = file.size {
            attrs.insert("size".into(), Value::from(size));
        }
        Ok(vec![SourceFile {
            path: PathBuf::from(file.name),
            attrs,
            download_uri: None,
            tags: vec![],
            data: Some(Arc::from(file.bytes.as_ref())),
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
    use futures_util::TryStreamExt;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn source(server: &MockServer, bookmark: bool) -> PixivIntegration {
        PixivIntegration {
            user: 123,
            bookmark,
            client: PixivClient::new(
                HttpClient::from_reqwest(
                    crate::http::client_builder().unwrap().no_proxy().build().unwrap(),
                ),
                server.uri(),
                "123_token",
            )
            .unwrap(),
        }
    }

    fn illustration(id: i64, user: i64) -> Value {
        json!({
            "id": id.to_string(),
            "title": format!("work {id}"),
            "illustType": 0,
            "userId": user.to_string(),
            "userName": "artist",
            "url": format!("https://example.com/{id}.jpg"),
            "xRestrict": 0,
            "createDate": "2026-10-01T00:00:00+09:00",
            "bookmarkData": {"id": id.to_string()}
        })
    }

    async fn mock_bookmarks(server: &MockServer, works: Vec<Value>) {
        for offset in (0..=works.len()).step_by(50) {
            Mock::given(method("GET"))
                .and(path("/ajax/user/123/illusts/bookmarks"))
                .and(query_param("offset", offset.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "error": false,
                    "body": {"works": works[offset..(offset + 50).min(works.len())]}
                })))
                .mount(server)
                .await;
        }
    }

    async fn fetch(
        source: &PixivIntegration,
        pointer: &dyn SourcePointer,
        limit: u32,
    ) -> Vec<PointedItem> {
        source.fetch(pointer, limit).await.unwrap().try_collect().await.unwrap()
    }

    fn ids(items: &[PointedItem]) -> Vec<i64> {
        items
            .iter()
            .map(|item| item.source_item.attrs["illustrationId"].as_i64().unwrap())
            .collect()
    }

    fn advance(pointer: &mut dyn SourcePointer, items: &[PointedItem]) {
        for item in items {
            pointer.update(&item.source_item, item.item_pointer.as_ref());
        }
    }

    #[tokio::test]
    async fn bookmark_increment_resumes_oldest_first_across_pages_and_id_widths() {
        let server = MockServer::start().await;
        mock_bookmarks(&server, (95..=205).rev().map(|id| illustration(id, 1)).collect())
            .await;
        let source = source(&server, true);
        let mut pointer = source.parse_raw_pointer(json!({
            "topBookmarkId": "98",
            "currentBookmarkId": "1",
            "touchBottom": true
        }));

        let items = fetch(&source, pointer.as_ref(), 3).await;
        assert_eq!(ids(&items), vec![99, 100, 101]);
        // Simulate interruption before the last item is settled.
        advance(pointer.as_mut(), &items[..2]);
        assert_eq!(pointer.dump()["topBookmarkId"], "100");
        let items = fetch(&source, pointer.as_ref(), 200).await;
        assert_eq!(ids(&items), (101..=205).collect::<Vec<_>>());
        advance(pointer.as_mut(), &items);
        assert_eq!(pointer.dump()["topBookmarkId"], "205");
    }

    #[tokio::test]
    async fn bookmark_history_finishes_only_after_the_last_item_is_processed() {
        let server = MockServer::start().await;
        mock_bookmarks(
            &server,
            (990..=1002).rev().map(|id| illustration(id, 1)).collect(),
        )
        .await;
        let source = source(&server, true);
        let mut pointer = source.default_pointer();
        let items = fetch(&source, pointer.as_ref(), 2).await;
        assert_eq!(ids(&items), vec![1002, 1001]);
        advance(pointer.as_mut(), &items);
        assert_eq!(pointer.dump()["touchBottom"], false);

        server.reset().await;
        mock_bookmarks(
            &server,
            (990..=1003).rev().map(|id| illustration(id, 1)).collect(),
        )
        .await;
        let items = fetch(&source, pointer.as_ref(), 11).await;
        assert_eq!(ids(&items), (990..=1000).rev().collect::<Vec<_>>());
        advance(pointer.as_mut(), &items[..10]);
        assert_eq!(pointer.dump()["touchBottom"], false);
        advance(pointer.as_mut(), &items[10..]);
        assert_eq!(pointer.dump()["touchBottom"], true);
        assert_eq!(pointer.dump()["topBookmarkId"], "1002");
        assert_eq!(ids(&fetch(&source, pointer.as_ref(), 10).await), vec![1003]);
    }

    #[tokio::test]
    async fn bookmark_history_looks_past_a_full_page_and_masked_tail() {
        let server = MockServer::start().await;
        let works = (1..=100)
            .rev()
            .map(|id| {
                let mut work = illustration(id, 1);
                work["isMasked"] = json!(id <= 50);
                work
            })
            .collect();
        mock_bookmarks(&server, works).await;
        let source = source(&server, true);
        let mut pointer = source.default_pointer();
        let items = fetch(&source, pointer.as_ref(), 50).await;
        assert_eq!(ids(&items), (51..=100).rev().collect::<Vec<_>>());
        advance(pointer.as_mut(), &items[..49]);
        assert_eq!(pointer.dump()["touchBottom"], false);
        advance(pointer.as_mut(), &items[49..]);
        assert_eq!(pointer.dump()["touchBottom"], true);
    }

    #[tokio::test]
    async fn bookmark_history_can_finish_when_the_remaining_tail_was_deleted() {
        let server = MockServer::start().await;
        mock_bookmarks(
            &server,
            (106..=110).rev().map(|id| illustration(id, 1)).collect(),
        )
        .await;
        let source = source(&server, true);
        let mut pointer = source.parse_raw_pointer(json!({
            "topBookmarkId": "110",
            "currentBookmarkId": "106",
            "touchBottom": false
        }));
        let items = fetch(&source, pointer.as_ref(), 10).await;
        assert_eq!(ids(&items), vec![106]);
        advance(pointer.as_mut(), &items);
        assert_eq!(pointer.dump()["touchBottom"], true);
    }

    #[tokio::test]
    async fn following_paginates_and_resumes_author_history_in_ascending_order() {
        let server = MockServer::start().await;
        let users: Vec<_> = (1..=50)
            .map(|user| json!({"userId": user, "illusts": [illustration(10, user)]}))
            .collect();
        for (offset, users) in [
            (0, users),
            (
                50,
                vec![json!({
                    "userId": "51",
                    "illusts": [illustration(104, 51), illustration(103, 51), illustration(102, 51)]
                })],
            ),
        ] {
            Mock::given(method("GET"))
                .and(path("/ajax/user/123/following"))
                .and(query_param("offset", offset.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "error": false, "body": {"users": users}
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/ajax/user/51/profile/all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": false,
                "body": {"illusts": {"101": null, "102": null}, "manga": {"103": null, "104": null}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ajax/user/51/illusts"))
            .and(query_param("ids[]", "101"))
            .and(query_param("ids[]", "104"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": false,
                "body": {
                    "104": illustration(104, 51), "102": illustration(102, 51),
                    "101": illustration(101, 51), "103": illustration(103, 51)
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let source = source(&server, false);
        let mut record =
            Map::from_iter((1..=50).map(|user| (user.to_string(), json!(10))));
        record.insert("51".into(), json!(100));
        let mut pointer =
            source.parse_raw_pointer(json!({"lastIllustrationRecord": record}));

        let items = fetch(&source, pointer.as_ref(), 2).await;
        assert_eq!(ids(&items), vec![101, 102, 103, 104]);
        advance(pointer.as_mut(), &items[..2]);
        let resumed = fetch(&source, pointer.as_ref(), 2).await;
        assert_eq!(ids(&resumed), vec![103, 104]);
        advance(pointer.as_mut(), &resumed);
        // A replayed older item cannot move the author's cursor backwards.
        advance(pointer.as_mut(), &items[..1]);
        assert_eq!(pointer.dump()["lastIllustrationRecord"]["51"], 104);
    }

    #[test]
    fn derives_user_from_session() {
        let p =
            Map::from_iter([("session-id".into(), Value::String("12345_token".into()))]);
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &p,
                )
                .is_ok()
        );
    }
    #[test]
    fn rejects_invalid_session() {
        let p = Map::from_iter([("session-id".into(), Value::String("bad".into()))]);
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &p,
                )
                .is_err()
        );
    }
}

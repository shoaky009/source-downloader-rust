use crate::http;
use crate::instance::mikan::MikanClient;
use crate::util;
use crate::util::{AsyncExpandIterator, ExpandHandler, IterationResult};
use rss_for_mikan::{Channel, Item};
use serde::{Deserialize, Serialize};
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, EMPTY_POINTER, ItemPointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source, SourceItems,
    SourcePointer, deserialize_component_config, source_items,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{Map, Value, json};
use source_downloader_sdk::time::OffsetDateTime;
use source_downloader_sdk::{SdComponent, SourceItem, serde_json};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;
use time::format_description::BorrowedFormatItem;
use time::{PrimitiveDateTime, UtcOffset};

pub struct MikanSourceSupplier {}

pub const SUPPLIER: MikanSourceSupplier = MikanSourceSupplier {};

static DATETIME_FORMAT: &[BorrowedFormatItem] = time::macros::format_description!(
    "[year]-[month]-[day]T[hour]:[minute]:[second][optional [.[subsecond]]]"
);
static TIME_OFFSET: UtcOffset = time::macros::offset!(+8);

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct MikanSourceConfig {
    url: String,
    #[serde(default)]
    all_episode: bool,
}

impl ComponentSupplier for MikanSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("mikan".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<MikanSourceConfig>(props)?;
        let http_client = http::build_client()?;
        Ok(Arc::new(MikanSource {
            url: config.url,
            all_episode: config.all_episode,
            mikan_client: Arc::new(MikanClient::new(None, http_client.clone())),
            http_client,
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Provides anime releases from Mikanani.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "url":{"type":"string"},
                    "all-episode":{"type":"boolean","default":false}
                },
                "required":["url"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            #[rustfmt::skip]
            source_pointer_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "latest":{"type":"string","format":"date-time"},
                    "shows":{
                        "type":"object",
                        "additionalProperties":{"type":"string","format":"date-time"}
                    }
                },
                "required":["latest","shows"]
            })),
        }))
    }
}

#[derive(SdComponent)]
#[component(Source)]
struct MikanSource {
    pub url: String,
    pub all_episode: bool,
    pub mikan_client: Arc<MikanClient>,
    pub http_client: reqwest::Client,
}

impl Debug for MikanSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MikanSource")
            .field("url", &self.url)
            .field("all_episode", &self.all_episode)
            .finish()
    }
}

impl Display for MikanSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "mikan")
    }
}

#[async_trait]
impl Source for MikanSource {
    async fn fetch(
        &self,
        source_pointer: &dyn SourcePointer,
        limit: u32,
    ) -> Result<SourceItems, ProcessingError> {
        let response = http::execute(
            &self.http_client,
            self.http_client.get(self.url.as_str()),
            "Fetch Mikan RSS",
        )
        .await?;
        let content = response
            .bytes()
            .await
            .map_err(|error| http::map_error(error, "Read Mikan RSS response body"))?;

        let channel = Channel::read_from(&content[..]).map_err(|e| {
            ProcessingError::non_retryable(format!("Failed to parse RSS, {}", e))
        })?;

        let items: Vec<SourceItem> =
            channel.items().iter().filter_map(Self::convert_item).collect();

        if !self.all_episode {
            let result = items
                .into_iter()
                .take(limit as usize)
                .map(|source_item| PointedItem {
                    source_item,
                    item_pointer: EMPTY_POINTER.clone(),
                })
                .collect();
            return Ok(source_items(result));
        }

        let pointer =
            source_pointer.as_any().downcast_ref::<MikanSourcePointer>().ok_or_else(
                || ProcessingError::non_retryable("Invalid Mikan source pointer"),
            )?;
        let handler = MikanItemExpandHandler {
            client: self.mikan_client.clone(),
            http_client: self.http_client.clone(),
            pointer,
        };
        let expanded_items = AsyncExpandIterator::new(items, limit, Box::new(handler))
            .collect_all()
            .await?;

        Ok(source_items(expanded_items))
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(MikanSourcePointer {
            latest: OffsetDateTime::now_utc(),
            shows: HashMap::new(),
        })
    }

    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<MikanSourcePointer>(value).unwrap_or_default())
    }
}

impl MikanSource {
    fn try_convert_item(item: &Item) -> Result<SourceItem, String> {
        let title = item.title().ok_or_else(|| "missing title".to_string())?.to_owned();

        let link = Uri::from_str(item.link().ok_or_else(|| "missing link".to_string())?)
            .map_err(|e| format!("invalid link: {e}"))?;

        let enclosure =
            item.enclosure().ok_or_else(|| "missing enclosure".to_string())?;

        let download_uri = Uri::from_str(enclosure.url())
            .map_err(|e| format!("invalid enclosure URL: {e}"))?;

        let pub_date = item
            .torrent
            .as_ref()
            .and_then(|torrent| torrent.pub_date.as_deref())
            .ok_or_else(|| "missing publication date".to_string())?;

        let datetime = PrimitiveDateTime::parse(pub_date, DATETIME_FORMAT)
            .map_err(|e| format!("invalid publication date: {e}"))?
            .assume_offset(TIME_OFFSET);

        Ok(SourceItem {
            title,
            link,
            datetime,
            content_type: enclosure.mime_type().to_owned(),
            download_uri,
            attrs: Default::default(),
            tags: Default::default(),
            identity: None,
        })
    }

    fn convert_item(item: &Item) -> Option<SourceItem> {
        match MikanSource::try_convert_item(item) {
            Ok(item) => Some(item),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    title = ?item.title(),
                    link = ?item.link(),
                    "Failed to convert Mikan RSS item"
                );
                None
            }
        }
    }
}

struct MikanItemExpandHandler<'a> {
    client: Arc<MikanClient>,
    http_client: reqwest::Client,
    pointer: &'a MikanSourcePointer,
}

#[async_trait]
impl ExpandHandler<SourceItem, PointedItem> for MikanItemExpandHandler<'_> {
    async fn expand(
        &self,
        item: SourceItem,
    ) -> Result<IterationResult<PointedItem>, ProcessingError> {
        let fansub_rss = self
            .client
            .get_episode_page_info(&item.link.to_string())
            .await
            .map_err(|e| ProcessingError::retryable(e.to_string()))?
            .fansub_rss;
        if fansub_rss.is_none() {
            return Ok(IterationResult { items: vec![], has_next: false });
        }
        let fansub_rss = fansub_rss.unwrap();
        let fansub_uri = Uri::from_str(&fansub_rss).unwrap();
        let fansub_query = util::query_map(&fansub_uri);
        let bangumi_id = fansub_query.get("bangumiId");
        if bangumi_id.is_none() {
            return Ok(IterationResult { items: vec![], has_next: false });
        }
        let subgroup_id = fansub_query.get("subgroupid");
        if subgroup_id.is_none() {
            return Ok(IterationResult { items: vec![], has_next: false });
        }
        let bangumi_id = bangumi_id.unwrap();
        let subgroup_id = subgroup_id.unwrap();

        let response = http::execute(
            &self.http_client,
            self.http_client.get(&fansub_rss),
            "Fetch Mikan fansub RSS",
        )
        .await?;
        let content = response.bytes().await.map_err(|error| {
            http::map_error(error, "Read Mikan fansub RSS response body")
        })?;

        let channel = Channel::read_from(&content[..])
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let mut fansub_items: Vec<SourceItem> =
            channel.items.iter().filter_map(MikanSource::convert_item).collect();
        fansub_items.sort_by_key(|a| a.datetime);
        if !fansub_items.contains(&item) {
            tracing::debug!("Item不在RSS列表中: {:?}", item);
            fansub_items.push(item);
        }

        let key = format!("{}-{}", bangumi_id, subgroup_id);
        let result: Vec<PointedItem> = fansub_items
            .into_iter()
            .filter(|x| {
                match self.pointer.shows.get(&key) {
                    None => true,                     // 没有记录，保留
                    Some(date) => *date > x.datetime, // 必须比记录的时间晚
                }
            })
            .map(|it| {
                let ptr = FansubPointer {
                    bangumi_id: bangumi_id.to_string(),
                    sub_group_id: subgroup_id.to_string(),
                    date: it.datetime,
                };
                PointedItem { source_item: it, item_pointer: Arc::new(ptr) }
            })
            .collect();

        Ok(IterationResult { items: result, has_next: false })
    }
}

#[derive(Debug)]
struct FansubPointer {
    pub bangumi_id: String,
    pub sub_group_id: String,
    pub date: OffsetDateTime,
}

impl FansubPointer {
    fn key(&self) -> String {
        format!("{}-{}", self.bangumi_id, self.sub_group_id)
    }
}

impl ItemPointer for FansubPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Serialize, Deserialize)]
struct MikanSourcePointer {
    latest: OffsetDateTime,
    shows: HashMap<String, OffsetDateTime>,
}

impl Default for MikanSourcePointer {
    fn default() -> Self {
        Self { latest: OffsetDateTime::UNIX_EPOCH, shows: HashMap::new() }
    }
}

impl SourcePointer for MikanSourcePointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap()
    }

    fn update(&mut self, _: &SourceItem, item_pointer: &dyn ItemPointer) {
        if let Some(pointer) = item_pointer.as_any().downcast_ref::<FansubPointer>() {
            self.shows
                .entry(pointer.key())
                .and_modify(|date| *date = (*date).max(pointer.date))
                .or_insert(pointer.date);
            self.latest = self.latest.max(pointer.date);
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_mikan_rss_item() {
        let channel = Channel::read_from(
            r#"<?xml version="1.0" encoding="utf-8"?>
                <rss
                  xmlns:torrent="https://mikanani.me/0.1/"
                  version="2.0">
                  <channel>
                    <title>Mikan Project - My Bangumi</title>
                    <link>https://mikanani.me</link>
                    <description>Mikan Project - My Bangumi</description>
                    <item>
                        <guid isPermaLink="false">[ANi] MAO / MAO 摩绪 - 18 [1080P][Baha][WEB-DL][AAC AVC][CHT][MP4]</guid>
                        <link>https://mikanani.me/Home/Episode/f7ae19bb438881d204581c2b4fbaf0185e4304ea</link>
                        <title>[ANi] MAO / MAO 摩绪 - 18 [1080P][Baha][WEB-DL][AAC AVC][CHT][MP4]</title>
                        <description>[ANi] MAO / MAO 摩绪 - 18 [1080P][Baha][WEB-DL][AAC AVC][CHT][MP4][409.4 MB]</description>
                        <torrent xmlns="https://mikanani.me/0.1/">
                            <link>https://mikanani.me/Home/Episode/f7ae19bb438881d204581c2b4fbaf0185e4304ea</link>
                            <contentLength>429287008</contentLength>
                            <pubDate>2026-08-03T12:00:48.405194</pubDate>
                        </torrent>
                        <enclosure type="application/x-bittorrent" length="429287008" url="https://mikanani.me/Download/20260803/f7ae19bb438881d204581c2b4fbaf0185e4304ea.torrent"/>
                    </item>
                    <item>
                        <guid isPermaLink="false">【喵萌奶茶屋】★07月新番★[二十世纪电气目录 / 20 Seiki Denki Mokuroku / Nijusseiki Denki Mokuroku][05][1080p][简日双语]</guid>
                        <link>https://mikanani.me/Home/Episode/43c6117ce84eccf09e931c9601c83015b350bc95</link>
                        <title>【喵萌奶茶屋】★07月新番★[二十世纪电气目录 / 20 Seiki Denki Mokuroku / Nijusseiki Denki Mokuroku][05][1080p][简日双语]</title>
                        <description>【喵萌奶茶屋】★07月新番★[二十世纪电气目录 / 20 Seiki Denki Mokuroku / Nijusseiki Denki Mokuroku][05][1080p][简日双语][644.4MB]</description>
                        <torrent xmlns="https://mikanani.me/0.1/">
                            <link>https://mikanani.me/Home/Episode/43c6117ce84eccf09e931c9601c83015b350bc95</link>
                            <contentLength>675702400</contentLength>
                            <pubDate>2026-08-03T03:28:00</pubDate>
                        </torrent>
                        <enclosure type="application/x-bittorrent" length="675702400" url="https://mikanani.me/Download/20260803/43c6117ce84eccf09e931c9601c83015b350bc95.torrent"/>
                    </item>
                  </channel>
                </rss>"#
                .as_bytes(),
        )
            .unwrap();
        assert_eq!(2, channel.items.len())
    }
}

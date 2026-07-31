use crate::instance::mikan::MikanClient;
use crate::util;
use crate::util::{AsyncExpandIterator, ExpandHandler, IterationResult};
use reqwest::StatusCode;
use rss::{Channel, Item};
use serde::{Deserialize, Serialize};
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, EMPTY_POINTER, ItemPointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source,
    SourcePointer,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::time::OffsetDateTime;
use source_downloader_sdk::time::format_description::well_known::Rfc2822;
use source_downloader_sdk::{SdComponent, SourceItem, serde_json};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;

pub struct MikanSourceSupplier {}

pub const SUPPLIER: MikanSourceSupplier = MikanSourceSupplier {};

impl ComponentSupplier for MikanSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("mikan".to_string())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let url = props
            .get("url")
            .ok_or_else(|| ComponentError::from("Missing 'url' property"))?
            .as_str();
        if url.is_none() {
            return Err(ComponentError::from("Invalid 'url' property"));
        }
        let url = url.unwrap().to_string();
        let all_episode =
            props.get("all-episode").map(|v| v.as_bool()).flatten().unwrap_or(false);
        Ok(Arc::new(MikanSource {
            url,
            all_episode,
            mikan_client: Arc::new(MikanClient::new(None)),
            http_client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("Failed to build client"),
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
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
    async fn fetch<'pointer>(
        &self,
        source_pointer: &'pointer dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let content = self
            .http_client
            .get(self.url.as_str())
            .send()
            .await
            .map_err(|e| reqwest_error(&e, "Failed to fetch RSS"))?
            .bytes()
            .await
            .map_err(|e| reqwest_error(&e, "Failed to read bytes"))?;

        let channel = Channel::read_from(&content[..]).map_err(|e| {
            ProcessingError::non_retryable(format!("Failed to parse RSS, {}", e))
        })?;

        let items: Vec<SourceItem> =
            channel.items().iter().filter_map(Self::convert_item).collect();

        if !self.all_episode {
            let result = items
                .into_iter()
                .map(|source_item| PointedItem {
                    source_item,
                    item_pointer: EMPTY_POINTER.clone(),
                })
                .collect();
            return Ok(result);
        }

        let pointer =
            source_pointer.as_any().downcast_ref::<MikanSourcePointer>().ok_or_else(
                || ProcessingError::non_retryable("Invalid Mikan source pointer"),
            )?;
        let handler =
            MikanItemExpandHandler { client: self.mikan_client.clone(), pointer };
        let expanded_items = AsyncExpandIterator::new(items, limit, Box::new(handler))
            .collect_all()
            .await?;

        Ok(expanded_items)
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
    // TODO如果失败要打印一下日志
    fn convert_item(item: &Item) -> Option<SourceItem> {
        let title = item.title()?.to_owned();
        let link = Uri::from_str(item.link()?).ok()?;
        let enclosure = item.enclosure()?;
        let download_uri = Uri::from_str(enclosure.url()).ok()?;
        let datetime = OffsetDateTime::parse(item.pub_date()?, &Rfc2822).ok()?;
        Some(SourceItem {
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
}

struct MikanItemExpandHandler<'a> {
    client: Arc<MikanClient>,
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

        let content = reqwest::get(&fansub_rss)
            .await
            .map_err(|e| ProcessingError::retryable(e.to_string()))?
            .bytes()
            .await
            .map_err(|e| ProcessingError::retryable(e.to_string()))?;

        let channel = Channel::read_from(&content[..])
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let mut fansub_items: Vec<SourceItem> =
            channel.items.iter().filter_map(|i| MikanSource::convert_item(i)).collect();
        fansub_items.sort_by(|a, b| a.datetime.cmp(&b.datetime));
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

pub fn reqwest_error(e: &reqwest::Error, prefix: &str) -> ProcessingError {
    if e.is_timeout() || e.is_connect() {
        return ProcessingError::retryable(format!("{}, {}", prefix, e));
    }

    if let Some(status) = e.status() {
        let retry = matches!(
            status,
            StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::INTERNAL_SERVER_ERROR
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT
        );
        if retry {
            return ProcessingError::retryable(format!("{}, {}", prefix, e));
        }
    }
    ProcessingError::non_retryable(format!("{}, {}", prefix, e))
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
            br#"<?xml version="1.0" encoding="utf-8"?>
                <rss
                  xmlns:torrent="https://mikanani.me/0.1/"
                  version="2.0">
                  <channel>
                    <title>Mikan Project - My Bangumi</title>
                    <link>https://mikanani.me</link>
                    <description>Mikan Project - My Bangumi</description>
                    <item>
                      <title>Episode 01</title>
                      <link>https://mikanani.me/Home/Episode/example</link>
                      <description>[Group] Episode 01</description>
                      <guid isPermaLink="false">https://mikanani.me/Home/Episode/example</guid>
                      <pubDate>Fri, 24 Jul 2026 16:22:47 +0800</pubDate>
                      <enclosure
                        url="https://mikanani.me/Download/example.torrent"
                        length="123"
                        type="application/x-bittorrent" />
                      <torrent xmlns="https://mikanani.me/0.1/">
                        <link>https://mikanani.me/Download/example.torrent</link>
                        <contentLength>123</contentLength>
                        <pubDate>2025-01-01T00:00:00.000</pubDate>
                      </torrent>
                    </item>
                  </channel>
                </rss>"#
                .as_slice(),
        )
        .unwrap();

        let item = MikanSource::convert_item(&channel.items()[0]).unwrap();

        assert_eq!("Episode 01", item.title);
        assert_eq!("2026-07-24 16:22:47.0 +08:00:00", item.datetime.to_string());
        assert_eq!(
            "https://mikanani.me/Download/example.torrent",
            item.download_uri.to_string()
        );
        assert_eq!("application/x-bittorrent", item.content_type);
    }
}

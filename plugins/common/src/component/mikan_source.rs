use crate::instance::mikan::MikanClient;
use crate::util;
use crate::util::{AsyncExpandIterator, ExpandHandler, IterationResult};
use parking_lot::RwLock;
use reqwest::StatusCode;
use rss_for_mikan::{Channel, Item};
use serde::{Deserialize, Serialize};
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    empty_item_pointer, ComponentError, ComponentSupplier, ComponentType, ItemPointer, PointedItem,
    ProcessingError, SdComponent, SdComponentMetadata, Source, SourcePointer,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::time::format_description::BorrowedFormatItem;
use source_downloader_sdk::time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};
use source_downloader_sdk::{serde_json, time, SdComponent, SourceItem};
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::str::FromStr;
use std::sync::Arc;

pub struct MikanSourceSupplier {}

pub const SUPPLIER: MikanSourceSupplier = MikanSourceSupplier {};

static DATETIME_FORMAT: &[BorrowedFormatItem] =
    time::macros::format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond]");
static TIME_OFFSET: UtcOffset = time::macros::offset!(+8);

impl ComponentSupplier for MikanSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("mikan".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let url = props
            .get("url")
            .ok_or_else(|| ComponentError::from("Missing 'url' property"))?
            .as_str();
        if url.is_none() {
            return Err(ComponentError::from("Invalid 'url' property"));
        }
        let url = url.unwrap().to_string();
        let all_episode = props
            .get("all-episode")
            .map(|v| v.as_bool())
            .flatten()
            .unwrap_or(false);
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

#[async_trait]
impl Source for MikanSource {
    async fn fetch(
        &self,
        source_pointer: Arc<dyn SourcePointer>,
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

        let channel = Channel::read_from(&content[..])
            .map_err(|e| ProcessingError::non_retryable(format!("Failed to parse RSS, {}", e)))?;

        let items: Vec<SourceItem> = channel
            .items
            .iter()
            .filter_map(|i| Self::convert_item(i))
            .collect();

        if !self.all_episode {
            let result = items
                .into_iter()
                .map(|source_item| PointedItem {
                    source_item,
                    item_pointer: empty_item_pointer(),
                })
                .collect();
            return Ok(result);
        }

        let mp = source_pointer
            .into_any()
            .downcast::<MikanSourcePointer>()
            .unwrap();
        let handler = MikanItemExpandHandler {
            client: self.mikan_client.clone(),
            pointer: mp,
        };
        let expanded_items = AsyncExpandIterator::new(items, limit, Box::new(handler))
            .collect_all()
            .await?;

        Ok(expanded_items)
    }

    fn default_pointer(&self) -> Arc<dyn SourcePointer> {
        Arc::new(MikanSourcePointer {
            latest: RwLock::new(OffsetDateTime::now_utc()),
            shows: RwLock::new(HashMap::new()),
        })
    }

    fn parse_raw_pointer(&self, value: Value) -> Arc<dyn SourcePointer> {
        Arc::new(serde_json::from_value::<MikanSourcePointer>(value).unwrap_or_default())
    }
}

impl MikanSource {
    // TODO如果失败要打印一下日志
    fn convert_item(item: &Item) -> Option<SourceItem> {
        let title = item.title.as_ref()?.to_string();
        let link_str = item.link.as_ref()?;
        let link = Uri::from_str(link_str).ok()?;
        let enclosure = item.enclosure.as_ref()?;
        let download_uri = Uri::from_str(&enclosure.url).ok()?;
        let pub_date = item
            .torrent
            .as_ref()
            .map(|x| x.pub_date.to_owned())
            .flatten()?;
        let datetime = PrimitiveDateTime::parse(&pub_date, DATETIME_FORMAT)
            .ok()?
            .assume_offset(TIME_OFFSET);
        Some(SourceItem {
            title,
            link,
            datetime,
            content_type: enclosure.mime_type.clone(),
            download_uri,
            attrs: Default::default(),
            tags: Default::default(),
            identity: None,
        })
    }
}

struct MikanItemExpandHandler {
    client: Arc<MikanClient>,
    pointer: Arc<MikanSourcePointer>,
}

#[async_trait]
impl ExpandHandler<SourceItem, PointedItem> for MikanItemExpandHandler {
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
            return Ok(IterationResult {
                items: vec![],
                has_next: false,
            });
        }
        let fansub_rss = fansub_rss.unwrap();
        let fansub_uri = Uri::from_str(&fansub_rss).unwrap();
        let fansub_query = util::query_map(&fansub_uri);
        let bangumi_id = fansub_query.get("bangumiId");
        if bangumi_id.is_none() {
            return Ok(IterationResult {
                items: vec![],
                has_next: false,
            });
        }
        let subgroup_id = fansub_query.get("subgroupid");
        if subgroup_id.is_none() {
            return Ok(IterationResult {
                items: vec![],
                has_next: false,
            });
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
        let mut fansub_items: Vec<SourceItem> = channel
            .items
            .iter()
            .filter_map(|i| MikanSource::convert_item(i))
            .collect();
        fansub_items.sort_by(|a, b| a.datetime.cmp(&b.datetime));
        if !fansub_items.contains(&item) {
            tracing::debug!("Item不在RSS列表中: {:?}", item);
            fansub_items.push(item);
        }

        let key = format!("{}-{}", bangumi_id, subgroup_id);
        let result: Vec<PointedItem> = fansub_items
            .into_iter()
            .filter(|x| {
                match self.pointer.shows.read().get(&key) {
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
                PointedItem {
                    source_item: it,
                    item_pointer: Box::new(ptr),
                }
            })
            .collect();

        Ok(IterationResult {
            items: result,
            has_next: false,
        })
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
    latest: RwLock<OffsetDateTime>,
    shows: RwLock<HashMap<String, OffsetDateTime>>,
}

impl Default for MikanSourcePointer {
    fn default() -> Self {
        Self {
            latest: RwLock::new(OffsetDateTime::UNIX_EPOCH),
            shows: Default::default(),
        }
    }
}

impl SourcePointer for MikanSourcePointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap()
    }

    fn update(&self, _: &SourceItem, item_pointer: &Box<dyn ItemPointer>) {
        if let Some(p) = item_pointer.as_any().downcast_ref::<FansubPointer>() {
            self.shows.write().insert(p.key(), p.date);
            let mut g = self.latest.write();
            *g = (*g).max(p.date);
        }
    }

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

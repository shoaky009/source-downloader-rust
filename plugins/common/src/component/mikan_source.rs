use crate::instance::mikan::MikanClient;
use crate::util;
use crate::util::{AsyncExpandIterator, ExpandHandler, IterationResult};
use parking_lot::RwLock;
use rss_for_mikan::{Channel, Item};
use sdk::async_trait::async_trait;
use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemPointer, PointedItem, ProcessingError,
    SdComponent, SdComponentMetadata, Source, SourcePointer, empty_item_pointer,
};
use sdk::http::Uri;
use sdk::serde_json::{Map, Value};
use sdk::time::format_description::BorrowedFormatItem;
use sdk::time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};
use sdk::{SdComponent, SourceItem, serde_json, time};
use serde::{Deserialize, Serialize};
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
            client: Arc::new(MikanClient::new(None)),
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
    pub client: Arc<MikanClient>,
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
        pointer: Arc<dyn SourcePointer>,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let content = reqwest::get(self.url.as_str())
            .await
            .map_err(|e| ProcessingError {
                message: format!("Failed to fetch RSS: {}", e),
                skip: false,
            })?
            .bytes()
            .await
            .map_err(|e| ProcessingError {
                message: format!("Failed to read bytes: {}", e),
                skip: false,
            })?;

        let channel = Channel::read_from(&content[..]).map_err(|e| ProcessingError {
            message: format!("Failed to parse RSS: {}", e),
            skip: false,
        })?;

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

        let mp = pointer.into_any().downcast::<MikanSourcePointer>().unwrap();
        let handler = MikanItemExpandHandler {
            client: self.client.clone(),
            pointer: mp,
        };
        let expanded_items = AsyncExpandIterator::new(items, 100, Box::new(handler))
            .collect_all()
            .await;

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
        })
    }
}

struct MikanItemExpandHandler {
    client: Arc<MikanClient>,
    pointer: Arc<MikanSourcePointer>,
}

#[async_trait]
impl ExpandHandler<SourceItem, PointedItem> for MikanItemExpandHandler {
    async fn expand(&self, item: SourceItem) -> IterationResult<PointedItem> {
        let fansub_rss = self
            .client
            .get_episode_page_info(&item.link.to_string())
            .await
            .unwrap()
            .fansub_rss;
        if fansub_rss.is_none() {
            return IterationResult {
                items: vec![],
                continue_expand: false,
            };
        }
        let fansub_rss = fansub_rss.unwrap();
        let fansub_uri = Uri::from_str(&fansub_rss).unwrap();
        let fansub_query = util::query_map(&fansub_uri);
        let bangumi_id = fansub_query.get("bangumiId");
        if bangumi_id.is_none() {
            return IterationResult {
                items: vec![],
                continue_expand: false,
            };
        }
        let subgroup_id = fansub_query.get("subgroupid");
        if subgroup_id.is_none() {
            return IterationResult {
                items: vec![],
                continue_expand: false,
            };
        }
        let bangumi_id = bangumi_id.unwrap();
        let subgroup_id = subgroup_id.unwrap();

        let content = reqwest::get(&fansub_rss)
            .await
            .map_err(|e| ProcessingError {
                message: e.to_string(),
                skip: false,
            })
            .unwrap()
            .bytes()
            .await
            .map_err(|e| ProcessingError {
                message: e.to_string(),
                skip: false,
            })
            .unwrap();

        let channel = Channel::read_from(&content[..])
            .map_err(|e| ProcessingError {
                message: e.to_string(),
                skip: false,
            })
            .unwrap();
        let mut fansub_items: Vec<SourceItem> = channel
            .items
            .iter()
            .filter_map(|i| MikanSource::convert_item(i))
            .collect();
        fansub_items.sort_by(|a, b| a.datetime.cmp(&b.datetime));
        if !fansub_items.contains(&item) {
            tracing::debug!("Item不在RSS列表中: {:?}", item);
            // Rust中通常需要 clone 所有权放入 Vec，或者 item 本身就是 Copy 的
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

        IterationResult {
            items: result,
            continue_expand: false,
        }
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

    fn update(&self, _: &SourceItem, item_pointer: Box<dyn ItemPointer>) {
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

pub use component_macro::*;
pub use http::Uri;
pub use serde::{Deserialize, Serialize};
pub use serde_json::{Map, Value, from_str, from_value, to_string, to_value, to_vec};
use std::collections::HashSet;
pub use storage::*;
pub use time::{
    Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, UtcDateTime, UtcOffset, Weekday,
};

pub mod component;
pub mod instance;
pub mod plugin;
pub mod storage;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceItem {
    pub title: String,
    #[serde(with = "http_serde::uri")]
    pub link: Uri,
    #[serde(with = "time::serde::iso8601")]
    pub datetime: OffsetDateTime,
    pub content_type: String,
    #[serde(with = "http_serde::uri")]
    pub download_uri: Uri,
    #[serde(default)]
    pub attrs: Map<String, Value>,
    #[serde(default)]
    pub tags: HashSet<String>,
}

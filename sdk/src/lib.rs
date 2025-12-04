pub use component_macro::*;
pub use http::Uri;
pub use serde::{Deserialize, Serialize};
pub use serde_json::{Map, Value, from_str, from_value, to_string, to_value, to_vec};
use std::collections::HashSet;
pub use storage::*;
pub use time::*;

pub mod component;
pub mod instance;
pub mod plugin;
pub mod storage;

#[derive(Debug, Clone)]
pub struct SourceItem {
    pub title: String,
    pub link: Uri,
    pub datetime: OffsetDateTime,
    pub content_type: String,
    pub download_uri: Uri,
    pub attrs: Map<String, Value>,
    pub tags: HashSet<String>,
}

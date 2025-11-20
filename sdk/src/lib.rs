pub use http::Uri;
pub use serde_json::{Map, Value};
pub use serde::{Serialize, Deserialize};
use std::collections::HashSet;
pub use time::OffsetDateTime;
pub use component_macro::*;

pub mod component;
pub mod instance;
pub mod plugin;

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

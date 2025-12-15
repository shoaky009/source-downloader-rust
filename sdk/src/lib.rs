pub use component_macro::*;
pub use http;
pub use serde;
pub use serde_json;
pub use time;
pub mod component;
pub mod instance;
pub mod plugin;
pub mod storage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceItem {
    pub title: String,
    #[serde(with = "http_serde::uri")]
    pub link: http::Uri,
    #[serde(with = "time::serde::iso8601")]
    pub datetime: time::OffsetDateTime,
    pub content_type: String,
    #[serde(with = "http_serde::uri")]
    pub download_uri: http::Uri,
    #[serde(default)]
    pub attrs: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub tags: std::collections::HashSet<String>,
}

// pub mod prelude {
//     pub use component_macro;
//     pub use http;
//     pub use serde;
//     pub use serde_json;
//     pub use time;
// }
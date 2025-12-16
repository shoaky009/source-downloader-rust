pub use async_trait;
pub use component_macro::*;
pub use http;
// pub use serde;
pub use serde_json;
use std::fmt::Display;
pub use time;
pub mod component;
pub mod instance;
pub mod plugin;
pub mod storage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
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

impl SourceItem {
    pub fn hashing(&self) -> String {
        let s = format!(
            "{}-{}-{}-{}",
            self.title, self.link, self.content_type, self.download_uri
        );
        let b = s.as_bytes();
        let r = fastmurmur3::hash(b).swap_bytes();
        format!("{:032x}", r)
    }
}

#[cfg(test)]
mod test {
    use crate::SourceItem;

    #[test]
    fn test_hashing() {
        let item = SourceItem {
            title: "test".to_string(),
            link: http::Uri::from_static("https://example.com"),
            datetime: time::OffsetDateTime::now_utc(),
            content_type: "text/html".to_string(),
            download_uri: http::Uri::from_static("https://example.com/test.html"),
            attrs: serde_json::Map::new(),
            tags: std::collections::HashSet::new(),
        };
        assert_eq!("89a9f52da0578bd8495906c356c68d69", item.hashing());
    }
}

// pub mod prelude {
//     pub use component_macro;
//     pub use http;
//     pub use serde;
//     pub use serde_json;
//     pub use time;
// }

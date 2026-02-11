pub use async_trait;
pub use component_macro::*;
pub use http;
pub use serde_json;
use std::fmt::{Display, Formatter};
pub use time;
pub mod component;
pub mod instance;
pub mod plugin;
pub mod storage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash)]
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
    pub tags: Vec<String>,
    pub identity: Option<String>,
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

impl Display for SourceItem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceItem")
            .field("title", &self.title)
            .field("link", &self.link)
            .field("datetime", &self.datetime)
            .field("content_type", &self.content_type)
            .field("download_uri", &self.download_uri)
            .field("attrs", &self.attrs)
            .field("tags", &self.tags)
            .field("identity", &self.identity)
            .finish()
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
            tags: vec![],
            identity: None,
        };
        assert_eq!("89a9f52da0578bd8495906c356c68d69", item.hashing());
    }
}

#[cfg(feature = "test")]
pub mod test_utils {
    use crate::SourceItem;
    use crate::component::FileContentStatus::Undetected;
    use crate::component::{FileContent, SourceFile};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    impl Default for SourceItem {
        fn default() -> Self {
            SourceItem {
                title: "".to_string(),
                link: http::Uri::from_static("localhost"),
                datetime: time::OffsetDateTime::now_utc(),
                content_type: "text/html".to_string(),
                download_uri: http::Uri::from_static("localhost"),
                attrs: serde_json::Map::new(),
                tags: vec![],
                identity: None,
            }
        }
    }

    impl Default for SourceFile {
        fn default() -> Self {
            SourceFile {
                path: PathBuf::from(""),
                attrs: Default::default(),
                download_uri: None,
                tags: vec![],
                data: None,
            }
        }
    }

    impl Default for FileContent {
        fn default() -> Self {
            FileContent {
                download_path: PathBuf::new(),
                file_download_path: PathBuf::new(),
                source_save_path: PathBuf::new(),
                pattern_variables: HashMap::new(),
                tags: vec![],
                attrs: serde_json::Map::new(),
                file_uri: None,
                target_save_path: PathBuf::new(),
                target_filename: "".to_string(),
                exist_target_path: None,
                errors: vec![],
                status: Undetected,
                target_path: OnceLock::default(),
            }
        }
    }
}

// pub mod prelude {
//     pub use component_macro;
//     pub use http;
//     pub use serde;
//     pub use serde_json;
//     pub use time;
// }

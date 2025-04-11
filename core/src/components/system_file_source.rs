pub mod system_file_source;

use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PointedItem, SdComponent,
    SdComponentMetadata, Source, empty_pointer,
};

use sdk::{Map, OffsetDateTime, SourceItem, Value};
use std::collections::HashSet;
use std::path::PathBuf;

pub struct SystemFileSourceSupplier;

impl ComponentSupplier<SystemFileSource> for SystemFileSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("system-file".to_string())]
    }

    fn apply(&self, props: Map<String, Value>) -> Result<Box<SystemFileSource>, ComponentError> {
        let path = props
            .get("path")
            .unwrap()
            .as_str()
            .ok_or_else(|| ComponentError::from("Missing path property"))?;

        let mode = props.get("mode").and_then(|v| v.as_i64()).unwrap_or(0) as i8;
        let path = PathBuf::from(path);
        Ok(Box::new(SystemFileSource { path, mode }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

struct SystemFileSource {
    pub path: PathBuf,
    mode: i8,
}

impl SystemFileSource {
    fn test(&self) -> bool {
        self.path.ends_with(".test")
    }
}

impl SdComponent for SystemFileSource {}

impl Source for SystemFileSource {
    fn fetch(&self) -> Vec<PointedItem> {
        vec![PointedItem {
            source_item: SourceItem {
                title: "test".to_string(),
                link: "https://example.com".parse().unwrap(),
                datetime: OffsetDateTime::now_utc(),
                content_type: "text/plain".to_string(),
                download_uri: "https://example.com/download".parse().unwrap(),
                attrs: Map::new(),
                tags: HashSet::new(),
            },
            pointer: empty_pointer(),
        }]
    }
}

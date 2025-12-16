use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, NullSourcePointer, PointedItem,
    ProcessingError, SdComponent, SdComponentMetadata, Source, SourcePointer, empty_item_pointer,
};

use sdk::serde_json::{Map, Value};
use sdk::time::OffsetDateTime;
use sdk::{SdComponent, SourceItem};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

pub struct SystemFileSourceSupplier;
pub const SUPPLIER: SystemFileSourceSupplier = SystemFileSourceSupplier {};

impl ComponentSupplier for SystemFileSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("system-file".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let path = props
            .get("path")
            .ok_or_else(|| ComponentError::from("Missing 'path' property"))?
            .to_string();

        let mode = props.get("mode").and_then(|v| v.as_i64()).unwrap_or(0) as i8;
        let path = PathBuf::from(path);
        Ok(Arc::new(SystemFileSource { path, mode }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(SdComponent, Debug)]
#[component(Source)]
struct SystemFileSource {
    path: PathBuf,
    mode: i8,
}

#[async_trait::async_trait]
impl Source for SystemFileSource {
    async fn fetch(&self, _: Arc<dyn SourcePointer>) -> Result<Vec<PointedItem>, ProcessingError> {
        if self.mode == 1 {
            let _ = fs::read_dir(&self.path);
            return Ok(vec![]);
        }
        Ok(vec![PointedItem {
            source_item: SourceItem {
                title: "test".to_string(),
                link: "https://example.com".parse().unwrap(),
                datetime: OffsetDateTime::now_utc(),
                content_type: "text/plain".to_string(),
                download_uri: "https://example.com/download".parse().unwrap(),
                attrs: Map::new(),
                tags: HashSet::new(),
            },
            item_pointer: empty_item_pointer(),
        }])
    }

    fn default_pointer(&self) -> Arc<dyn SourcePointer> {
        Arc::new(NullSourcePointer {})
    }

    fn parse_raw_pointer(&self, _: Value) -> Arc<dyn SourcePointer> {
        Arc::new(NullSourcePointer {})
    }
}

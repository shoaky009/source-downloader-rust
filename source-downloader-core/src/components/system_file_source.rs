use source_downloader_sdk::component::{
    empty_item_pointer, ComponentError, ComponentSupplier, ComponentType, NullSourcePointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source, SourcePointer,
};

use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::time::OffsetDateTime;
use source_downloader_sdk::{SdComponent, SourceItem};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
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
    async fn fetch(
        &self,
        _: Arc<dyn SourcePointer>,
        _: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        match self.mode {
            0 => self.create_root_file_source_items(),
            1 => self.create_each_file_source_items(),
            _ => Err(ProcessingError::non_retryable(format!(
                "Only support mode 0 or 1, but got: {}",
                self.mode
            ))),
        }
    }

    fn default_pointer(&self) -> Arc<dyn SourcePointer> {
        Arc::new(NullSourcePointer {})
    }

    fn parse_raw_pointer(&self, _: Value) -> Arc<dyn SourcePointer> {
        Arc::new(NullSourcePointer {})
    }
}

impl SystemFileSource {
    fn create_root_file_source_items(&self) -> Result<Vec<PointedItem>, ProcessingError> {
        self.path
            .read_dir()
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?
            .map(|p| Self::from_path(p.unwrap().path().as_path()))
            .collect()
    }

    // Mode 1: 对应 createEachFileSourceItems (path.walk)
    fn create_each_file_source_items(&self) -> Result<Vec<PointedItem>, ProcessingError> {
        walkdir::WalkDir::new(&self.path)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(|x| Self::from_path(x.path()))
            .collect()
    }

    fn from_path(path: &Path) -> Result<PointedItem, ProcessingError> {
        let file_name = path.file_name();
        let is_dir = path.is_dir();
        let file_type = if is_dir { "directory" } else { "file" };
        let file_size = path.metadata().unwrap().len();

        let mut attrs = Map::new();
        attrs.insert("size".to_string(), Value::from(file_size));

        let url = format!("file://{}", path.to_str().unwrap());
        let source_item = SourceItem {
            title: file_name.unwrap().to_string_lossy().to_string(),
            link: url
                .parse()
                .unwrap_or_else(|_| "vfs://unknown".parse().unwrap()),
            datetime: OffsetDateTime::now_utc(),
            content_type: file_type.to_string(),
            download_uri: url
                .parse()
                .unwrap_or_else(|_| "vfs://unknown".parse().unwrap()),
            attrs,
            tags: HashSet::new(),
            identity: None,
        };

        Ok(PointedItem {
            source_item,
            item_pointer: empty_item_pointer(),
        })
    }
}

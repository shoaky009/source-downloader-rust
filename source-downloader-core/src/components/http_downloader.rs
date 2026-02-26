use async_trait::async_trait;
use serde_json::{Map, Value};
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct HttpDownloaderSupplier;
pub const SUPPLIER: HttpDownloaderSupplier = HttpDownloaderSupplier {};

impl ComponentSupplier for HttpDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("http".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(HttpDownloader {
            path: props["download-path"].to_string(),
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        todo!()
    }
}

#[derive(SdComponent, Debug)]
#[component(Downloader)]
#[allow(dead_code, unused)]
struct HttpDownloader {
    path: String,
}

impl Display for HttpDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "http")
    }
}

#[allow(dead_code, unused)]
#[async_trait]
impl Downloader for HttpDownloader {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
        todo!()
    }

    fn default_download_path(&self) -> &str {
        &self.path
    }

    async fn cancel(&self, item: &SourceItem, files: &[SourceFile]) -> Result<(), ProcessingError> {
        todo!()
    }
}

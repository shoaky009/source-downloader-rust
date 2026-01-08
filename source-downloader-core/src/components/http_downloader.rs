use serde_json::{Map, Value};
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader, SdComponent,
    SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::SdComponent;
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

#[allow(dead_code, unused)]
impl Downloader for HttpDownloader {
    fn submit(&self, task: &DownloadTask) -> Result<(), ComponentError> {
        todo!()
    }

    fn default_download_path(&self) -> &str {
        &self.path
    }

    fn cancel(&self, item: &DownloadTask, files: &[SourceFile]) -> Result<(), ComponentError> {
        todo!()
    }
}

use async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader,
    ProcessingError, SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct NoneDownloaderSupplier;
pub const SUPPLIER: NoneDownloaderSupplier = NoneDownloaderSupplier;

impl ComponentSupplier for NoneDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("none".to_owned())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let path = match props.get("downloadPath") {
            None => std::env::current_dir().map_err(|error| {
                ComponentError::new(format!("Failed to get current directory: {error}"))
            })?,
            Some(Value::String(path)) => PathBuf::from(path),
            Some(_) => {
                return Err(ComponentError::from("Invalid 'downloadPath' property"));
            }
        };
        Ok(Arc::new(NoneDownloader {
            download_path: path.to_string_lossy().into_owned(),
        }))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(Downloader)]
pub struct NoneDownloader {
    download_path: String,
}

impl Display for NoneDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("none")
    }
}

#[async_trait]
impl Downloader for NoneDownloader {
    async fn submit(&self, _: &DownloadTask) -> Result<(), ProcessingError> {
        Ok(())
    }

    fn default_download_path(&self) -> &str {
        &self.download_path
    }

    async fn cancel(
        &self,
        _: &SourceItem,
        _: &[SourceFile],
    ) -> Result<(), ProcessingError> {
        Ok(())
    }
}

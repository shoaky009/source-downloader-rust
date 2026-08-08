use async_trait::async_trait;
use serde::Deserialize;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader,
    ProcessingError, SdComponent, SdComponentMetadata, SourceFile,
    deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

pub struct UrlDownloaderSupplier;
pub const SUPPLIER: UrlDownloaderSupplier = UrlDownloaderSupplier;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct UrlDownloaderConfig {
    download_path: String,
}

impl ComponentSupplier for UrlDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("url".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: UrlDownloaderConfig = deserialize_component_config(props)?;
        Ok(Arc::new(UrlDownloader { download_path: config.download_path }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(Downloader)]
pub struct UrlDownloader {
    download_path: String,
}

impl Display for UrlDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("url")
    }
}

#[async_trait]
impl Downloader for UrlDownloader {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
        for file in task.download_files {
            if let Some(parent) = file.path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        let body = reqwest::get(task.source_item.download_uri.to_string())
            .await
            .map_err(|error| ProcessingError::retryable(error.to_string()))?
            .error_for_status()
            .map_err(|error| ProcessingError::retryable(error.to_string()))?
            .bytes()
            .await
            .map_err(|error| ProcessingError::retryable(error.to_string()))?;

        for (index, file) in task.download_files.iter().enumerate() {
            let mut target = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(file.path)
                .await?;
            target.seek(std::io::SeekFrom::Start(0)).await?;
            if index == 0 {
                target.write_all(&body).await?;
            }
            target.flush().await?;
        }
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

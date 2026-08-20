use serde::{Deserialize, Deserializer};
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader,
    ProcessingError, SdComponent, SdComponentMetadata, SourceFile,
    deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct NoneDownloaderSupplier;
pub const SUPPLIER: NoneDownloaderSupplier = NoneDownloaderSupplier;

fn deserialize_optional_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NoneDownloaderConfig {
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    download_path: Option<String>,
}

impl ComponentSupplier for NoneDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("none".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<NoneDownloaderConfig>(props)?;
        let path = match config.download_path {
            None => std::env::current_dir().map_err(|error| {
                ComponentError::new(format!("Failed to get current directory: {error}"))
            })?,
            Some(path) => PathBuf::from(path),
        };
        Ok(Arc::new(NoneDownloader {
            download_path: path.to_string_lossy().into_owned(),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description:
                "Discards downloads while exposing a configured destination path."
                    .to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "downloadPath":{"type":"string"}
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
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

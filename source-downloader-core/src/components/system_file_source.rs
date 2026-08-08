use async_trait::async_trait;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader,
    EmptyPointer, PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source,
    SourceFile, SourcePointer, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::time::OffsetDateTime;
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use url::Url;
use walkdir::WalkDir;

pub struct SystemFileSourceSupplier;
pub const SUPPLIER: SystemFileSourceSupplier = SystemFileSourceSupplier {};

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SystemFileSourceConfig {
    path: String,
    #[serde(default)]
    mode: i64,
}

impl ComponentSupplier for SystemFileSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::source("system-file".to_owned()),
            ComponentType::downloader("system-file".to_owned()),
        ]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<SystemFileSourceConfig>(props)?;
        if config.mode != 0 && config.mode != 1 {
            return Err(ComponentError::new(format!(
                "Invalid configuration at 'mode': Unknown mode: {}",
                config.mode
            )));
        }
        let path = PathBuf::from(config.path);
        Ok(Arc::new(SystemFileSource {
            download_path: path.to_string_lossy().into_owned(),
            path,
            mode: config.mode as i8,
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(Source, Downloader)]
struct SystemFileSource {
    path: PathBuf,
    download_path: String,
    mode: i8,
}

impl Display for SystemFileSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("system-file")
    }
}

#[async_trait]
impl Source for SystemFileSource {
    async fn fetch<'pointer>(
        &self,
        _: &'pointer dyn SourcePointer,
        _: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        match self.mode {
            0 => self.create_root_file_source_items(),
            1 => self.create_each_file_source_items(),
            mode => Err(ProcessingError::non_retryable(format!("Unknown mode: {mode}"))),
        }
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(EmptyPointer)
    }

    fn parse_raw_pointer(&self, _: Value) -> Box<dyn SourcePointer> {
        Box::new(EmptyPointer)
    }
}

#[async_trait]
impl Downloader for SystemFileSource {
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

impl SystemFileSource {
    fn create_each_file_source_items(&self) -> Result<Vec<PointedItem>, ProcessingError> {
        WalkDir::new(&self.path)
            .into_iter()
            .filter_map(|entry| match entry {
                Ok(entry) if !is_hidden(entry.path()) => match fs::metadata(entry.path())
                {
                    Ok(metadata) if !metadata.is_dir() => {
                        Some(Self::from_path(entry.path()))
                    }
                    Ok(_) => None,
                    Err(error) => {
                        Some(Err(ProcessingError::non_retryable(error.to_string())))
                    }
                },
                Ok(_) => None,
                Err(error) => {
                    Some(Err(ProcessingError::non_retryable(error.to_string())))
                }
            })
            .collect()
    }

    fn create_root_file_source_items(&self) -> Result<Vec<PointedItem>, ProcessingError> {
        fs::read_dir(&self.path)
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?
            .filter_map(|entry| match entry {
                Ok(entry) if !is_hidden(&entry.path()) => {
                    Some(Self::from_path(&entry.path()))
                }
                Ok(_) => None,
                Err(error) => {
                    Some(Err(ProcessingError::non_retryable(error.to_string())))
                }
            })
            .collect()
    }

    fn from_path(path: &Path) -> Result<PointedItem, ProcessingError> {
        let metadata = fs::metadata(path)?;
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let absolute_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let uri = Url::from_file_path(absolute_path)
            .map_err(|_| ProcessingError::non_retryable("Failed to create file URI"))?;
        let uri: source_downloader_sdk::http::Uri =
            uri.as_str().parse().map_err(|error| {
                ProcessingError::non_retryable(format!("Invalid file URI: {error}"))
            })?;
        let datetime = metadata
            .created()
            .ok()
            .and_then(|created| created.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|duration| {
                OffsetDateTime::from_unix_timestamp_nanos(duration.as_nanos() as i128)
                    .ok()
            })
            .unwrap_or_else(OffsetDateTime::now_utc);
        let content_type = if metadata.is_dir() { "directory" } else { "file" };
        let attrs = Map::from_iter([(String::from("size"), Value::from(metadata.len()))]);
        Ok(PointedItem {
            source_item: SourceItem {
                title,
                link: uri.clone(),
                datetime,
                content_type: content_type.to_owned(),
                download_uri: uri,
                attrs,
                tags: Vec::new(),
                identity: None,
            },
            item_pointer: Arc::new(EmptyPointer),
        })
    }
}

#[cfg(windows)]
fn is_hidden(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;

    fs::metadata(path)
        .map(|metadata| metadata.file_attributes() & 0x2 != 0)
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

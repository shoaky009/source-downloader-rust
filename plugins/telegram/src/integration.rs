use crate::client::TelegramClientInstance;
use crate::source::MEDIA_TYPE_ATTR;
use futures_util::future::{AbortHandle, Abortable};
use parking_lot::Mutex;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentCreateContext, ComponentError, ComponentSupplier, ComponentType,
    DownloadTask, Downloader, ItemFileResolver, ProcessingError, SdComponent,
    SdComponentMetadata, SourceFile, Stateful, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::any::TypeId;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::io::AsyncWriteExt;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct TelegramIntegrationConfig {
    client: String,
    download_path: String,
}

pub struct TelegramIntegrationSupplier;
pub const INTEGRATION_SUPPLIER: TelegramIntegrationSupplier = TelegramIntegrationSupplier;

impl ComponentSupplier for TelegramIntegrationSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::file_resolver("telegram".into()),
            ComponentType::downloader("telegram".into()),
        ]
    }

    fn apply(
        &self,
        context: &dyn ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: TelegramIntegrationConfig = deserialize_component_config(props)?;
        let instance = context
            .get_instance(&config.client, TypeId::of::<TelegramClientInstance>())?;
        let client = instance.downcast::<TelegramClientInstance>().map_err(|_| {
            ComponentError::new(format!(
                "Telegram instance '{}' has an incompatible type",
                config.client
            ))
        })?;
        Ok(Arc::new(TelegramIntegration {
            client,
            download_path: config.download_path,
            downloads: Mutex::new(HashMap::new()),
            downloaded: AtomicU64::new(0),
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

struct DownloadState {
    abort_handle: AbortHandle,
    total: u64,
    downloaded: Arc<AtomicU64>,
    started_at: Instant,
}

struct ActiveDownload<'a> {
    downloads: &'a Mutex<HashMap<PathBuf, DownloadState>>,
    target: PathBuf,
    temporary: PathBuf,
}

impl Drop for ActiveDownload<'_> {
    fn drop(&mut self) {
        self.downloads.lock().remove(&self.target);
        if let Err(error) = std::fs::remove_file(&self.temporary)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                %error,
                path = %self.temporary.display(),
                "Failed to remove Telegram temporary file"
            );
        }
    }
}

#[derive(source_downloader_sdk::SdComponent)]
#[component(ItemFileResolver, Downloader, Stateful)]
struct TelegramIntegration {
    client: Arc<TelegramClientInstance>,
    download_path: String,
    downloads: Mutex<HashMap<PathBuf, DownloadState>>,
    downloaded: AtomicU64,
}

impl std::fmt::Debug for TelegramIntegration {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramIntegration")
            .field("download_path", &self.download_path)
            .field("active_downloads", &self.downloads.lock().len())
            .field("downloaded", &self.downloaded.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Display for TelegramIntegration {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("telegram")
    }
}

#[async_trait]
impl ItemFileResolver for TelegramIntegration {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        if item.content_type == "message" {
            let (chat_id, message_id) = telegram_target(item)?;
            let (peer, _) = self.client.chat(chat_id).await?;
            let client = self.client.client().await?;
            let message = client
                .get_messages_by_id(peer, &[message_id])
                .await
                .map_err(crate::client::telegram_error)?
                .into_iter()
                .next()
                .flatten()
                .ok_or_else(|| {
                    ProcessingError::non_retryable(format!(
                        "Telegram message {chat_id}/{message_id} was not found"
                    ))
                })?;
            let mut file = SourceFile::new(PathBuf::from(format!("{}.md", item.title)));
            file.data = Some(Arc::from(message.markdown_text().into_bytes()));
            return Ok(vec![file]);
        }
        if item.attrs.get("site").and_then(Value::as_str) == Some("Telegraph") {
            return Ok(Vec::new());
        }
        let mut file = SourceFile::new(PathBuf::from(&item.title));
        file.download_uri = Some(item.download_uri.clone());
        if let Some(media_type) = item.attrs.get(MEDIA_TYPE_ATTR) {
            file.attrs.insert(MEDIA_TYPE_ATTR.into(), media_type.clone());
        }
        Ok(vec![file])
    }
}

#[async_trait]
impl Downloader for TelegramIntegration {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
        let (chat_id, message_id) = telegram_target(task.source_item)?;
        let target =
            task.download_files.first().map(|file| file.path.to_path_buf()).ok_or_else(
                || ProcessingError::non_retryable("Telegram download has no file"),
            )?;
        let (peer, _) = self.client.chat(chat_id).await?;
        let client = self.client.client().await?;
        let message = client
            .get_messages_by_id(peer, &[message_id])
            .await
            .map_err(crate::client::telegram_error)?
            .into_iter()
            .next()
            .flatten()
            .ok_or_else(|| {
                ProcessingError::non_retryable(format!(
                    "Telegram message {chat_id}/{message_id} was not found"
                ))
            })?;
        let media = message.media().ok_or_else(|| {
            ProcessingError::non_retryable(format!(
                "Telegram message {chat_id}/{message_id} has no downloadable media"
            ))
        })?;
        let total = media.size().unwrap_or_default() as u64;
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let temporary = temporary_path(&target);
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let downloaded = Arc::new(AtomicU64::new(0));
        {
            let mut downloads = self.downloads.lock();
            if downloads.contains_key(&target) {
                return Err(ProcessingError::non_retryable(format!(
                    "Telegram file is already downloading: {}",
                    target.display()
                )));
            }
            downloads.insert(
                target.clone(),
                DownloadState {
                    abort_handle,
                    total,
                    downloaded: downloaded.clone(),
                    started_at: Instant::now(),
                },
            );
        }
        let active_download =
            ActiveDownload { downloads: &self.downloads, target, temporary };

        let result = Abortable::new(
            download_media(&client, &media, &active_download.temporary, downloaded),
            abort_registration,
        )
        .await
        .map_err(|_| {
            ProcessingError::non_retryable(format!(
                "Telegram download cancelled: {}",
                active_download.target.display()
            ))
        })
        .and_then(|result| result);
        match result {
            Ok(()) => {
                tokio::fs::rename(&active_download.temporary, &active_download.target)
                    .await?;
                self.downloaded.fetch_add(1, Ordering::Relaxed);
                tracing::info!(
                    path = %active_download.target.display(),
                    "Telegram file downloaded"
                );
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn default_download_path(&self) -> &str {
        &self.download_path
    }

    async fn cancel(
        &self,
        _: &SourceItem,
        files: &[SourceFile],
    ) -> Result<(), ProcessingError> {
        let downloads = self.downloads.lock();
        for file in files {
            if let Some(state) = downloads.get(&file.path) {
                state.abort_handle.abort();
            }
        }
        Ok(())
    }
}

impl Stateful for TelegramIntegration {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        let downloading = self
            .downloads
            .lock()
            .iter()
            .map(|(path, state)| {
                let downloaded = state.downloaded.load(Ordering::Relaxed);
                let elapsed = state.started_at.elapsed();
                let rate = if elapsed.is_zero() {
                    0.0
                } else {
                    downloaded as f64 / elapsed.as_secs_f64()
                };
                let progress = if state.total == 0 {
                    0.0
                } else {
                    (downloaded as f64 * 10_000.0 / state.total as f64).round() / 100.0
                };
                source_downloader_sdk::serde_json::json!({
                    "path": path.to_string_lossy(),
                    "totalSize": state.total,
                    "downloadedSize": downloaded,
                    "progress": progress,
                    "rate": readable_rate(rate),
                    "duration": elapsed.as_secs(),
                })
            })
            .collect::<Vec<_>>();
        Some(Map::from_iter([
            ("downloaded".into(), Value::from(self.downloaded.load(Ordering::Relaxed))),
            ("downloading".into(), Value::Array(downloading)),
        ]))
    }
}

async fn download_media(
    client: &grammers_client::Client,
    media: &grammers_client::media::Media,
    path: &Path,
    downloaded: Arc<AtomicU64>,
) -> Result<(), ProcessingError> {
    let mut output = tokio::fs::File::create(path).await?;
    let mut parts = client.iter_download(media);
    while let Some(bytes) = parts.next().await.map_err(crate::client::telegram_error)? {
        output.write_all(&bytes).await?;
        downloaded.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    }
    output.flush().await?;
    Ok(())
}

fn readable_rate(rate: f64) -> String {
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
    let mut value = rate;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn telegram_target(item: &SourceItem) -> Result<(i64, i32), ProcessingError> {
    let query = item.download_uri.query().ok_or_else(|| {
        ProcessingError::non_retryable(format!(
            "Invalid Telegram download URI: {}",
            item.download_uri
        ))
    })?;
    let values = url::form_urlencoded::parse(query.as_bytes()).collect::<HashMap<_, _>>();
    let chat_id = values
        .get("channel")
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| ProcessingError::non_retryable("Telegram channel is missing"))?;
    let message_id = values
        .get("post")
        .and_then(|value| value.parse::<i32>().ok())
        .ok_or_else(|| ProcessingError::non_retryable("Telegram post is missing"))?;
    Ok((chat_id, message_id))
}

fn temporary_path(target: &Path) -> PathBuf {
    let name = target.file_name().and_then(|name| name.to_str()).unwrap_or("download");
    target.with_file_name(format!("{name}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};

    #[test]
    fn parses_private_post_target() {
        let item = SourceItem {
            title: "file".into(),
            link: Uri::from_static("tg://privatepost?channel=7&post=2"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: "application/octet-stream".into(),
            download_uri: Uri::from_static("tg://privatepost?channel=-7&post=42"),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        };
        assert_eq!(telegram_target(&item).unwrap(), (-7, 42));
    }
}

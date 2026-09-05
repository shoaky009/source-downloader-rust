use crate::client::TelegramClientInstance;
use crate::source::MEDIA_TYPE_ATTR;
use futures_util::future::{AbortHandle, Abortable};
use futures_util::{Stream, StreamExt};
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
use std::time::{Duration, Instant};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

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
        Some(Box::new(SdComponentMetadata {
            description: "Resolves and downloads Telegram media.".into(),
            props_json_schema: Some(source_downloader_sdk::serde_json::json!({
                "type": "object",
                "properties": {
                    "client": {"type": "string"},
                    "download-path": {"type": "string"}
                },
                "required": ["client", "download-path"]
            })),
            props_ui_schema: Some(source_downloader_sdk::serde_json::json!({
                "client": {
                    "ui:field": "instanceField",
                    "ui:options": {"factoryType": std::any::type_name::<crate::client::TelegramClientInstanceFactory>()}
                }
            })),
            state_json_schema: Some(source_downloader_sdk::serde_json::json!({
                "type": "object",
                "properties": {
                    "downloaded": {"type": "integer", "minimum": 0},
                    "downloading": {"type": "array", "items": {"type": "object", "properties": {
                        "path": {"type": "string"},
                        "totalSize": {"type": "integer", "minimum": 0},
                        "downloadedSize": {"type": "integer", "minimum": 0},
                        "progress": {"type": "number", "minimum": 0},
                        "rate": {"type": "string"},
                        "duration": {"type": "integer", "minimum": 0}
                    }, "required": ["path", "totalSize", "downloadedSize", "progress", "rate", "duration"]}}
                },
                "required": ["downloaded", "downloading"]
            })),
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

struct DownloadState {
    abort_handle: AbortHandle,
    total: u64,
    downloaded: Arc<AtomicU64>,
    transferred: Arc<AtomicU64>,
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
        let transferred = Arc::new(AtomicU64::new(0));
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
                    transferred: transferred.clone(),
                    started_at: Instant::now(),
                },
            );
        }
        let active_download =
            ActiveDownload { downloads: &self.downloads, target, temporary };

        let result = Abortable::new(
            download_media(
                &client,
                &media,
                &active_download.temporary,
                &downloaded,
                &transferred,
            ),
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
                    state.transferred.load(Ordering::Relaxed) as f64
                        / elapsed.as_secs_f64()
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

const DOWNLOAD_CHUNK_SIZE: u64 = 512 * 1024;

async fn download_media(
    client: &grammers_client::Client,
    media: &grammers_client::media::Media,
    path: &Path,
    downloaded: &AtomicU64,
    transferred: &AtomicU64,
) -> Result<(), ProcessingError> {
    download_parts(
        path,
        media.size().map(|size| size as u64),
        downloaded,
        transferred,
        |offset| {
            let parts = client
                .iter_download(media)
                .chunk_size(DOWNLOAD_CHUNK_SIZE as i32)
                .skip_chunks((offset / DOWNLOAD_CHUNK_SIZE) as i32);
            futures_util::stream::try_unfold(parts, |mut parts| async move {
                parts.next().await.map(|part| part.map(|bytes| (bytes, parts)))
            })
        },
    )
    .await
}

async fn download_parts<S>(
    path: &Path,
    total: Option<u64>,
    downloaded: &AtomicU64,
    transferred: &AtomicU64,
    mut parts_at: impl FnMut(u64) -> S,
) -> Result<(), ProcessingError>
where
    S: Stream<Item = Result<Vec<u8>, grammers_client::InvocationError>>,
{
    let mut output = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
        .await?;
    let mut offset = output.metadata().await?.len();
    if total.is_some_and(|total| offset > total) {
        offset = 0;
        output.set_len(0).await?;
    }
    loop {
        if total == Some(offset) {
            downloaded.store(offset, Ordering::Relaxed);
            return Ok(());
        }
        // Telegram offsets must be aligned; replay an incomplete last chunk.
        offset -= offset % DOWNLOAD_CHUNK_SIZE;
        let chunks = offset / DOWNLOAD_CHUNK_SIZE;
        if i32::try_from(chunks).is_err() {
            return Err(ProcessingError::non_retryable(
                "Telegram resume offset is too large",
            ));
        }
        output.set_len(offset).await?;
        output.seek(std::io::SeekFrom::Start(offset)).await?;
        downloaded.store(offset, Ordering::Relaxed);
        let parts = parts_at(offset);
        futures_util::pin_mut!(parts);
        let retry_delay = loop {
            match parts.next().await {
                Some(Ok(bytes)) => {
                    output.write_all(&bytes).await?;
                    // Finish pending Tokio file writes before publishing the checkpoint.
                    output.flush().await?;
                    offset += bytes.len() as u64;
                    downloaded.store(offset, Ordering::Relaxed);
                    transferred.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                }
                Some(Err(error)) => match download_retry_delay(&error) {
                    Some(delay) => {
                        tracing::warn!(%error, offset, "Telegram download interrupted; resuming");
                        break delay;
                    }
                    None => return Err(crate::client::telegram_error(error)),
                },
                None => {
                    if total.is_some_and(|total| total != offset) {
                        return Err(ProcessingError::retryable(format!(
                            "Telegram download incomplete: expected {} bytes, received {offset}",
                            total.unwrap_or_default()
                        )));
                    }
                    return Ok(());
                }
            }
        };
        tokio::time::sleep(retry_delay).await;
    }
}

fn download_retry_delay(error: &grammers_client::InvocationError) -> Option<Duration> {
    let grammers_client::InvocationError::Rpc(rpc) = error else {
        return None;
    };
    match rpc.code {
        -503 => Some(Duration::from_secs(5)),
        420 => Some(Duration::from_secs(if rpc.name == "FLOOD_WAIT" {
            rpc.value.map(|value| u64::from(value) + 3).unwrap_or(5)
        } else {
            5
        })),
        _ => None,
    }
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

    fn rpc_error(
        code: i32,
        name: &str,
        value: Option<u32>,
    ) -> grammers_client::InvocationError {
        grammers_client::InvocationError::Rpc(
            grammers_client::tl::types::RpcError {
                error_code: code,
                error_message: match value {
                    Some(value) => format!("{name}_{value}"),
                    None => name.into(),
                },
            }
            .into(),
        )
    }

    #[tokio::test]
    async fn resumes_after_error_without_redownloading_complete_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("media.tmp");
        let prefix = vec![1; DOWNLOAD_CHUNK_SIZE as usize];
        let downloaded = AtomicU64::new(0);
        let transferred = AtomicU64::new(0);
        let total = DOWNLOAD_CHUNK_SIZE + 3;
        let result =
            download_parts(&path, Some(total), &downloaded, &transferred, |offset| {
                assert_eq!(offset, 0);
                futures_util::stream::iter(vec![
                    Ok(prefix.clone()),
                    Err(rpc_error(500, "INTERNAL", None)),
                ])
            })
            .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), prefix);
        // Simulate a partial write at interruption; only this tail is replayed.
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, &[9, 9]).unwrap();
        drop(file);
        transferred.store(0, Ordering::Relaxed);
        download_parts(&path, Some(total), &downloaded, &transferred, |offset| {
            assert_eq!(offset, DOWNLOAD_CHUNK_SIZE);
            futures_util::stream::iter(vec![Ok(vec![2, 3, 4])])
        })
        .await
        .unwrap();
        let mut expected = prefix;
        expected.extend_from_slice(&[2, 3, 4]);
        assert_eq!(std::fs::read(path).unwrap(), expected);
        assert_eq!(downloaded.load(Ordering::Relaxed), total);
        assert_eq!(transferred.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn retries_timeout_from_saved_offset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("media.tmp");
        let downloaded = AtomicU64::new(0);
        let transferred = AtomicU64::new(0);
        let mut attempts = 0;
        download_parts(
            &path,
            Some(DOWNLOAD_CHUNK_SIZE + 1),
            &downloaded,
            &transferred,
            |offset| {
                attempts += 1;
                futures_util::stream::iter(if attempts == 1 {
                    assert_eq!(offset, 0);
                    vec![
                        Ok(vec![7; DOWNLOAD_CHUNK_SIZE as usize]),
                        Err(rpc_error(-503, "TIMEOUT", None)),
                    ]
                } else {
                    assert_eq!(offset, DOWNLOAD_CHUNK_SIZE);
                    vec![Ok(vec![8])]
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(attempts, 2);
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len(), DOWNLOAD_CHUNK_SIZE as usize + 1);
        assert!(bytes[..bytes.len() - 1].iter().all(|byte| *byte == 7));
        assert_eq!(bytes.last(), Some(&8));
    }

    #[tokio::test]
    async fn completed_partial_file_needs_no_download() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("media.tmp");
        std::fs::write(&path, b"complete").unwrap();
        let downloaded = AtomicU64::new(0);
        let transferred = AtomicU64::new(0);
        let mut requested = false;
        download_parts(&path, Some(8), &downloaded, &transferred, |_| {
            requested = true;
            futures_util::stream::empty()
        })
        .await
        .unwrap();
        assert!(!requested);
        assert_eq!(std::fs::read(path).unwrap(), b"complete");
        assert_eq!(downloaded.load(Ordering::Relaxed), 8);
        assert_eq!(transferred.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn rejects_early_eof_and_retains_written_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("media.tmp");
        let downloaded = AtomicU64::new(0);
        let transferred = AtomicU64::new(0);
        let result = download_parts(&path, Some(10), &downloaded, &transferred, |_| {
            futures_util::stream::iter(vec![Ok(vec![1, 2, 3])])
        })
        .await;
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), [1, 2, 3]);
    }

    #[tokio::test]
    async fn cancellation_interrupts_retry_wait_and_preserves_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("media.tmp");
        let downloaded = AtomicU64::new(0);
        let transferred = AtomicU64::new(0);
        let (abort, registration) = AbortHandle::new_pair();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            Abortable::new(
                download_parts(
                    &path,
                    Some(DOWNLOAD_CHUNK_SIZE + 1),
                    &downloaded,
                    &transferred,
                    |_| {
                        let abort = &abort;
                        async_stream::stream! {
                            yield Ok(vec![7; DOWNLOAD_CHUNK_SIZE as usize]);
                            abort.abort();
                            yield Err(rpc_error(420, "FLOOD_WAIT", Some(100)));
                        }
                    },
                ),
                registration,
            ),
        )
        .await
        .unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read(path).unwrap(), vec![7; DOWNLOAD_CHUNK_SIZE as usize]);
    }

    #[test]
    fn retry_delays_match_telegram_errors() {
        assert_eq!(
            download_retry_delay(&rpc_error(-503, "TIMEOUT", None)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            download_retry_delay(&rpc_error(420, "FLOOD_WAIT", Some(17))),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            download_retry_delay(&rpc_error(420, "FLOOD_WAIT", None)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            download_retry_delay(&rpc_error(400, "FILE_REFERENCE_EXPIRED", None)),
            None
        );
    }
    #[test]
    fn failed_download_keeps_partial_file_and_releases_target() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("media.bin");
        let temporary = temporary_path(&target);
        std::fs::write(&temporary, b"downloaded prefix").unwrap();
        let (abort_handle, _) = AbortHandle::new_pair();
        let downloads = Mutex::new(HashMap::from([(
            target.clone(),
            DownloadState {
                abort_handle,
                total: 100,
                downloaded: Arc::new(AtomicU64::new(17)),
                transferred: Arc::new(AtomicU64::new(0)),
                started_at: Instant::now(),
            },
        )]));
        drop(ActiveDownload {
            downloads: &downloads,
            target: target.clone(),
            temporary: temporary.clone(),
        });
        assert!(downloads.lock().is_empty());
        assert_eq!(std::fs::read(temporary).unwrap(), b"downloaded prefix");
        assert!(!target.exists());
    }

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

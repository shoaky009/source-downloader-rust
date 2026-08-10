use async_trait::async_trait;
use futures_util::future::{AbortHandle, Abortable};
use futures_util::stream::{self, StreamExt};
use parking_lot::Mutex;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader,
    ProcessingError, SdComponent, SdComponentMetadata, SourceFile, SourceFileRef,
    Stateful, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;
use tokio::io::AsyncWriteExt;

pub struct HttpDownloaderSupplier;
pub const SUPPLIER: HttpDownloaderSupplier = HttpDownloaderSupplier {};

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct HttpDownloaderConfig {
    download_path: String,
    #[serde(default = "default_parallelism")]
    parallelism: usize,
}

fn default_parallelism() -> usize {
    5
}

impl ComponentSupplier for HttpDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("http".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<HttpDownloaderConfig>(props)?;
        if config.parallelism == 0 {
            return Err(ComponentError::new(
                "Invalid configuration at 'parallelism': HTTP downloader parallelism must be greater than zero",
            ));
        }
        let client = reqwest::Client::builder().build().map_err(|error| {
            ComponentError::new(format!("Failed to build HTTP client: {error}"))
        })?;
        Ok(Arc::new(HttpDownloader {
            path: config.download_path,
            client,
            parallelism: config.parallelism,
            downloads: Mutex::new(HashMap::new()),
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Downloads source files over HTTP with bounded parallelism."
                .to_owned(),
            props_json_schema: Some(
                json!({"type":"object","properties":{"download-path":{"type":"string"},"parallelism":{"type":"integer","minimum":1,"default":5}},"required":["download-path"]}),
            ),
            props_ui_schema: None,
            state_json_schema: Some(
                json!({"type":"object","additionalProperties":{"type":"object","properties":{"file":{"type":"string"},"speed":{"type":"string"}},"required":["file","speed"]}}),
            ),
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(SdComponent, Debug)]
#[component(Downloader, Stateful)]
struct HttpDownloader {
    path: String,
    client: reqwest::Client,
    parallelism: usize,
    downloads: Mutex<HashMap<PathBuf, DownloadState>>,
}

#[derive(Debug)]
struct DownloadState {
    abort_handle: AbortHandle,
    downloaded_bytes: Arc<AtomicU64>,
    started_at: Instant,
}

impl Display for HttpDownloader {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("http")
    }
}

impl HttpDownloader {
    async fn download(
        &self,
        file: &SourceFileRef<'_>,
        headers: Option<&HashMap<&String, &String>>,
    ) -> Result<(), ProcessingError> {
        let Some(uri) = file.download_uri else {
            return Ok(());
        };
        let path = file.path.to_path_buf();
        if let Some(parent) = path.parent()
            && parent != Path::new(&self.path)
            && !parent.exists()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        let downloaded_bytes = Arc::new(AtomicU64::new(0));
        {
            let mut downloads = self.downloads.lock();
            if downloads.contains_key(&path) {
                return Err(ProcessingError::non_retryable(format!(
                    "File already downloading: {}",
                    path.display()
                )));
            }
            downloads.insert(
                path.clone(),
                DownloadState {
                    abort_handle,
                    downloaded_bytes: Arc::clone(&downloaded_bytes),
                    started_at: Instant::now(),
                },
            );
        }

        let result = Abortable::new(
            self.download_response(uri.to_string(), &path, headers, downloaded_bytes),
            abort_registration,
        )
        .await
        .map_err(|_| {
            ProcessingError::non_retryable(format!(
                "HTTP download cancelled: {}",
                path.display()
            ))
        })
        .and_then(|result| result);
        self.downloads.lock().remove(&path);
        if let Err(download_error) = &result {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(ProcessingError::non_retryable(format!(
                        "{}; failed to remove partial file {}: {error}",
                        download_error.message(),
                        path.display()
                    )));
                }
            }
        }
        result
    }

    async fn download_response(
        &self,
        uri: String,
        path: &Path,
        headers: Option<&HashMap<&String, &String>>,
        downloaded_bytes: Arc<AtomicU64>,
    ) -> Result<(), ProcessingError> {
        let mut request = self.client.get(&uri);
        if let Some(headers) = headers {
            for (name, value) in headers {
                request = request.header(name.as_str(), value.as_str());
            }
        }
        let mut response = request.send().await.map_err(|error| {
            ProcessingError::retryable(format!("Failed to download {uri}: {error}"))
        })?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(ProcessingError::skip(format!(
                "Failed to download status code {status}: {uri}"
            )));
        }
        if status.is_client_error() {
            return Err(ProcessingError::non_retryable(format!(
                "Failed to download status code {status}: {uri}"
            )));
        }
        if status.is_server_error() {
            return Err(ProcessingError::non_retryable(format!(
                "Failed to download status code {status}: {uri}"
            )));
        }

        let mut target = tokio::fs::File::create(path).await?;
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            ProcessingError::retryable(format!("Failed while downloading {uri}: {error}"))
        })? {
            downloaded_bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            target.write_all(&chunk).await?;
        }
        target.flush().await?;
        Ok(())
    }
}

#[async_trait]
impl Downloader for HttpDownloader {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
        let downloads = task
            .download_files
            .iter()
            .map(|file| self.download(file, task.headers.as_ref()))
            .collect::<Vec<_>>();
        let results = stream::iter(downloads)
            .buffer_unordered(self.parallelism)
            .collect::<Vec<_>>()
            .await;
        results.into_iter().collect()
    }

    fn default_download_path(&self) -> &str {
        &self.path
    }

    async fn cancel(
        &self,
        _: &SourceItem,
        files: &[SourceFile],
    ) -> Result<(), ProcessingError> {
        for file in files {
            if let Some(download) = self.downloads.lock().remove(&file.path) {
                download.abort_handle.abort();
            }
        }
        Ok(())
    }
}

impl Stateful for HttpDownloader {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        let mut state = Map::new();
        for (path, download) in self.downloads.lock().iter() {
            let elapsed_seconds = download.started_at.elapsed().as_secs();
            let downloaded = download.downloaded_bytes.load(Ordering::Relaxed);
            let speed = downloaded.checked_div(elapsed_seconds).unwrap_or(downloaded);
            state.insert(
                path.to_string_lossy().into_owned(),
                serde_json::json!({
                    "file": path.to_string_lossy(),
                    "speed": readable_rate(speed),
                }),
            );
        }
        Some(state)
    }
}

fn readable_rate(rate: u64) -> String {
    const KILOBYTE: f64 = 1024.0;
    const MEGABYTE: f64 = KILOBYTE * 1024.0;
    const GIGABYTE: f64 = MEGABYTE * 1024.0;
    let rate_as_float = rate as f64;
    if rate_as_float > GIGABYTE {
        format!("{:.2} GiB/s", rate_as_float / GIGABYTE)
    } else if rate_as_float > MEGABYTE {
        format!("{:.2} MiB/s", rate_as_float / MEGABYTE)
    } else if rate_as_float > KILOBYTE {
        format!("{:.2} KiB/s", rate_as_float / KILOBYTE)
    } else {
        format!("{rate} B/s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::component::SourceFileRef;
    use source_downloader_sdk::http::Uri;
    use std::collections::HashMap;
    use std::path::Path;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn serve_once(
        response: &'static [u8],
    ) -> (Uri, tokio::sync::oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let size = stream.read(&mut request).await.unwrap();
            request.truncate(size);
            let _ = request_tx.send(String::from_utf8(request).unwrap());
            stream.write_all(response).await.unwrap();
        });
        (format!("http://{address}/file").parse().unwrap(), request_rx)
    }

    async fn serve_slow() -> (Uri, tokio::sync::oneshot::Receiver<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000000\r\n\r\npartial")
                .await;
            let _ = started_tx.send(());
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
        (format!("http://{address}/slow").parse().unwrap(), started_rx)
    }

    fn create_downloader(path: &Path) -> Arc<dyn Downloader> {
        Arc::new(HttpDownloader {
            path: path.to_string_lossy().into_owned(),
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            parallelism: 2,
            downloads: Mutex::new(HashMap::new()),
        })
    }

    #[tokio::test]
    async fn downloads_files_with_configured_headers() {
        let (uri, request) =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\npayload").await;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("nested/file.bin");
        let mut source_file = SourceFile::new(target.clone());
        source_file.download_uri = Some(uri);
        let file_ref = SourceFileRef::from(&source_file);
        let headers = HashMap::from([("X-Test".to_owned(), "present".to_owned())]);
        let task = DownloadTask {
            source_item: &SourceItem::default(),
            download_files: &[file_ref],
            download_path: Path::new(directory.path()),
            category: &None,
            tags: None,
            headers: Some(headers.iter().collect()),
        };
        let downloader = create_downloader(directory.path());

        downloader.submit(&task).await.unwrap();

        assert_eq!(b"payload", tokio::fs::read(target).await.unwrap().as_slice());
        assert!(request.await.unwrap().to_ascii_lowercase().contains("x-test: present"));
    }

    #[tokio::test]
    async fn not_found_is_skippable_and_leaves_no_file() {
        let (uri, _) =
            serve_once(b"HTTP/1.1 404 Not Found\r\nContent-Length: 3\r\n\r\n404").await;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("missing.bin");
        let mut source_file = SourceFile::new(target.clone());
        source_file.download_uri = Some(uri);
        let file_ref = SourceFileRef::from(&source_file);
        let item = SourceItem::default();
        let category = None;
        let files = [file_ref];
        let task = DownloadTask {
            source_item: &item,
            download_files: &files,
            download_path: directory.path(),
            category: &category,
            tags: None,
            headers: None,
        };

        let error = create_downloader(directory.path()).submit(&task).await.unwrap_err();

        assert!(matches!(error, ProcessingError::NonRetryable { skip: true, .. }));
        assert!(!target.exists());
    }

    #[tokio::test]
    async fn cancel_aborts_download_and_removes_partial_file() {
        let (uri, started) = serve_slow().await;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("cancelled.bin");
        let mut source_file = SourceFile::new(target.clone());
        source_file.download_uri = Some(uri);
        let file_ref = SourceFileRef::from(&source_file);
        let item = SourceItem::default();
        let category = None;
        let files = [file_ref];
        let task = DownloadTask {
            source_item: &item,
            download_files: &files,
            download_path: directory.path(),
            category: &category,
            tags: None,
            headers: None,
        };
        let downloader = create_downloader(directory.path());
        let cancel_files = [source_file.clone()];

        let (submit_result, cancel_result) =
            tokio::join!(downloader.submit(&task), async {
                started.await.unwrap();
                downloader.cancel(&item, &cancel_files).await
            });

        assert!(submit_result.unwrap_err().message().contains("cancelled"));
        cancel_result.unwrap();
        assert!(!target.exists());
    }
    #[test]
    fn readable_rate_uses_human_readable_units() {
        assert_eq!(readable_rate(1024), "1024 B/s");
        assert_eq!(readable_rate(1025), "1.00 KiB/s");
        assert_eq!(readable_rate(1024 * 1024 + 1), "1.00 MiB/s");
    }

    #[test]
    fn supplier_rejects_zero_parallelism() {
        let props = serde_json::json!({
            "download-path": "downloads",
            "parallelism": 0
        })
        .as_object()
        .unwrap()
        .clone();

        let error = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid configuration at 'parallelism': HTTP downloader parallelism must be greater than zero"
        );
    }
}

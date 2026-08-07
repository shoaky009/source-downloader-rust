use crate::http;
use parking_lot::Mutex;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    AsyncDownloader, ComponentError, ComponentSupplier, ComponentType, DownloadTask,
    Downloader, ProcessingError, SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub struct QbittorrentDownloaderSupplier;
pub const SUPPLIER: QbittorrentDownloaderSupplier = QbittorrentDownloaderSupplier;

impl ComponentSupplier for QbittorrentDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("qbittorrent".to_string())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let endpoint = props
            .get("endpoint")
            .or_else(|| props.get("host"))
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::new("Missing or invalid 'endpoint' property"))?
            .trim_end_matches('/')
            .to_string();
        let username = optional_string(props, "username")?;
        let password = optional_string(props, "password")?;
        let always_download_all = props
            .get("always-download-all")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    ComponentError::new("Invalid 'always-download-all' property")
                })
            })
            .transpose()?
            .unwrap_or(false);
        let client = if endpoint.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|error| ComponentError::new(error.to_string()))?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(QbittorrentDownloader {
            client,
            endpoint,
            username,
            password,
            logged_in: Mutex::new(false),
            default_path: OnceLock::new(),
            always_download_all,
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

fn optional_string(
    props: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ComponentError> {
    props
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| ComponentError::new(format!("Invalid '{key}' property")))
        })
        .transpose()
}

struct QbittorrentDownloader {
    client: reqwest::Client,
    endpoint: String,
    username: Option<String>,
    password: Option<String>,
    logged_in: Mutex<bool>,
    default_path: OnceLock<String>,
    always_download_all: bool,
}

impl Debug for QbittorrentDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QbittorrentDownloader")
            .field("endpoint", &self.endpoint)
            .field("always_download_all", &self.always_download_all)
            .finish()
    }
}
impl Display for QbittorrentDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "qbittorrent")
    }
}
impl SdComponent for QbittorrentDownloader {
    fn as_async_downloader(
        self: Arc<Self>,
    ) -> Result<Arc<dyn AsyncDownloader>, ComponentError> {
        Ok(self)
    }
}

#[derive(Deserialize)]
struct TorrentInfo {
    progress: f64,
}
#[derive(Deserialize)]
struct TorrentFile {
    index: u32,
    name: String,
}

impl QbittorrentDownloader {
    async fn login(&self) -> Result<(), ProcessingError> {
        if *self.logged_in.lock() {
            return Ok(());
        }
        let Some(username) = &self.username else {
            *self.logged_in.lock() = true;
            return Ok(());
        };
        let response = http::execute(
            &self.client,
            self.client.post(format!("{}/api/v2/auth/login", self.endpoint)).form(&[
                ("username", username.as_str()),
                ("password", self.password.as_deref().unwrap_or("")),
            ]),
            "Login to qBittorrent",
        )
        .await?
        .text()
        .await
        .map_err(|error| http::map_error(error, "Read qBittorrent login"))?;
        if response.trim() != "Ok." {
            return Err(ProcessingError::non_retryable(format!(
                "qBittorrent login failed: {response}"
            )));
        }
        *self.logged_in.lock() = true;
        Ok(())
    }

    async fn request(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<reqwest::Response, ProcessingError> {
        self.login().await?;
        let result = http::execute(&self.client, request, operation).await;
        if result.as_ref().err().is_some_and(|error| error.to_string().contains("403")) {
            *self.logged_in.lock() = false;
            self.login().await?;
        }
        result
    }

    async fn torrent_hash(&self, item: &SourceItem) -> Result<String, ProcessingError> {
        if let Some(hash) = magnet_hash(&item.download_uri.to_string()) {
            return Ok(hash);
        }
        let bytes = self
            .client
            .get(item.download_uri.to_string())
            .send()
            .await
            .map_err(|error| http::map_error(error, "Fetch torrent for info hash"))?
            .error_for_status()
            .map_err(|error| http::map_error(error, "Fetch torrent for info hash"))?
            .bytes()
            .await
            .map_err(|error| http::map_error(error, "Read torrent for info hash"))?;
        let info = info_slice(&bytes)?;
        Ok(format!("{:x}", Sha1::digest(info)))
    }

    async fn set_unwanted(
        &self,
        hash: &str,
        wanted_paths: &[String],
    ) -> Result<(), ProcessingError> {
        let mut files = Vec::new();
        for attempt in 0..4 {
            let response = self
                .request(
                    self.client
                        .get(format!("{}/api/v2/torrents/files", self.endpoint))
                        .query(&[("hash", hash)]),
                    "Get qBittorrent files",
                )
                .await;
            match response {
                Ok(response) => {
                    files =
                        response.json::<Vec<TorrentFile>>().await.map_err(|error| {
                            http::map_error(error, "Decode qBittorrent files")
                        })?;
                    break;
                }
                Err(error) if attempt < 3 => {
                    tracing::debug!(attempt, %error, "qBittorrent metadata not ready");
                    tokio::time::sleep(Duration::from_millis(300 * (attempt + 1))).await;
                }
                Err(error) => return Err(error),
            }
        }
        let unwanted = files
            .into_iter()
            .filter(|file| !wanted_paths.iter().any(|path| path == &file.name))
            .map(|file| file.index.to_string())
            .collect::<Vec<_>>();
        if unwanted.is_empty() {
            return Ok(());
        }
        self.request(
            self.client.post(format!("{}/api/v2/torrents/filePrio", self.endpoint)).form(
                &[
                    ("hash", hash.to_string()),
                    ("id", unwanted.join("|")),
                    ("priority", "0".to_string()),
                ],
            ),
            "Set qBittorrent file priority",
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl Downloader for QbittorrentDownloader {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
        let tags = task.tags.unwrap_or_default().join(",");
        let mut form = vec![
            ("urls", task.source_item.download_uri.to_string()),
            ("savepath", task.download_path.to_string_lossy().into_owned()),
        ];
        if let Some(category) = task.category.as_ref() {
            form.push(("category", category.clone()));
        }
        if !tags.is_empty() {
            form.push(("tags", tags));
        }
        let body = self
            .request(
                self.client
                    .post(format!("{}/api/v2/torrents/add", self.endpoint))
                    .form(&form),
                "Add qBittorrent torrent",
            )
            .await?
            .text()
            .await
            .map_err(|error| http::map_error(error, "Read qBittorrent add response"))?;
        if body.trim() != "Ok." {
            return Err(ProcessingError::non_retryable(format!(
                "qBittorrent add failed: {body}"
            )));
        }
        if !self.always_download_all {
            let hash = self.torrent_hash(task.source_item).await?;
            let wanted = task
                .download_files
                .iter()
                .map(|file| file.path.to_string_lossy().replace('\\', "/"))
                .collect::<Vec<_>>();
            self.set_unwanted(&hash, &wanted).await?;
        }
        Ok(())
    }

    fn default_download_path(&self) -> &str {
        self.default_path.get_or_init(|| {
            let request =
                self.client.get(format!("{}/api/v2/app/defaultSavePath", self.endpoint));
            let result = tokio::runtime::Handle::try_current().ok().and_then(|handle| {
                tokio::task::block_in_place(|| {
                    handle.block_on(async {
                        self.request(request, "Get qBittorrent default save path")
                            .await
                            .ok()?
                            .text()
                            .await
                            .ok()
                    })
                })
            });
            result.unwrap_or_else(|| ".".to_string())
        })
    }

    async fn cancel(
        &self,
        item: &SourceItem,
        _: &[SourceFile],
    ) -> Result<(), ProcessingError> {
        let hash = self.torrent_hash(item).await?;
        self.request(
            self.client
                .post(format!("{}/api/v2/torrents/delete", self.endpoint))
                .form(&[("hashes", hash), ("deleteFiles", "false".to_string())]),
            "Delete qBittorrent torrent",
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl AsyncDownloader for QbittorrentDownloader {
    async fn is_finished(&self, item: &SourceItem) -> Option<bool> {
        let hash = self.torrent_hash(item).await.ok()?;
        let response = self
            .request(
                self.client
                    .get(format!("{}/api/v2/torrents/info", self.endpoint))
                    .query(&[("hashes", hash)]),
                "Get qBittorrent status",
            )
            .await
            .ok()?;
        response
            .json::<Vec<TorrentInfo>>()
            .await
            .ok()?
            .first()
            .map(|torrent| torrent.progress >= 1.0)
    }
}

fn magnet_hash(uri: &str) -> Option<String> {
    let uri = url::Url::parse(uri).ok()?;
    if uri.scheme() != "magnet" {
        return None;
    }
    uri.query_pairs()
        .find(|(key, value)| key == "xt" && value.starts_with("urn:btih:"))
        .map(|(_, value)| value.trim_start_matches("urn:btih:").to_ascii_lowercase())
}

fn info_slice(bytes: &[u8]) -> Result<&[u8], ProcessingError> {
    if bytes.first() != Some(&b'd') {
        return Err(ProcessingError::non_retryable("Invalid torrent metadata"));
    }
    let mut cursor = 1;
    while bytes.get(cursor) != Some(&b'e') {
        let key = parse_string(bytes, &mut cursor)?;
        let start = cursor;
        skip_value(bytes, &mut cursor)?;
        if key == b"info" {
            return Ok(&bytes[start..cursor]);
        }
    }
    Err(ProcessingError::non_retryable("Torrent info dictionary missing"))
}
fn parse_string<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<&'a [u8], ProcessingError> {
    let colon = bytes[*cursor..]
        .iter()
        .position(|byte| *byte == b':')
        .map(|offset| *cursor + offset)
        .ok_or_else(|| ProcessingError::non_retryable("Invalid bencode string"))?;
    let length = std::str::from_utf8(&bytes[*cursor..colon])
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .ok_or_else(|| ProcessingError::non_retryable("Invalid bencode length"))?;
    let start = colon + 1;
    let end = start
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| ProcessingError::non_retryable("Truncated bencode string"))?;
    *cursor = end;
    Ok(&bytes[start..end])
}
fn skip_value(bytes: &[u8], cursor: &mut usize) -> Result<(), ProcessingError> {
    match bytes.get(*cursor) {
        Some(b'i') => {
            *cursor += 1;
            let end = bytes[*cursor..].iter().position(|byte| *byte == b'e').ok_or_else(
                || ProcessingError::non_retryable("Invalid bencode integer"),
            )?;
            *cursor += end + 1;
        }
        Some(b'l' | b'd') => {
            *cursor += 1;
            while bytes.get(*cursor) != Some(&b'e') {
                if bytes.get(*cursor).is_some_and(u8::is_ascii_digit) {
                    parse_string(bytes, cursor)?;
                    if bytes.get(*cursor) == Some(&b'e') {
                        break;
                    }
                }
                skip_value(bytes, cursor)?;
            }
            *cursor += 1;
        }
        Some(byte) if byte.is_ascii_digit() => {
            parse_string(bytes, cursor)?;
        }
        _ => return Err(ProcessingError::non_retryable("Invalid bencode value")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hashes() {
        assert_eq!(
            Some("abcdef".to_string()),
            magnet_hash("magnet:?xt=urn:btih:ABCDEF&dn=test")
        );
        let torrent = b"d8:announce1:x4:infod4:name1:aee";
        assert_eq!(b"d4:name1:ae", info_slice(torrent).unwrap());
    }

    #[test]
    fn validates_endpoint() {
        assert!(SUPPLIER.apply(&Map::new()).is_err());
    }
}

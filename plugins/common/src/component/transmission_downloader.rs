use crate::http;
use parking_lot::RwLock;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    AsyncDownloader, ComponentError, ComponentSupplier, ComponentType, DownloadTask,
    Downloader, ProcessingError, SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, OnceLock};

pub struct TransmissionDownloaderSupplier;
pub const SUPPLIER: TransmissionDownloaderSupplier = TransmissionDownloaderSupplier;

impl ComponentSupplier for TransmissionDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("transmission".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let endpoint = required_string(props, "url")?;
        let username = optional(props, "username")?;
        let password = optional(props, "password")?;
        let client = if endpoint.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|error| ComponentError::new(error.to_string()))?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(TransmissionDownloader {
            client,
            endpoint,
            username,
            password,
            session_id: RwLock::new(None),
            default_path: OnceLock::new(),
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Downloads torrents through Transmission.".to_owned(),
            props_json_schema: Some(json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "username": {"type": "string"},
                    "password": {"type": "string"}
                },
                "required": ["url"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

fn required_string(
    props: &Map<String, Value>,
    key: &str,
) -> Result<String, ComponentError> {
    let value = props.get(key).ok_or_else(|| {
        ComponentError::new(format!(
            "Invalid configuration at '{key}': missing field `{key}`"
        ))
    })?;
    serde_json::from_value::<String>(value.clone()).map_err(|error| {
        ComponentError::new(format!("Invalid configuration at '{key}': {error}"))
    })
}

fn optional(
    props: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ComponentError> {
    props
        .get(key)
        .map(|value| {
            serde_json::from_value::<String>(value.clone()).map_err(|error| {
                ComponentError::new(format!("Invalid configuration at '{key}': {error}"))
            })
        })
        .transpose()
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Downloader, AsyncDownloader)]
struct TransmissionDownloader {
    client: reqwest::Client,
    endpoint: String,
    username: Option<String>,
    password: Option<String>,
    session_id: RwLock<Option<String>>,
    default_path: OnceLock<String>,
}

impl Display for TransmissionDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "transmission")
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: String,
    arguments: T,
}
#[derive(Deserialize)]
struct AddArguments {
    #[serde(rename = "torrent-added")]
    added: Option<TorrentId>,
    #[serde(rename = "torrent-duplicate")]
    duplicate: Option<TorrentId>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TorrentId {
    hash_string: String,
}
#[derive(Deserialize)]
struct Torrents {
    #[serde(default)]
    torrents: Vec<Torrent>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Torrent {
    percent_done: f64,
    #[serde(default)]
    files: Vec<TorrentFile>,
}
#[derive(Deserialize)]
struct TorrentFile {
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Session {
    download_dir: String,
}

impl TransmissionDownloader {
    fn builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut request = self.client.post(&self.endpoint).json(body);
        if let Some(username) = &self.username {
            request = request.basic_auth(username, self.password.as_ref());
        }
        if let Some(session_id) = self.session_id.read().as_ref() {
            request = request.header("X-Transmission-Session-Id", session_id);
        }
        request
    }

    async fn rpc<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        arguments: Value,
    ) -> Result<T, ProcessingError> {
        let body = json!({"method": method, "arguments": arguments});
        for replay in 0..2 {
            let response = self
                .builder(&body)
                .send()
                .await
                .map_err(|error| http::map_error(error, "Transmission RPC"))?;
            if response.status() == reqwest::StatusCode::CONFLICT {
                let session_id = response
                    .headers()
                    .get("X-Transmission-Session-Id")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        ProcessingError::non_retryable(
                            "Transmission 409 response omitted session ID",
                        )
                    })?;
                *self.session_id.write() = Some(session_id);
                if replay == 0 {
                    continue;
                }
            }
            let response = response
                .error_for_status()
                .map_err(|error| http::map_error(error, "Transmission RPC"))?;
            let envelope = response
                .json::<Envelope<T>>()
                .await
                .map_err(|error| http::map_error(error, "Decode Transmission RPC"))?;
            if envelope.result != "success" {
                return Err(ProcessingError::non_retryable(format!(
                    "Transmission {method} failed: {}",
                    envelope.result
                )));
            }
            return Ok(envelope.arguments);
        }
        Err(ProcessingError::non_retryable("Transmission session negotiation failed"))
    }

    async fn torrent_hash(&self, item: &SourceItem) -> Result<String, ProcessingError> {
        if let Some(hash) = magnet_hash(&item.download_uri.to_string()) {
            return Ok(hash);
        }
        let bytes = http::execute(
            &self.client,
            self.client.get(item.download_uri.to_string()),
            "Fetch torrent for Transmission hash",
        )
        .await?
        .bytes()
        .await
        .map_err(|error| http::map_error(error, "Read torrent for Transmission hash"))?;
        Ok(hex::encode(Sha1::digest(info_slice(&bytes)?)))
    }

    async fn get_torrent(&self, hash: &str) -> Result<Option<Torrent>, ProcessingError> {
        let values: Torrents = self
            .rpc(
                "torrent-get",
                json!({"ids": [hash], "fields": ["hashString", "percentDone", "files"]}),
            )
            .await?;
        Ok(values.torrents.into_iter().next())
    }
}

#[async_trait]
impl Downloader for TransmissionDownloader {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
        let labels = task.tags.unwrap_or_default();
        let arguments: AddArguments = self
            .rpc(
                "torrent-add",
                json!({
                    "filename": task.source_item.download_uri.to_string(),
                    "download-dir": task.download_path,
                    "labels": labels,
                    "paused": false
                }),
            )
            .await?;
        let hash = arguments
            .added
            .or(arguments.duplicate)
            .map(|torrent| torrent.hash_string)
            .ok_or_else(|| {
                ProcessingError::non_retryable("Transmission add omitted hash")
            })?;
        let Some(torrent) = self.get_torrent(&hash).await? else {
            tracing::warn!(%hash, "Transmission torrent not visible after add");
            return Ok(());
        };
        let wanted = task
            .download_files
            .iter()
            .map(|file| file.path.to_string_lossy().replace('\\', "/"))
            .collect::<Vec<_>>();
        let unwanted = torrent
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| !wanted.iter().any(|path| path == &file.name))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if !unwanted.is_empty() {
            let _: Value = self
                .rpc("torrent-set", json!({"ids": [hash], "files-unwanted": unwanted}))
                .await?;
        }
        Ok(())
    }

    fn default_download_path(&self) -> &str {
        self.default_path.get_or_init(|| {
            tokio::runtime::Handle::try_current()
                .ok()
                .and_then(|handle| {
                    tokio::task::block_in_place(|| {
                        handle.block_on(async {
                            self.rpc::<Session>("session-get", json!({}))
                                .await
                                .ok()
                                .map(|session| session.download_dir)
                        })
                    })
                })
                .unwrap_or_else(|| ".".to_string())
        })
    }

    async fn cancel(
        &self,
        item: &SourceItem,
        _: &[SourceFile],
    ) -> Result<(), ProcessingError> {
        let hash = self.torrent_hash(item).await?;
        let _: Value = self
            .rpc("torrent-remove", json!({"ids": [hash], "delete-local-data": false}))
            .await?;
        Ok(())
    }
}

#[async_trait]
impl AsyncDownloader for TransmissionDownloader {
    async fn is_finished(&self, item: &SourceItem) -> Option<bool> {
        let hash = self.torrent_hash(item).await.ok()?;
        self.get_torrent(&hash).await.ok()?.map(|torrent| torrent.percent_done >= 1.0)
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
        skip(bytes, &mut cursor)?;
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
fn skip(bytes: &[u8], cursor: &mut usize) -> Result<(), ProcessingError> {
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
                skip(bytes, cursor)?;
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
    fn supplier_requires_url() {
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_err()
        );
    }

    #[test]
    fn extracts_magnet_hash() {
        assert_eq!(Some("abcdef".to_string()), magnet_hash("magnet:?xt=urn:btih:ABCDEF"));
    }
}

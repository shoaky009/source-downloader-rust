use crate::http;
use parking_lot::Mutex;
use serde::Deserialize;
use sha1::{Digest, Sha1};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    AsyncDownloader, ComponentCompatibilityConstraint, ComponentCompatibilityRelation,
    ComponentCompatibilityRule, ComponentError, ComponentSelector, ComponentSupplier,
    ComponentType, DownloadTask, Downloader, FileContent, FileMover, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{self, Map, Value};
use std::fmt::{Debug, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub struct QbittorrentDownloaderSupplier;
pub const SUPPLIER: QbittorrentDownloaderSupplier = QbittorrentDownloaderSupplier;

impl ComponentSupplier for QbittorrentDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::downloader("qbittorrent".to_string()),
            ComponentType::file_mover("qbittorrent".to_string()),
        ]
    }
    fn compatibility_rules(&self) -> Vec<ComponentCompatibilityRule> {
        vec![ComponentCompatibilityRule {
            code: "qbittorrent-instance-must-match".to_owned(),
            owner: ComponentType::file_mover("qbittorrent".to_owned()),
            constraint: ComponentCompatibilityConstraint::Requires {
                target: ComponentSelector {
                    root_type:
                        source_downloader_sdk::component::ComponentRootType::Downloader,
                    type_names: vec!["qbittorrent".to_owned()],
                },
                relations: vec![ComponentCompatibilityRelation::InstanceNameEquals],
            },
            message: "qBittorrent file mover requires the downloader to use the same component instance"
                .to_owned(),
        }]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let endpoint = match optional_string(props, "endpoint")? {
            Some(endpoint) => endpoint,
            None => optional_string(props, "host")?.ok_or_else(|| {
                ComponentError::new(
                    "Invalid configuration at 'endpoint': \
                     missing field `endpoint`",
                )
            })?,
        }
        .trim_end_matches('/')
        .to_string();
        let username = optional_string(props, "username")?;
        let password = optional_string(props, "password")?;
        let always_download_all = props
            .get("always-download-all")
            .map(|value| {
                serde_json::from_value::<bool>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'always-download-all': {error}"
                    ))
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
            serde_json::from_value::<String>(value.clone()).map_err(|error| {
                ComponentError::new(format!("Invalid configuration at '{key}': {error}"))
            })
        })
        .transpose()
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Downloader, AsyncDownloader, FileMover)]
struct QbittorrentDownloader {
    client: reqwest::Client,
    endpoint: String,
    username: Option<String>,
    password: Option<String>,
    logged_in: Mutex<bool>,
    default_path: OnceLock<String>,
    always_download_all: bool,
}

impl Display for QbittorrentDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "qbittorrent")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
struct TorrentInfo {
    progress: f64,
    content_path: Option<PathBuf>,
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
        Ok(hex::encode(Sha1::digest(info)))
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

    fn run_sync<T>(
        &self,
        future: impl Future<Output = Result<T, ProcessingError>>,
    ) -> Result<T, ProcessingError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            ProcessingError::non_retryable(
                "qBittorrent file operations require a Tokio runtime",
            )
        })?;
        tokio::task::block_in_place(|| handle.block_on(future))
    }

    async fn rename_file(
        &self,
        hash: &str,
        old_path: &Path,
        new_path: &Path,
    ) -> Result<(), ProcessingError> {
        self.request(
            self.client
                .post(format!("{}/api/v2/torrents/renameFile", self.endpoint))
                .form(&[
                    ("hash", hash.to_string()),
                    ("oldPath", torrent_path(old_path)),
                    ("newPath", torrent_path(new_path)),
                ]),
            "Rename qBittorrent file",
        )
        .await?;
        Ok(())
    }

    async fn set_location(
        &self,
        hash: &str,
        location: &Path,
    ) -> Result<(), ProcessingError> {
        self.request(
            self.client
                .post(format!("{}/api/v2/torrents/setLocation", self.endpoint))
                .form(&[
                    ("hashes", hash.to_string()),
                    ("location", location.to_string_lossy().into_owned()),
                ]),
            "Set qBittorrent location",
        )
        .await?;
        Ok(())
    }

    async fn torrent_files(&self, hash: &str) -> Result<Vec<PathBuf>, ProcessingError> {
        let response = self
            .request(
                self.client
                    .get(format!("{}/api/v2/torrents/info", self.endpoint))
                    .query(&[("hashes", hash)]),
                "Get qBittorrent status",
            )
            .await?;
        let Some(info) = response
            .json::<Vec<TorrentInfo>>()
            .await
            .map_err(|error| http::map_error(error, "Decode qBittorrent status"))?
            .into_iter()
            .next()
        else {
            return Ok(Vec::new());
        };
        let Some(content_path) = info.content_path else {
            return Ok(Vec::new());
        };
        if content_path.extension().is_some() {
            return Ok(vec![content_path]);
        }
        let response = self
            .request(
                self.client
                    .get(format!("{}/api/v2/torrents/files", self.endpoint))
                    .query(&[("hash", hash)]),
                "Get qBittorrent files",
            )
            .await?;
        response
            .json::<Vec<TorrentFile>>()
            .await
            .map_err(|error| http::map_error(error, "Decode qBittorrent files"))
            .map(|files| {
                files.into_iter().map(|file| content_path.join(file.name)).collect()
            })
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

impl FileMover for QbittorrentDownloader {
    fn move_file(&self, _: &SourceItem, _: &FileContent) -> Result<(), ProcessingError> {
        Err(ProcessingError::non_retryable("qBittorrent only supports batch file moves"))
    }

    fn replace(
        &self,
        source_item: &SourceItem,
        files: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        let torrent_files = magnet_hash(&source_item.download_uri.to_string())
            .map(|hash| self.run_sync(self.torrent_files(&hash)))
            .transpose()?
            .unwrap_or_default();
        for file in files {
            let existing_path =
                file.exist_target_path.as_ref().unwrap_or_else(|| file.target_path());
            if !existing_path.exists() {
                self.batch_move(source_item, files)?;
                continue;
            }
            if torrent_files.iter().any(|path| path == file.target_path()) {
                tracing::info!(path = %existing_path.display(), "Torrent target is already managed; skipping replacement");
                continue;
            }
            let backup_path = existing_path.with_file_name(format!(
                "{}.bak",
                existing_path
                    .file_name()
                    .ok_or_else(|| ProcessingError::non_retryable(
                        "Replacement path has no file name"
                    ))?
                    .to_string_lossy(),
            ));
            std::fs::rename(existing_path, &backup_path)?;
            match self.batch_move(source_item, files) {
                Ok(()) => {
                    if backup_path.exists() {
                        std::fs::remove_file(&backup_path)?;
                    }
                }
                Err(error) => {
                    if !existing_path.exists() && backup_path.exists() {
                        std::fs::rename(&backup_path, existing_path)?;
                    }
                    return Err(error);
                }
            }
        }
        Ok(())
    }

    fn create_directories(&self, _: &Path) -> Result<(), ProcessingError> {
        Ok(())
    }

    fn is_supported_batch_move(&self) -> bool {
        true
    }

    fn batch_move(
        &self,
        source_item: &SourceItem,
        files: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        let Some(first_file) = files.first() else {
            return Ok(());
        };
        let item_location = first_file
            .file_save_root_dir()
            .unwrap_or_else(|| first_file.target_save_path.clone());
        self.run_sync(async {
            let hash = self.torrent_hash(source_item).await?;
            for file in files {
                let old_path = file
                    .file_download_path
                    .strip_prefix(&file.download_path)
                    .map_err(|_| {
                        ProcessingError::non_retryable(format!(
                            "Downloaded file '{}' is outside download path '{}'",
                            file.file_download_path.display(),
                            file.download_path.display(),
                        ))
                    })?;
                let new_path =
                    file.target_path().strip_prefix(&item_location).map_err(|_| {
                        ProcessingError::non_retryable(format!(
                            "Target file '{}' is outside item location '{}'",
                            file.target_path().display(),
                            item_location.display(),
                        ))
                    })?;
                self.rename_file(&hash, old_path, new_path).await?;
            }
            self.set_location(&hash, &item_location).await
        })
    }
}

fn torrent_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
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
    use source_downloader_sdk::component::FileContentStatus;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn form(request: &wiremock::Request) -> HashMap<String, String> {
        url::form_urlencoded::parse(&request.body).into_owned().collect()
    }

    fn source_item() -> SourceItem {
        SourceItem {
            title: "test".to_string(),
            link: "https://example.com/item".parse().unwrap(),
            datetime: time::OffsetDateTime::UNIX_EPOCH,
            content_type: "application/x-bittorrent".to_string(),
            download_uri:
                "magnet://localhost/?xt=urn:btih:0123456789abcdef0123456789abcdef01234567"
                    .parse()
                    .unwrap(),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        }
    }

    fn file_content() -> FileContent {
        FileContent {
            download_path: PathBuf::from("/downloads"),
            file_download_path: PathBuf::from("/downloads/show/01.mkv"),
            source_save_path: PathBuf::from("/library"),
            pattern_variables: HashMap::new(),
            file_save_path_pattern: String::new(),
            filename_pattern: String::new(),
            tags: vec![],
            attrs: Map::new(),
            file_uri: None,
            target_save_path: PathBuf::from("/library/anime"),
            target_filename: "episode-01.mkv".to_string(),
            exist_target_path: None,
            errors: vec![],
            status: FileContentStatus::Normal,
            target_path: OnceLock::new(),
            data: None,
            processed_variables: None,
        }
    }

    #[test]
    fn declares_same_instance_compatibility_rule() {
        let rules = SUPPLIER.compatibility_rules();

        assert_eq!(
            rules[0].constraint,
            ComponentCompatibilityConstraint::Requires {
                target: ComponentSelector {
                    root_type:
                        source_downloader_sdk::component::ComponentRootType::Downloader,
                    type_names: vec!["qbittorrent".to_owned()],
                },
                relations: vec![ComponentCompatibilityRelation::InstanceNameEquals],
            }
        );
    }

    #[test]
    fn extracts_hashes() {
        assert_eq!(
            Some("abcdef".to_string()),
            magnet_hash("magnet:?xt=urn:btih:ABCDEF&dn=test")
        );
        let torrent = b"d8:announce1:x4:infod4:name1:aee";
        assert_eq!(b"d4:name1:ae", info_slice(torrent).unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exposes_file_mover_and_moves_torrent_in_batch() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/renameFile"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/torrents/setLocation"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let component = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::from_iter([("endpoint".into(), Value::String(server.uri()))]),
            )
            .unwrap();
        let mover = component.as_file_mover().unwrap();
        let file = file_content();
        mover.batch_move(&source_item(), &[&file]).unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(2, requests.len());
        assert_eq!(
            HashMap::from_iter([
                (
                    "hash".to_string(),
                    "0123456789abcdef0123456789abcdef01234567".to_string(),
                ),
                ("oldPath".to_string(), "show/01.mkv".to_string()),
                ("newPath".to_string(), "episode-01.mkv".to_string()),
            ]),
            form(&requests[0]),
        );
        assert_eq!(
            HashMap::from_iter([
                (
                    "hashes".to_string(),
                    "0123456789abcdef0123456789abcdef01234567".to_string(),
                ),
                ("location".to_string(), "/library/anime".to_string()),
            ]),
            form(&requests[1]),
        );
    }

    #[test]
    fn validates_endpoint() {
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_err()
        );
    }
}

use crate::http;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct TorrentFileResolverSupplier;
pub const SUPPLIER: TorrentFileResolverSupplier = TorrentFileResolverSupplier;

impl ComponentSupplier for TorrentFileResolverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_resolver("torrent".to_string())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(TorrentFileResolver { client: http::build_client()? }))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(ItemFileResolver)]
struct TorrentFileResolver {
    client: reqwest::Client,
}

impl Display for TorrentFileResolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "torrent")
    }
}

#[async_trait]
impl ItemFileResolver for TorrentFileResolver {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let uri = item.download_uri.to_string();
        let bytes = if uri.starts_with("magnet:") {
            let exact_source = url::Url::parse(&uri)
                .ok()
                .and_then(|url| {
                    url.query_pairs()
                        .find(|(key, value)| key == "xs" && !value.is_empty())
                        .map(|(_, value)| value.into_owned())
                })
                .ok_or_else(|| {
                    ProcessingError::non_retryable(
                        "Magnet metadata discovery requires an exact-source (xs) torrent URL",
                    )
                })?;
            self.fetch(&exact_source).await?
        } else {
            self.fetch(&uri).await?
        };
        parse_torrent(&bytes)
    }
}

impl TorrentFileResolver {
    async fn fetch(&self, uri: &str) -> Result<Vec<u8>, ProcessingError> {
        http::execute(&self.client, self.client.get(uri), "Fetch torrent metadata")
            .await?
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| http::map_error(error, "Read torrent metadata"))
    }
}

#[derive(Debug)]
enum Bencode<'a> {
    Integer(i64),
    Bytes(&'a [u8]),
    List(Vec<Bencode<'a>>),
    Dictionary(BTreeMap<&'a [u8], Bencode<'a>>),
}

fn parse_torrent(bytes: &[u8]) -> Result<Vec<SourceFile>, ProcessingError> {
    let mut cursor = 0;
    let root = parse_value(bytes, &mut cursor)?;
    if cursor != bytes.len() {
        return Err(invalid("Trailing data in torrent metadata"));
    }
    let root = dictionary(&root, "torrent root")?;
    let info = dictionary(required(root, b"info")?, "torrent info")?;
    let name = utf8(required(info, b"name")?, "torrent name")?;
    let mut files = Vec::new();
    if let Some(entries) = info.get(b"files".as_slice()) {
        for entry in list(entries, "torrent files")? {
            let entry = dictionary(entry, "torrent file")?;
            let length = integer(required(entry, b"length")?, "torrent file length")?;
            if length <= 0 {
                continue;
            }
            let segments = list(required(entry, b"path")?, "torrent file path")?
                .iter()
                .map(|segment| utf8(segment, "torrent path segment"))
                .collect::<Result<Vec<_>, _>>()?;
            if segments.is_empty() || ignored(segments.last().unwrap()) {
                continue;
            }
            let mut path = PathBuf::from(name);
            for segment in segments {
                validate_segment(segment)?;
                path.push(segment);
            }
            files.push(source_file(path, length));
        }
    } else {
        validate_segment(name)?;
        let length = integer(required(info, b"length")?, "torrent length")?;
        if length > 0 && !ignored(name) {
            files.push(source_file(PathBuf::from(name), length));
        }
    }
    Ok(files)
}

fn source_file(path: PathBuf, length: i64) -> SourceFile {
    SourceFile {
        path,
        attrs: Map::from_iter([("size".to_string(), Value::from(length))]),
        download_uri: None,
        tags: vec![],
        data: None,
    }
}

fn ignored(name: &str) -> bool {
    name.contains("如果您看到此文件，请升级到BitComet")
        || name.contains("_____padding_file_")
}

fn validate_segment(segment: &str) -> Result<(), ProcessingError> {
    if segment.is_empty()
        || matches!(segment, "." | "..")
        || segment.contains(['/', '\\'])
    {
        Err(invalid("Unsafe path segment in torrent metadata"))
    } else {
        Ok(())
    }
}

fn parse_value<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<Bencode<'a>, ProcessingError> {
    match bytes.get(*cursor).copied() {
        Some(b'i') => parse_integer(bytes, cursor),
        Some(b'l') => parse_list(bytes, cursor),
        Some(b'd') => parse_dictionary(bytes, cursor),
        Some(b'0'..=b'9') => parse_bytes(bytes, cursor),
        _ => Err(invalid("Invalid bencode value")),
    }
}

fn parse_integer<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<Bencode<'a>, ProcessingError> {
    *cursor += 1;
    let end = bytes[*cursor..]
        .iter()
        .position(|byte| *byte == b'e')
        .map(|offset| *cursor + offset)
        .ok_or_else(|| invalid("Unterminated bencode integer"))?;
    let raw = std::str::from_utf8(&bytes[*cursor..end])
        .map_err(|_| invalid("Invalid bencode integer"))?;
    if raw.is_empty()
        || raw == "-0"
        || raw.starts_with('0') && raw.len() > 1
        || raw.starts_with("-0")
    {
        return Err(invalid("Non-canonical bencode integer"));
    }
    let value = raw.parse().map_err(|_| invalid("Invalid bencode integer"))?;
    *cursor = end + 1;
    Ok(Bencode::Integer(value))
}

fn parse_bytes<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<Bencode<'a>, ProcessingError> {
    let colon = bytes[*cursor..]
        .iter()
        .position(|byte| *byte == b':')
        .map(|offset| *cursor + offset)
        .ok_or_else(|| invalid("Invalid bencode byte string"))?;
    let length = std::str::from_utf8(&bytes[*cursor..colon])
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .ok_or_else(|| invalid("Invalid bencode byte length"))?;
    let start = colon + 1;
    let end = start
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| invalid("Truncated bencode byte string"))?;
    *cursor = end;
    Ok(Bencode::Bytes(&bytes[start..end]))
}

fn parse_list<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<Bencode<'a>, ProcessingError> {
    *cursor += 1;
    let mut values = Vec::new();
    while bytes.get(*cursor) != Some(&b'e') {
        values.push(parse_value(bytes, cursor)?);
    }
    *cursor += 1;
    Ok(Bencode::List(values))
}

fn parse_dictionary<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
) -> Result<Bencode<'a>, ProcessingError> {
    *cursor += 1;
    let mut values = BTreeMap::new();
    while bytes.get(*cursor) != Some(&b'e') {
        let Bencode::Bytes(key) = parse_bytes(bytes, cursor)? else { unreachable!() };
        let value = parse_value(bytes, cursor)?;
        if values.insert(key, value).is_some() {
            return Err(invalid("Duplicate bencode dictionary key"));
        }
    }
    *cursor += 1;
    Ok(Bencode::Dictionary(values))
}

fn required<'a>(
    values: &'a BTreeMap<&[u8], Bencode<'a>>,
    key: &[u8],
) -> Result<&'a Bencode<'a>, ProcessingError> {
    values.get(key).ok_or_else(|| invalid("Missing torrent metadata field"))
}
fn dictionary<'a>(
    value: &'a Bencode<'a>,
    field: &str,
) -> Result<&'a BTreeMap<&'a [u8], Bencode<'a>>, ProcessingError> {
    match value {
        Bencode::Dictionary(value) => Ok(value),
        _ => Err(invalid(format!("Invalid {field}"))),
    }
}
fn list<'a>(
    value: &'a Bencode<'a>,
    field: &str,
) -> Result<&'a [Bencode<'a>], ProcessingError> {
    match value {
        Bencode::List(value) => Ok(value),
        _ => Err(invalid(format!("Invalid {field}"))),
    }
}
fn integer(value: &Bencode<'_>, field: &str) -> Result<i64, ProcessingError> {
    match value {
        Bencode::Integer(value) => Ok(*value),
        _ => Err(invalid(format!("Invalid {field}"))),
    }
}
fn utf8<'a>(value: &'a Bencode<'a>, field: &str) -> Result<&'a str, ProcessingError> {
    match value {
        Bencode::Bytes(value) => std::str::from_utf8(value)
            .map_err(|_| invalid(format!("Invalid UTF-8 {field}"))),
        _ => Err(invalid(format!("Invalid {field}"))),
    }
}
fn invalid(message: impl Into<String>) -> ProcessingError {
    ProcessingError::non_retryable(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_and_multi_file_torrents() {
        let single = b"d4:infod6:lengthi12e4:name8:file.mkvee";
        let files = parse_torrent(single).unwrap();
        assert_eq!(PathBuf::from("file.mkv"), files[0].path);
        assert_eq!(Some(&Value::from(12)), files[0].attrs.get("size"));

        let multi = b"d4:infod5:filesld6:lengthi5e4:pathl1:a5:b.mkveed6:lengthi0e4:pathl7:ignoredeee4:name4:Showee";
        let files = parse_torrent(multi).unwrap();
        assert_eq!(1, files.len());
        assert_eq!(PathBuf::from("Show/a/b.mkv"), files[0].path);
    }

    #[test]
    fn rejects_path_traversal() {
        let torrent = b"d4:infod5:filesld6:lengthi1e4:pathl2:..1:aeee4:name4:Showee";
        assert!(parse_torrent(torrent).is_err());
    }
}

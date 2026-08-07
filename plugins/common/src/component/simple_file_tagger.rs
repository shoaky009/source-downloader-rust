use source_downloader_sdk::SdComponent;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileTagger, SdComponent,
    SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct SimpleFileTaggerSupplier;
pub const SUPPLIER: SimpleFileTaggerSupplier = SimpleFileTaggerSupplier;

impl ComponentSupplier for SimpleFileTaggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_tagger("simple".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let mut mapping =
            HashMap::from([("x-subrip".to_string(), "subtitle".to_string())]);
        if let Some(value) = props.get("external-mapping") {
            let external = value.as_object().ok_or_else(|| {
                ComponentError::new("Invalid 'external-mapping' property")
            })?;
            for (subtype, tag) in external {
                mapping.insert(
                    subtype.clone(),
                    tag.as_str()
                        .ok_or_else(|| {
                            ComponentError::new("Invalid 'external-mapping' value")
                        })?
                        .to_string(),
                );
            }
        }
        Ok(Arc::new(SimpleFileTagger { mapping }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(FileTagger)]
struct SimpleFileTagger {
    mapping: HashMap<String, String>,
}
impl Display for SimpleFileTagger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "simple")
    }
}

#[async_trait]
impl FileTagger for SimpleFileTagger {
    async fn tag(&self, source_file: &SourceFile) -> Option<String> {
        let extension = source_file.path.extension()?.to_str()?.to_ascii_lowercase();
        let mime = mimetype_detector::detect_file(&source_file.path)
            .ok()
            .map(|value| value.mime())
            .or_else(|| mime_from_extension(&extension))?;
        if mime == "application/octet-stream" {
            return None;
        }
        let (top_level, subtype) = mime.split_once('/')?;
        if top_level != "application" {
            return Some(top_level.to_string());
        }
        self.mapping.get(subtype).cloned()
    }
}

fn mime_from_extension(extension: &str) -> Option<&'static str> {
    match extension {
        "mkv" => Some("video/x-matroska"),
        "mp4" => Some("video/mp4"),
        "mp3" => Some("audio/mpeg"),
        "flac" => Some("audio/flac"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "txt" => Some("text/plain"),
        "srt" => Some("application/x-subrip"),
        "ass" => Some("text/x-ssa"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    async fn tag(path: &str, props: Map<String, Value>) -> Option<String> {
        SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap()
            .as_file_tagger()
            .unwrap()
            .tag(&SourceFile::new(PathBuf::from(path)))
            .await
    }
    #[test]
    fn supplier_contract() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::file_tagger("simple".to_string())]
        );
        assert!(SUPPLIER.is_support_no_props());
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_ok()
        );
    }
    #[tokio::test]
    async fn tags_common_top_level_types_and_subtitles() {
        assert_eq!(Some("video".to_string()), tag("show.mkv", Map::new()).await);
        assert_eq!(Some("audio".to_string()), tag("song.flac", Map::new()).await);
        assert_eq!(Some("image".to_string()), tag("cover.png", Map::new()).await);
        assert_eq!(Some("text".to_string()), tag("notes.txt", Map::new()).await);
        assert_eq!(Some("subtitle".to_string()), tag("show.srt", Map::new()).await);
    }
    #[tokio::test]
    async fn external_mapping_overrides_default() {
        let props = Map::from_iter([(
            "external-mapping".to_string(),
            Value::Object(Map::from_iter([(
                "x-subrip".to_string(),
                Value::String("captions".to_string()),
            )])),
        )]);
        assert_eq!(Some("captions".to_string()), tag("show.srt", props).await);
    }
    #[tokio::test]
    async fn rejects_extensionless_and_unknown_files() {
        assert_eq!(None, tag("README", Map::new()).await);
        assert_eq!(None, tag("file.unknown", Map::new()).await);
    }
}

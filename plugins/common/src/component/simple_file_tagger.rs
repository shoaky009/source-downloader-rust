use serde::Deserialize;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileTagger, SdComponent,
    SdComponentMetadata, SourceFile, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

type Mapping = HashMap<Cow<'static, str>, Cow<'static, str>>;

pub struct SimpleFileTaggerSupplier;
static MIME_DETECTOR: LazyLock<infer::Infer> = LazyLock::new(|| {
    let mut detector = infer::Infer::new();
    detector.add("application/x-subrip", "ass", is_ass);
    detector
});

fn is_ass(bytes: &[u8]) -> bool {
    bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes).starts_with(b"[Script Info]")
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct SimpleFileTaggerConfig {
    #[serde(default)]
    external_mapping: HashMap<String, String>,
    #[serde(default)]
    extension_mapping: HashMap<String, String>,
}
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
        let config = deserialize_component_config::<SimpleFileTaggerConfig>(props)?;
        let mut mapping: Mapping = HashMap::from([
            ("x-subrip".into(), "subtitle".into()),
            ("x-substation-alpha".into(), "subtitle".into()),
            ("x-webvtt".into(), "subtitle".into()),
            ("x-microdvd".into(), "subtitle".into()),
            ("x-pgs".into(), "subtitle".into()),
            ("x-vobsub".into(), "subtitle".into()),
            ("x-sami".into(), "subtitle".into()),
            ("ttml+xml".into(), "subtitle".into()),
            ("x-scc".into(), "subtitle".into()),
        ]);
        mapping.extend(
            config
                .external_mapping
                .into_iter()
                .map(|(mime, tag)| (mime.into(), tag.into())),
        );
        let mut extension_mapping: Mapping = HashMap::from([
            ("srt".into(), "application/x-subrip".into()),
            ("ass".into(), "application/x-subrip".into()),
            ("ssa".into(), "application/x-subrip".into()),
            ("vtt".into(), "application/x-webvtt".into()),
            ("sub".into(), "application/x-microdvd".into()),
            ("sup".into(), "application/x-pgs".into()),
            ("idx".into(), "application/x-vobsub".into()),
            ("smi".into(), "application/x-sami".into()),
            ("sami".into(), "application/x-sami".into()),
            ("ttml".into(), "application/ttml+xml".into()),
            ("dfxp".into(), "application/ttml+xml".into()),
            ("scc".into(), "application/x-scc".into()),
            ("nfo".into(), "text/x-nfo".into()),
            ("txt".into(), "text/plain".into()),
            ("css".into(), "text/css".into()),
        ]);
        extension_mapping.extend(config.extension_mapping.into_iter().map(
            |(extension, mime)| {
                (
                    extension
                        .trim_start_matches("*.")
                        .trim_start_matches('.')
                        .to_ascii_lowercase()
                        .into(),
                    mime.into(),
                )
            },
        ));
        Ok(Arc::new(SimpleFileTagger { mapping, extension_mapping }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Tags files using MIME type and extension mappings.".into(),
            props_json_schema: Some(
                json!({"type":"object","properties":{"external-mapping":{"type":"object","additionalProperties":{"type":"string"},"default":{}},"extension-mapping":{"type":"object","additionalProperties":{"type":"string"},"default":{}}}}),
            ),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, SdComponent)]
#[component(FileTagger)]
struct SimpleFileTagger {
    mapping: Mapping,
    extension_mapping: Mapping,
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
        let mime = self
            .extension_mapping
            .get(extension.as_str())
            .map(|mime| mime.as_ref())
            .or_else(|| {
                MIME_DETECTOR
                    .get_from_path(&source_file.path)
                    .ok()
                    .flatten()
                    .map(|kind| kind.mime_type())
            })?;
        let (top_level, subtype) = mime.split_once('/')?;
        if top_level != "application" {
            return Some(top_level.to_string());
        }
        self.mapping.get(subtype).map(|tag| tag.to_string())
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
    async fn tags_text_extensions_and_subtitles() {
        for path in ["notes.txt", "theme.css", "release.nfo"] {
            assert_eq!(Some("text".to_string()), tag(path, Map::new()).await);
        }

        for path in [
            "show.srt",
            "show.ass",
            "show.ssa",
            "show.vtt",
            "show.sub",
            "show.sup",
            "show.idx",
            "show.smi",
            "show.sami",
            "show.ttml",
            "show.dfxp",
            "show.scc",
        ] {
            assert_eq!(Some("subtitle".to_string()), tag(path, Map::new()).await);
        }
    }

    #[tokio::test]
    async fn detects_common_top_level_types_from_magic_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("video.bin");
        let audio = dir.path().join("audio.bin");
        let image = dir.path().join("image.bin");

        let mut matroska = vec![0; 257];
        matroska[..4].copy_from_slice(b"\x1a\x45\xdf\xa3");
        matroska[16..27].copy_from_slice(b"\x42\x82\x88matroska");
        std::fs::write(&video, matroska).unwrap();
        std::fs::write(&audio, b"fLaC").unwrap();
        std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();

        for (path, expected) in [(&video, "video"), (&audio, "audio"), (&image, "image")]
        {
            assert_eq!(
                Some(expected.to_string()),
                tag(path.to_str().unwrap(), Map::new()).await
            );
        }
    }
    #[tokio::test]
    async fn custom_infer_matcher_detects_existing_ass_content() {
        let dir = tempfile::tempdir().unwrap();
        let subtitle = dir.path().join("show.bin");
        std::fs::write(
            &subtitle,
            "\u{feff}[Script Info]\nScriptType: v4.00+\n[Events]\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,字幕\n",
        )
        .unwrap();

        assert_eq!(
            Some("subtitle".to_string()),
            tag(subtitle.to_str().unwrap(), Map::new()).await
        );
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
        assert_eq!(Some("captions".to_string()), tag("show.ass", props).await);
    }
    #[tokio::test]
    async fn extension_mapping_accepts_tika_style_globs() {
        let props = Map::from_iter([(
            "extension-mapping".to_string(),
            Value::Object(Map::from_iter([(
                "*.CAP".to_string(),
                Value::String("application/x-subrip".to_string()),
            )])),
        )]);

        assert_eq!(Some("subtitle".to_string()), tag("show.cap", props).await);
    }
    #[tokio::test]
    async fn rejects_extensionless_and_unknown_files() {
        assert_eq!(None, tag("README", Map::new()).await);
        assert_eq!(None, tag("file.unknown", Map::new()).await);
    }
}

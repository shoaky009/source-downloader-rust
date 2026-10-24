use anitomy::ElementKind;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct AnitomVariableProviderSupplier;
pub const SUPPLIER: AnitomVariableProviderSupplier = AnitomVariableProviderSupplier;

impl ComponentSupplier for AnitomVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("anitom".to_string())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(AnitomVariableProvider))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: concat!(
                "Parses filenames locally with Anitomy into animeTitle, animeSeason, ",
                "episodeNumber, animeType, releaseGroup, videoResolution and other ",
                "raw variables. These names are not aliases of season or episode.",
            )
            .into(),
            props_json_schema: None,
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
            #[rustfmt::skip]
            output_schema: Some(source_downloader_sdk::serde_json::json!(
            {
                "type": "object",
                "properties": {
                    "fileVariables": {
                        "type": "object",
                        "properties": {
                            "audioTerm": {"type": "string"},
                            "deviceCompatibility": {"type": "string"},
                            "episodeNumber": {"type": "string"},
                            "episodeTitle": {"type": "string"},
                            "episodeNumberAlt": {"type": "string"},
                            "fileChecksum": {"type": "string"},
                            "fileExtension": {"type": "string"},
                            "language": {"type": "string"},
                            "other": {"type": "string"},
                            "releaseGroup": {"type": "string"},
                            "releaseInformation": {"type": "string"},
                            "releaseVersion": {"type": "string"},
                            "animeSeason": {"type": "string"},
                            "source": {"type": "string"},
                            "subtitles": {"type": "string"},
                            "animeTitle": {"type": "string"},
                            "animeType": {"type": "string"},
                            "videoResolution": {"type": "string"},
                            "videoTerm": {"type": "string"},
                            "volumeNumber": {"type": "string"},
                            "animeYear": {"type": "string"},
                            "date": {"type": "string"}
                        },
                        "additionalProperties": false,
                        "description": "Parses filename (file stage) or input text (variable-process); item stage emits no variables. All elements are optional strings. Repeated elements with the same key collapse to one value."
                    },
                    "extractedVariables": {
                        "type": "object",
                        "properties": {
                            "audioTerm": {"type": "string"},
                            "deviceCompatibility": {"type": "string"},
                            "episodeNumber": {"type": "string"},
                            "episodeTitle": {"type": "string"},
                            "episodeNumberAlt": {"type": "string"},
                            "fileChecksum": {"type": "string"},
                            "fileExtension": {"type": "string"},
                            "language": {"type": "string"},
                            "other": {"type": "string"},
                            "releaseGroup": {"type": "string"},
                            "releaseInformation": {"type": "string"},
                            "releaseVersion": {"type": "string"},
                            "animeSeason": {"type": "string"},
                            "source": {"type": "string"},
                            "subtitles": {"type": "string"},
                            "animeTitle": {"type": "string"},
                            "animeType": {"type": "string"},
                            "videoResolution": {"type": "string"},
                            "videoTerm": {"type": "string"},
                            "volumeNumber": {"type": "string"},
                            "animeYear": {"type": "string"},
                            "date": {"type": "string"}
                        },
                        "additionalProperties": false,
                        "description": "Parses filename (file stage) or input text (variable-process); item stage emits no variables. All elements are optional strings. Repeated elements with the same key collapse to one value."
                    }
                },
                "additionalProperties": false
            })),
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
struct AnitomVariableProvider;

impl Display for AnitomVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anitom")
    }
}

#[async_trait]
impl VariableProvider for AnitomVariableProvider {
    async fn item_variables(
        &self,
        _: &SourceItem,
    ) -> Result<PatternVariables, ProcessingError> {
        Ok(HashMap::new())
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Result<Vec<PatternVariables>, ProcessingError> {
        Ok(files
            .iter()
            .map(|file| {
                file.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(parse_variables)
                    .unwrap_or_default()
            })
            .collect())
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Result<Option<HashMap<String, Value>>, ProcessingError> {
        Ok(Some(
            parse_variables(value)
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        ))
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("animeTitle".to_string())
    }
}

fn parse_variables(file_name: &str) -> PatternVariables {
    anitomy::parse(file_name)
        .into_iter()
        .map(|element| {
            (variable_name(element.kind()).to_string(), element.value().to_string())
        })
        .collect()
}

fn variable_name(kind: ElementKind) -> &'static str {
    match kind {
        ElementKind::AudioTerm => "audioTerm",
        ElementKind::DeviceCompatibility => "deviceCompatibility",
        ElementKind::Episode => "episodeNumber",
        ElementKind::EpisodeTitle => "episodeTitle",
        ElementKind::EpisodeAlt => "episodeNumberAlt",
        ElementKind::FileChecksum => "fileChecksum",
        ElementKind::FileExtension => "fileExtension",
        ElementKind::Language => "language",
        ElementKind::Other => "other",
        ElementKind::ReleaseGroup => "releaseGroup",
        ElementKind::ReleaseInformation => "releaseInformation",
        ElementKind::ReleaseVersion => "releaseVersion",
        ElementKind::Season => "animeSeason",
        ElementKind::Source => "source",
        ElementKind::Subtitles => "subtitles",
        ElementKind::Title => "animeTitle",
        ElementKind::Type => "animeType",
        ElementKind::VideoResolution => "videoResolution",
        ElementKind::VideoTerm => "videoTerm",
        ElementKind::Volume => "volumeNumber",
        ElementKind::Year => "animeYear",
        ElementKind::Date => "date",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
    use std::path::PathBuf;

    fn item() -> SourceItem {
        SourceItem {
            title: String::new(),
            link: Uri::from_static("https://example.com"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: String::new(),
            download_uri: Uri::from_static("https://example.com/file"),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        }
    }

    #[test]
    fn supplier_supports_anitom_without_properties() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::variable_provider("anitom".to_string())]
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
    async fn parses_anime_filename_into_kotlin_compatible_variable_names() {
        let provider = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::new(),
            )
            .unwrap()
            .as_variable_provider()
            .unwrap();
        let files = [SourceFile::new(PathBuf::from(
            "[TaigaSubs]_Toradora!_(2008)_-_01v2_-_Tiger_and_Dragon_[1280x720_H.264_FLAC][1234ABCD].mkv",
        ))];

        let variables =
            provider.file_variables(&item(), &HashMap::new(), &files).await.unwrap();

        assert_eq!(variables[0].get("animeTitle").map(String::as_str), Some("Toradora!"));
        assert_eq!(variables[0].get("episodeNumber").map(String::as_str), Some("01"));
        assert_eq!(variables[0].get("releaseVersion").map(String::as_str), Some("2"));
        assert_eq!(
            variables[0].get("videoResolution").map(String::as_str),
            Some("1280x720")
        );
        assert_eq!(provider.primary_variable_name().as_deref(), Some("animeTitle"));
    }

    #[tokio::test]
    async fn returns_one_variable_map_per_file() {
        let provider = AnitomVariableProvider;
        let files = [
            SourceFile::new(PathBuf::from("[Group] Show - 01.mkv")),
            SourceFile::new(PathBuf::from("notes.txt")),
        ];

        let variables =
            provider.file_variables(&item(), &HashMap::new(), &files).await.unwrap();
        assert_eq!(files.len(), variables.len());
        assert!(provider.item_variables(&item()).await.unwrap().is_empty());
    }
}

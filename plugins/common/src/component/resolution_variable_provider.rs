use regex::Regex;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct ResolutionVariableProviderSupplier;
pub const SUPPLIER: ResolutionVariableProviderSupplier =
    ResolutionVariableProviderSupplier;

impl ComponentSupplier for ResolutionVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("resolution".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let only_high_resolution = props
            .get("only-high-resolution")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    ComponentError::new("Invalid 'only-high-resolution' property")
                })
            })
            .transpose()?
            .unwrap_or(true);
        Ok(Arc::new(ResolutionVariableProvider { only_high_resolution }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(VariableProvider)]
struct ResolutionVariableProvider {
    only_high_resolution: bool,
}

impl Display for ResolutionVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "resolution")
    }
}

static RESOLUTIONS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        ("(?i)1920x1080", "FullHD"),
        ("(?i)1280x720", "HD"),
        ("(?i)2560x1440", "2K"),
        ("(?i)3840x2160", "4K"),
        ("(?i)7680x4320", "8K"),
        ("4K", "4K"),
        ("2K", "2K"),
        ("8K", "8K"),
    ]
    .into_iter()
    .map(|(pattern, value)| {
        (Regex::new(pattern).expect("static regex must compile"), value)
    })
    .collect()
});

#[async_trait]
impl VariableProvider for ResolutionVariableProvider {
    async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
        HashMap::new()
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        files
            .iter()
            .map(|file| {
                let stem =
                    file.path.file_stem().and_then(|value| value.to_str()).unwrap_or("");
                RESOLUTIONS
                    .iter()
                    .filter(|(_, resolution)| {
                        !self.only_high_resolution || !resolution.contains("HD")
                    })
                    .find_map(|(regex, resolution)| {
                        regex.is_match(stem).then(|| {
                            HashMap::from([(
                                "resolution".to_string(),
                                (*resolution).to_string(),
                            )])
                        })
                    })
                    .unwrap_or_default()
            })
            .collect()
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        RESOLUTIONS
            .iter()
            .filter(|(_, resolution)| {
                !self.only_high_resolution || !resolution.contains("HD")
            })
            .find_map(|(regex, resolution)| {
                regex.is_match(value).then(|| {
                    HashMap::from([(
                        "resolution".to_string(),
                        Value::String((*resolution).to_string()),
                    )])
                })
            })
    }
    fn primary_variable_name(&self) -> Option<String> {
        None
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
    async fn resolutions(
        only_high_resolution: bool,
        names: &[&str],
    ) -> Vec<Option<String>> {
        let files = names
            .iter()
            .map(|name| SourceFile::new(PathBuf::from(name)))
            .collect::<Vec<_>>();
        ResolutionVariableProvider { only_high_resolution }
            .file_variables(&item(), &HashMap::new(), &files)
            .await
            .into_iter()
            .map(|variables| variables.get("resolution").cloned())
            .collect()
    }

    #[test]
    fn supplier_uses_kebab_case_default_and_validation() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::variable_provider("resolution".to_string())]
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
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::from_iter([(
                        "onlyHighResolution".to_string(),
                        Value::Bool(false),
                    )]),
                )
                .is_ok()
        );
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::from_iter([(
                        "only-high-resolution".to_string(),
                        Value::String("true".to_string()),
                    )]),
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn maps_dimensions_and_labels_in_declared_order() {
        assert_eq!(
            vec![
                Some("FullHD".to_string()),
                Some("HD".to_string()),
                Some("2K".to_string()),
                Some("4K".to_string()),
                Some("8K".to_string()),
                None
            ],
            resolutions(
                false,
                &[
                    "1920X1080.mkv",
                    "1280x720.mkv",
                    "2560x1440 4K.mkv",
                    "3840x2160.mkv",
                    "7680x4320.mkv",
                    "4k.mkv"
                ]
            )
            .await
        );
    }

    #[tokio::test]
    async fn high_resolution_mode_excludes_both_full_hd_and_hd() {
        assert_eq!(
            vec![None, None, Some("2K".to_string()), Some("4K".to_string())],
            resolutions(true, &["1920x1080.mkv", "1280x720.mkv", "2K.mkv", "4K.mkv"])
                .await
        );
    }
}

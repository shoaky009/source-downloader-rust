use crate::expression::cel::FACTORY;
use crate::expression::{
    CompiledExpression, CompiledExpressionFactory, file_content_variables,
};
use serde::Deserialize;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileContentFilter,
    SdComponent, SdComponentMetadata, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use tracing::warn;

pub struct ExpressionFileContentFilterSupplier;
pub const SUPPLIER: ExpressionFileContentFilterSupplier =
    ExpressionFileContentFilterSupplier {};

impl ComponentSupplier for ExpressionFileContentFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_content_filter("expression".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let cfg = deserialize_component_config::<Cfg>(props)?;
        let mut exclusions = Vec::new();
        for (index, x) in cfg.exclusions.into_iter().enumerate() {
            exclusions.push(FACTORY.create(&x).map_err(|error| {
                ComponentError::new(format!(
                    "Invalid configuration at 'exclusions[{index}]': {error}"
                ))
            })?);
        }

        let mut inclusions = Vec::new();
        for (index, x) in cfg.inclusions.into_iter().enumerate() {
            inclusions.push(FACTORY.create(&x).map_err(|error| {
                ComponentError::new(format!(
                    "Invalid configuration at 'inclusions[{index}]': {error}"
                ))
            })?);
        }

        Ok(Arc::new(ExpressionFileContentFilter { exclusions, inclusions }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description:
                "Filters file content using inclusion and exclusion expressions."
                    .to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "exclusions":{
                        "type":"array",
                        "items":{"type":"string"}
                    },
                    "inclusions":{
                        "type":"array",
                        "items":{"type":"string"}
                    }
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(SdComponent)]
#[component(FileContentFilter)]
pub struct ExpressionFileContentFilter {
    exclusions: Vec<Box<dyn CompiledExpression<bool>>>,
    inclusions: Vec<Box<dyn CompiledExpression<bool>>>,
}

impl ExpressionFileContentFilter {
    pub fn new(
        exclusions: Vec<Box<dyn CompiledExpression<bool>>>,
        inclusions: Vec<Box<dyn CompiledExpression<bool>>>,
    ) -> Self {
        Self { exclusions, inclusions }
    }
}

#[derive(Deserialize)]
struct Cfg {
    #[serde(default)]
    exclusions: Vec<String>,
    #[serde(default)]
    inclusions: Vec<String>,
}

impl Debug for ExpressionFileContentFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpressionFileContentFilter")
            .field("exclusions", &self.exclusions.len())
            .field("inclusions", &self.inclusions.len())
            .finish()
    }
}

impl Display for ExpressionFileContentFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "expression")
    }
}

impl FileContentFilter for ExpressionFileContentFilter {
    fn filter(&self, file: &FileContent) -> bool {
        if self.exclusions.is_empty() && self.inclusions.is_empty() {
            return true;
        }

        let file_vars = file_content_variables(file);
        if self.exclusions.iter().any(|expr| {
            expr.execute(&file_vars)
                .inspect_err(|e| {
                    warn!(
                        "Exclusions expression execution error will be false, error: {e}"
                    )
                })
                .unwrap_or(false)
        }) {
            return false;
        }
        if self.inclusions.is_empty() {
            return true;
        }
        self.inclusions.iter().all(|expr| {
            expr.execute(&file_vars)
                .inspect_err(|e| {
                    warn!(
                        "Inclusions expression execution error will be false, error: {e}"
                    )
                })
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod test {
    use crate::components::expression_file_content_filter::ExpressionFileContentFilter;
    use crate::expression::CompiledExpressionFactory;
    use crate::expression::cel::FACTORY;
    use maplit::hashmap;
    use source_downloader_sdk::component::{FileContent, FileContentFilter};
    use std::path::PathBuf;
    use std::str::FromStr;

    #[test]
    fn test_simple_exclusions() {
        let filter = ExpressionFileContentFilter::expressions(
            vec!["file.name == '1.txt'"],
            vec![],
        );

        let test_file_content1 = FileContent {
            file_download_path: PathBuf::from("1.txt"),
            ..Default::default()
        };
        assert!(!filter.filter(&test_file_content1));

        let test_file_content2 = FileContent {
            file_download_path: PathBuf::from("2.txt"),
            ..Default::default()
        };
        assert!(filter.filter(&test_file_content2));
    }

    #[test]
    fn test_simple_inclusions() {
        let filter = ExpressionFileContentFilter::expressions(
            vec![],
            vec!["file.name == '1.txt'"],
        );

        let test_file_content1 = FileContent {
            file_download_path: PathBuf::from("1.txt"),
            ..Default::default()
        };
        assert!(filter.filter(&test_file_content1));

        let test_file_content2 = FileContent {
            file_download_path: PathBuf::from("2.txt"),
            ..Default::default()
        };
        assert!(!filter.filter(&test_file_content2));
    }

    #[test]
    fn test_multiple() {
        let filter = ExpressionFileContentFilter::expressions(
            vec!["file.attrs.size > 1024*1024", "file.name.matches('.*qaz.*')"],
            vec!["file.attrs.size < 1024*1024", "file.name.matches('.*Test.*')"],
        );

        let test_file_content1 = FileContent {
            file_download_path: PathBuf::from_iter(vec![
                "src",
                "test",
                "kotlin",
                "io",
                "github",
                "shoaky",
                "sourcedownloader",
                "component",
                "ExpressionFileFilterTest.kt",
            ]),
            attrs: serde_json::Map::from_str(r#"{"size":1}"#).unwrap(),
            ..Default::default()
        };
        assert!(filter.filter(&test_file_content1));
    }

    #[test]
    fn test_all_variables() {
        let filter = ExpressionFileContentFilter::expressions(
            vec![],
            vec![
                "file.name.contains('test') &&
                'video' in file.tags &&
                file.extension == 'txt' &&
                file.vars.test == 'test' &&
                file.attrs.size > 10 &&
                'book' in file.paths",
            ],
        );
        let download_path = PathBuf::from_iter(vec!["src", "test", "resources"]);
        let test_file_content1 = FileContent {
            file_download_path: download_path.join("book").join("test.txt"),
            download_path,
            tags: vec!["video".to_string()],
            attrs: serde_json::Map::from_str(r#"{"size":100}"#).unwrap(),
            pattern_variables: hashmap! {
              "test".to_owned() => "test".to_owned(),
            },
            ..Default::default()
        };
        assert!(filter.filter(&test_file_content1));
    }

    #[test]
    fn test_contains_any() {
        let filter = ExpressionFileContentFilter::expressions(
            vec!["file.paths.containsAny(['SPs'], false)"],
            vec![],
        );
        let download_path =
            PathBuf::from_iter(vec!["src", "test", "resources", "downloads"]);
        let test_file_content1 = FileContent {
            file_download_path: download_path.join("SPs").join("test.txt"),
            download_path: download_path.clone(),
            ..Default::default()
        };
        assert!(!filter.filter(&test_file_content1));
        let test_file_content2 = FileContent {
            file_download_path: download_path.join("sps").join("test.txt"),
            ..test_file_content1
        };
        assert!(filter.filter(&test_file_content2));

        // ignore_case
        let filter = ExpressionFileContentFilter::expressions(
            vec!["file.paths.containsAny(['sp', 'sps', 'extra'], true)"],
            vec![],
        );
        let test_file_content3 = FileContent {
            file_download_path: download_path.join("SP").join("test.txt"),
            ..test_file_content2
        };
        assert!(!filter.filter(&test_file_content3));
    }

    impl ExpressionFileContentFilter {
        fn expressions(
            exclusions: Vec<&str>,
            inclusions: Vec<&str>,
        ) -> ExpressionFileContentFilter {
            let exclusions =
                exclusions.iter().map(|x| FACTORY.create(x).unwrap()).collect();
            let inclusions =
                inclusions.iter().map(|x| FACTORY.create(x).unwrap()).collect();
            ExpressionFileContentFilter { exclusions, inclusions }
        }
    }
}

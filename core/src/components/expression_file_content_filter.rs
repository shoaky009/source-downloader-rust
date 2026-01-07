use crate::expression::cel::FACTORY;
use crate::expression::{CompiledExpression, CompiledExpressionFactory, file_content_variables};
use sdk::SdComponent;
use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileContentFilter, SdComponent,
    SdComponentMetadata,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tracing::warn;

pub struct ExpressionFileContentFilterSupplier;
pub const SUPPLIER: ExpressionFileContentFilterSupplier = ExpressionFileContentFilterSupplier {};

impl ComponentSupplier for ExpressionFileContentFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_content_filter("expression".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let val = serde_json::to_value(props)
            .map_err(|e| ComponentError::new(format!("Failed to parse config: {}", e)))?;
        let cfg = serde_json::from_value::<Cfg>(val)
            .map_err(|e| ComponentError::new(format!("Failed to convert config: {}", e)))?;
        let mut exclusions = Vec::new();
        for x in cfg.exclusions {
            exclusions.push(FACTORY.create(&x)?);
        }

        let mut inclusions = Vec::new();
        for x in cfg.inclusions {
            inclusions.push(FACTORY.create(&x)?);
        }

        Ok(Arc::new(ExpressionFileContentFilter {
            exclusions,
            inclusions,
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(SdComponent)]
#[component(FileContentFilter)]
pub struct ExpressionFileContentFilter {
    exclusions: Vec<Box<dyn CompiledExpression<bool>>>,
    inclusions: Vec<Box<dyn CompiledExpression<bool>>>,
}

#[derive(Deserialize)]
struct Cfg {
    #[serde(default)]
    exclusions: Vec<String>,
    #[serde(default)]
    inclusions: Vec<String>,
}

impl Debug for ExpressionFileContentFilter {
    fn fmt(&self, _: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
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
                    warn!("Exclusions expression execution error will be false, error: {e}")
                })
                .unwrap_or(false)
        }) {
            return false;
        }
        if self.inclusions.is_empty() {
            return true;
        }
        self.inclusions.iter().any(|expr| {
            expr.execute(&file_vars)
                .inspect_err(|e| {
                    warn!("Inclusions expression execution error will be false, error: {e}")
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
    use sdk::component::{FileContent, FileContentFilter};
    use std::path::PathBuf;
    use std::str::FromStr;

    #[test]
    fn test_simple_exclusions() {
        let filter = ExpressionFileContentFilter::expressions(vec!["file.name == '1.txt'"], vec![]);

        let test_file_content1 = FileContent {
            file_download_path: PathBuf::from("1.txt"),
            ..Default::default()
        };
        assert_eq!(false, filter.filter(&test_file_content1));

        let test_file_content2 = FileContent {
            file_download_path: PathBuf::from("2.txt"),
            ..Default::default()
        };
        assert_eq!(true, filter.filter(&test_file_content2));
    }

    #[test]
    fn test_simple_inclusions() {
        let filter = ExpressionFileContentFilter::expressions(vec![], vec!["file.name == '1.txt'"]);

        let test_file_content1 = FileContent {
            file_download_path: PathBuf::from("1.txt"),
            ..Default::default()
        };
        assert_eq!(true, filter.filter(&test_file_content1));

        let test_file_content2 = FileContent {
            file_download_path: PathBuf::from("2.txt"),
            ..Default::default()
        };
        assert_eq!(false, filter.filter(&test_file_content2));
    }

    #[test]
    fn test_multiple() {
        let filter = ExpressionFileContentFilter::expressions(
            vec![
                "file.attrs.size > 1024*1024",
                "file.name.matches('.*qaz.*')",
            ],
            vec![
                "file.attrs.size < 1024*1024",
                "file.name.matches('.*Test.*')",
            ],
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
        assert_eq!(true, filter.filter(&test_file_content1));
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
        assert_eq!(true, filter.filter(&test_file_content1));
    }

    #[test]
    fn test_contains_any() {
        let filter = ExpressionFileContentFilter::expressions(
            vec!["file.paths.containsAny(['SPs'], false)"],
            vec![],
        );
        let download_path = PathBuf::from_iter(vec!["src", "test", "resources", "downloads"]);
        let test_file_content1 = FileContent {
            file_download_path: download_path.join("SPs").join("test.txt"),
            download_path: download_path.clone(),
            ..Default::default()
        };
        assert_eq!(false, filter.filter(&test_file_content1));
        let test_file_content2 = FileContent {
            file_download_path: download_path.join("sps").join("test.txt"),
            ..test_file_content1
        };
        assert_eq!(true, filter.filter(&test_file_content2));

        // ignore_case
        let filter = ExpressionFileContentFilter::expressions(
            vec!["file.paths.containsAny(['sp', 'sps', 'extra'], true)"],
            vec![],
        );
        let test_file_content3 = FileContent {
            file_download_path: download_path.join("SP").join("test.txt"),
            ..test_file_content2
        };
        assert_eq!(false, filter.filter(&test_file_content3));
    }

    impl ExpressionFileContentFilter {
        fn expressions(
            exclusions: Vec<&str>,
            inclusions: Vec<&str>,
        ) -> ExpressionFileContentFilter {
            let exclusions = exclusions
                .iter()
                .map(|x| FACTORY.create(x).unwrap())
                .collect();
            let inclusions = inclusions
                .iter()
                .map(|x| FACTORY.create(x).unwrap())
                .collect();
            ExpressionFileContentFilter {
                exclusions,
                inclusions,
            }
        }
    }
}

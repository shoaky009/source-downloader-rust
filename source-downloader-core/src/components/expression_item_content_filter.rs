use crate::expression::cel::FACTORY;
use crate::expression::{
    CompiledExpression, CompiledExpressionFactory, item_content_variables,
};
use serde::Deserialize;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemContent, ItemContentFilter,
    SdComponent, SdComponentMetadata, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use tracing::warn;

pub struct ExpressionItemContentFilterSupplier;
pub const SUPPLIER: ExpressionItemContentFilterSupplier =
    ExpressionItemContentFilterSupplier {};

impl ComponentSupplier for ExpressionItemContentFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::item_content_filter("expression".to_string())]
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

        Ok(Arc::new(ExpressionItemContentFilter { exclusions, inclusions }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description:
                "Filters item content using inclusion and exclusion expressions."
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
#[component(ItemContentFilter)]
pub struct ExpressionItemContentFilter {
    exclusions: Vec<Box<dyn CompiledExpression<bool>>>,
    inclusions: Vec<Box<dyn CompiledExpression<bool>>>,
}

impl ExpressionItemContentFilter {
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

impl Debug for ExpressionItemContentFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExpressionItemContentFilter")
            .field("exclusions", &self.exclusions.len())
            .field("inclusions", &self.inclusions.len())
            .finish()
    }
}

impl Display for ExpressionItemContentFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "expression")
    }
}

#[async_trait::async_trait]
impl ItemContentFilter for ExpressionItemContentFilter {
    async fn filter(&self, item_content: &ItemContent) -> bool {
        if self.exclusions.is_empty() && self.inclusions.is_empty() {
            return true;
        }

        let item_var = item_content_variables(item_content);
        if self.exclusions.iter().any(|expr| {
            expr.execute(&item_var)
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
            expr.execute(&item_var)
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
    use crate::components::expression_item_content_filter::SUPPLIER;
    use serde::Deserialize;
    use serde_json::{Map, Value};
    use serde_yaml::from_str;
    use source_downloader_sdk::SourceItem;
    use source_downloader_sdk::component::{ComponentSupplier, ItemContent};
    use source_downloader_sdk::storage::ProcessingStatus;
    use std::collections::HashMap;
    use std::fs::File;
    use std::path::Path;

    #[tokio::test]
    async fn test_all() {
        let path = Path::new("./tests/component/expression_item_filter_test_data.json");
        let file = File::open(path).unwrap();
        let test_data: Vec<TestData> = serde_json::from_reader(file).unwrap();
        let json = r#"{"title":"test","link":"localhost", "downloadUri":"localhost", "contentType":"txt", "datetime": "2025-12-05T10:07:53+09:00"}"#;
        let default_item: SourceItem = from_str(json).unwrap();
        for data in &test_data {
            let mut props = Map::new();
            props.insert("exclusions".into(), Value::from(data.exclusions.clone()));
            props.insert("inclusions".into(), Value::from(data.inclusions.clone()));
            let filter = SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &props,
                )
                .unwrap()
                .as_item_content_filter()
                .unwrap();
            let item = data.item.as_ref().unwrap_or(&default_item);
            let files = Vec::new();
            let variables = HashMap::new();
            let content = ItemContent {
                source_item: item,
                file_contents: &files,
                item_variables: &variables,
                status: ProcessingStatus::WaitingToRename,
            };
            let actual = filter.filter(&content).await;
            let expected = data.expected;
            assert_eq!(expected, actual, "{:#?}", data);
        }
    }

    #[derive(Deserialize, Debug, Clone)]
    struct TestData {
        #[serde(default)]
        exclusions: Vec<String>,
        #[serde(default)]
        inclusions: Vec<String>,
        expected: bool,
        #[serde(default)]
        item: Option<SourceItem>,
    }
}

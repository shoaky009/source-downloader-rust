use crate::expression::cel::FACTORY;
use crate::expression::{CompiledExpression, CompiledExpressionFactory, source_item_variables};
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    SourceItemFilter,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use tracing::warn;

pub struct ExpressionItemFilterSupplier;
pub const SUPPLIER: ExpressionItemFilterSupplier = ExpressionItemFilterSupplier {};

impl ComponentSupplier for ExpressionItemFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::item_filter("expression".to_string())]
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

        Ok(Arc::new(ExpressionItemFilter {
            exclusions,
            inclusions,
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(SdComponent)]
#[component(SourceItemFilter)]
pub struct ExpressionItemFilter {
    exclusions: Vec<Box<dyn CompiledExpression<bool>>>,
    inclusions: Vec<Box<dyn CompiledExpression<bool>>>,
}

impl ExpressionItemFilter {
    pub fn new(
        exclusions: Vec<Box<dyn CompiledExpression<bool>>>,
        inclusions: Vec<Box<dyn CompiledExpression<bool>>>,
    ) -> Self {
        Self {
            exclusions,
            inclusions,
        }
    }
}

#[derive(Deserialize)]
struct Cfg {
    #[serde(default)]
    exclusions: Vec<String>,
    #[serde(default)]
    inclusions: Vec<String>,
}

impl Debug for ExpressionItemFilter {
    fn fmt(&self, _: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl Display for ExpressionItemFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "expression")
    }
}

#[async_trait::async_trait]
impl SourceItemFilter for ExpressionItemFilter {
    async fn filter(&self, item: &SourceItem) -> bool {
        if self.exclusions.is_empty() && self.inclusions.is_empty() {
            return true;
        }

        let item_var = source_item_variables(item);
        if self.exclusions.iter().any(|expr| {
            expr.execute(&item_var)
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
            expr.execute(&item_var)
                .inspect_err(|e| {
                    warn!("Inclusions expression execution error will be false, error: {e}")
                })
                .unwrap_or(false)
        })
    }
}

#[cfg(test)]
mod test {
    use crate::components::expression_item_filter::SUPPLIER;
    use serde::Deserialize;
    use serde_json::{Map, Value};
    use serde_yaml::from_str;
    use source_downloader_sdk::SourceItem;
    use source_downloader_sdk::component::ComponentSupplier;
    use std::fs::File;
    use std::path::Path;

    #[tokio::test]
    async fn test_all() {
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
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
                .apply(&props)
                .unwrap()
                .as_source_item_filter()
                .unwrap();
            let item = data.item.as_ref().unwrap_or(&default_item);
            let p = item.clone();
            let actual = filter.filter(&p).await;
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

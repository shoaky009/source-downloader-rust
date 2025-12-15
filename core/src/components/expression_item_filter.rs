use crate::{CelCompiledExpressionFactory, CompiledExpression, CompiledExpressionFactory};
use sdk::SdComponent;
use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFilter, PointedItem, SdComponent,
    SdComponentMetadata,
};
use sdk::serde::Deserialize;
use sdk::serde_json::{Map, Value};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use tracing::warn;

pub struct ExpressionItemFilterSupplier;
pub const SUPPLIER: ExpressionItemFilterSupplier = ExpressionItemFilterSupplier {};

impl ComponentSupplier for ExpressionItemFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::item_filter("expression".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let fac = CelCompiledExpressionFactory {};
        let val = serde_json::to_value(props)
            .map_err(|e| ComponentError::new(format!("Failed to parse config: {}", e)))?;
        let cfg = serde_json::from_value::<Cfg>(val)
            .map_err(|e| ComponentError::new(format!("Failed to convert config: {}", e)))?;
        let mut exclusions = Vec::new();
        for x in cfg.exclusions {
            exclusions.push(fac.create(&x)?);
        }

        let mut inclusions = Vec::new();
        for x in cfg.inclusions {
            inclusions.push(fac.create(&x)?);
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
#[component(ItemFilter)]
struct ExpressionItemFilter {
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

impl Debug for ExpressionItemFilter {
    fn fmt(&self, _: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl ItemFilter for ExpressionItemFilter {
    fn filter(&self, item: &PointedItem) -> bool {
        if self.exclusions.is_empty() && self.inclusions.is_empty() {
            return true;
        }

        let mut vars = Map::new();
        vars.insert(
            "title".to_owned(),
            Value::from(item.source_item.title.to_owned()),
        );
        vars.insert(
            "datetime".to_owned(),
            Value::from(item.source_item.link.to_string()),
        );
        vars.insert(
            "year".to_owned(),
            Value::from(item.source_item.datetime.year()),
        );
        vars.insert(
            "month".to_owned(),
            Value::from(item.source_item.datetime.month() as u8),
        );
        vars.insert(
            "link".to_owned(),
            Value::from(item.source_item.link.to_string()),
        );
        vars.insert(
            "downloadUri".to_owned(),
            Value::from(item.source_item.download_uri.to_string()),
        );
        vars.insert(
            "contentType".to_owned(),
            Value::from(item.source_item.content_type.to_string()),
        );
        vars.insert(
            "tags".to_owned(),
            Value::from(
                item.source_item
                    .tags
                    .iter()
                    .map(|x| Value::from(x.to_string()))
                    .collect::<Vec<Value>>(),
            ),
        );

        vars.insert(
            "attrs".to_owned(),
            Value::from(item.source_item.attrs.to_owned()),
        );

        let mut item_var = Map::new();
        item_var.insert("item".to_string(), Value::Object(vars));
        for excl in &self.exclusions {
            let result = excl.execute(&item_var);
            if result.is_err() {
                warn!(
                    "Exclusions expression execution error will be false, error: {}",
                    result.clone().unwrap_err()
                );
            }
            if result.unwrap_or(false) {
                return false;
            }
        }
        if self.inclusions.is_empty() {
            return true;
        }
        for incl in &self.inclusions {
            let result = incl.execute(&item_var);
            if result.is_err() {
                warn!(
                    "Inclusions expression execution error will be false, error: {}",
                    result.clone().unwrap_err()
                );
            }

            if result.unwrap_or(false) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod test {
    use crate::components::expression_item_filter::ExpressionItemFilterSupplier;
    use sdk::SourceItem;
    use sdk::component::{ComponentSupplier, PointedItem, empty_pointer};
    use sdk::serde::Deserialize;
    use serde_json::{Map, Value};
    use serde_yaml::from_str;
    use std::fs::File;
    use std::path::Path;

    const SUPPLIER: ExpressionItemFilterSupplier = ExpressionItemFilterSupplier {};

    #[test]
    fn test_all() {
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
            let filter = SUPPLIER.apply(&props).unwrap().as_item_filter().unwrap();
            let item = data.item.as_ref().unwrap_or(&default_item);
            let p = PointedItem {
                source_item: item.clone(), // 这个必要，因为 filter.filter 需要 owned
                pointer: empty_pointer(),
            };
            let actual = filter.filter(&p);
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

use crate::{CelCompiledExpressionFactory, CompiledExpression, CompiledExpressionFactory};
use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFilter, PointedItem, SdComponent,
    SdComponentMetadata,
};
use sdk::{Deserialize, Map, SdComponent, Value};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

pub struct ExpressionItemFilterSupplier {}

impl ComponentSupplier for ExpressionItemFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::item_filter("expression".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let fac = CelCompiledExpressionFactory {};
        let cfg = serde_json::from_value::<Cfg>(Value::Object(props.clone())).map_err(|e| {
            ComponentError::new(format!("Failed to parse ExpressionItemFilter config: {}", e))
        })?;
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
    exclusions: Vec<String>,
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

        for excl in &self.exclusions {
            if excl.execute(&vars).unwrap_or(false) {
                return false;
            }
        }
        if self.inclusions.is_empty() {
            return true;
        }
        for incl in &self.inclusions {
            if incl.execute(&vars).unwrap_or(false) {
                return true;
            }
        }
        false
    }
}

use regex::Regex;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    VariableReplacer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const SUPPLIER: RegexVariableReplacerSupplier = RegexVariableReplacerSupplier;

pub struct RegexVariableReplacerSupplier;

#[derive(Deserialize)]
struct RegexConfig {
    regex: String,
    replacement: String,
}

impl ComponentSupplier for RegexVariableReplacerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_replacer("regex".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: RegexConfig = serde_json::from_value(Value::Object(props.clone()))
            .map_err(|error| {
                ComponentError::new(format!("Invalid regex replacer config: {error}"))
            })?;
        let regex = Regex::new(&config.regex).map_err(|error| {
            ComponentError::new(format!("Invalid replacer regex: {error}"))
        })?;
        Ok(Arc::new(RegexVariableReplacer { regex, replacement: config.replacement }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableReplacer)]
struct RegexVariableReplacer {
    regex: Regex,
    replacement: String,
}

impl VariableReplacer for RegexVariableReplacer {
    fn replace(&self, _: &str, value: String) -> String {
        self.regex.replace_all(&value, self.replacement.as_str()).into_owned()
    }
}

impl Display for RegexVariableReplacer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} -> {}", self.regex, self.replacement)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_matching_variables() {
        let regex = RegexVariableReplacer {
            regex: Regex::new("(?i)bdrip").unwrap(),
            replacement: "BD".to_owned(),
        };
        assert_eq!("Show BD", regex.replace("", "Show BDRip".to_owned()));
    }
}

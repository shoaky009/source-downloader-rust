use regex::Regex;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    Trimmer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const SUPPLIER: RegexTrimmerSupplier = RegexTrimmerSupplier;

pub struct RegexTrimmerSupplier;

#[derive(Deserialize)]
struct RegexConfig {
    regex: String,
}

impl ComponentSupplier for RegexTrimmerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trimmer("regex".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: RegexConfig = serde_json::from_value(Value::Object(props.clone()))
            .map_err(|error| {
                ComponentError::new(format!("Invalid regex trimmer config: {error}"))
            })?;
        let regex = Regex::new(&config.regex).map_err(|error| {
            ComponentError::new(format!("Invalid trimmer regex: {error}"))
        })?;
        Ok(Arc::new(RegexTrimmer { regex }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Trimmer)]
struct RegexTrimmer {
    regex: Regex,
}

impl Display for RegexTrimmer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "regex({})", self.regex)
    }
}

impl Trimmer for RegexTrimmer {
    fn trim(&self, value: String, _: usize) -> String {
        self.regex.replace_all(&value, "").into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_regex_matches() {
        let regex = RegexTrimmer { regex: Regex::new("[0-9]+").unwrap() };
        assert_eq!("ab", regex.trim("a123b".to_owned(), 1));
    }
}

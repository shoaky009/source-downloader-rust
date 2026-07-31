use regex::Regex;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    Trimmer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const FORCE_SUPPLIER: ForceTrimmerSupplier = ForceTrimmerSupplier;
pub const REGEX_SUPPLIER: RegexTrimmerSupplier = RegexTrimmerSupplier;

pub struct ForceTrimmerSupplier;
pub struct RegexTrimmerSupplier;

impl ComponentSupplier for ForceTrimmerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trimmer("force".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(ForceTrimmer))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

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
struct ForceTrimmer;

impl Display for ForceTrimmer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("force")
    }
}

impl Trimmer for ForceTrimmer {
    fn trim(&self, value: String, expect_size: usize) -> String {
        if value.len() <= expect_size {
            return value;
        }
        let mut end = expect_size.min(value.len());
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        value[..end].to_owned()
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
    fn built_in_trimmers_match_kotlin_behavior() {
        assert_eq!("你", ForceTrimmer.trim("你好a".to_owned(), 4));
        let regex = RegexTrimmer { regex: Regex::new("[0-9]+").unwrap() };
        assert_eq!("ab", regex.trim("a123b".to_owned(), 1));
    }
}

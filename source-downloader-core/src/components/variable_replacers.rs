use regex::Regex;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    VariableReplacer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const FULL_WIDTH_SUPPLIER: FullWidthReplacerSupplier = FullWidthReplacerSupplier;
pub const REGEX_SUPPLIER: RegexVariableReplacerSupplier = RegexVariableReplacerSupplier;
pub const WINDOWS_PATH_SUPPLIER: WindowsPathReplacerSupplier =
    WindowsPathReplacerSupplier;

pub struct FullWidthReplacerSupplier;
pub struct RegexVariableReplacerSupplier;
pub struct WindowsPathReplacerSupplier;

impl ComponentSupplier for FullWidthReplacerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_replacer("full-width".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(FullWidthReplacer))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

impl ComponentSupplier for WindowsPathReplacerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_replacer("windows-path".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(WindowsPathReplacer))
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
    replacement: String,
}

impl ComponentSupplier for RegexVariableReplacerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_replacer("regex".to_owned())]
    }

    fn apply(
        &self,
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
struct FullWidthReplacer;

impl VariableReplacer for FullWidthReplacer {
    fn replace(&self, _: &str, value: String) -> String {
        value
            .chars()
            .map(|character| match character {
                '【' => '[',
                '】' => ']',
                '　' => ' ',
                '\u{ff01}'..='\u{ff5e}' => {
                    char::from_u32(character as u32 - 0xfee0).unwrap_or(character)
                }
                character => character,
            })
            .collect()
    }
}

impl Display for FullWidthReplacer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("full-width")
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableReplacer)]
struct WindowsPathReplacer;

impl VariableReplacer for WindowsPathReplacer {
    fn replace(&self, _: &str, value: String) -> String {
        value
            .chars()
            .map(|character| match character {
                '<' => '＜',
                '>' => '＞',
                ':' => '：',
                '"' => '＂',
                '/' => '／',
                '\\' => '＼',
                '|' => '｜',
                '?' => '？',
                '*' => '＊',
                character => character,
            })
            .collect()
    }
}

impl Display for WindowsPathReplacer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("windows-path")
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
    fn built_in_replacers_match_kotlin_behavior() {
        assert_eq!(
            "ABC 123[ok]",
            FullWidthReplacer.replace("", "ＡＢＣ　１２３【ｏｋ】".to_owned())
        );
        assert_eq!("a：b／c＊", WindowsPathReplacer.replace("", "a:b/c*".to_owned()));
        let regex = RegexVariableReplacer {
            regex: Regex::new("(?i)bdrip").unwrap(),
            replacement: "BD".to_owned(),
        };
        assert_eq!("Show BD", regex.replace("", "Show BDRip".to_owned()));
    }
}

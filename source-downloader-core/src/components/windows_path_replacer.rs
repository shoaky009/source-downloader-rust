use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    VariableReplacer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const SUPPLIER: WindowsPathReplacerSupplier = WindowsPathReplacerSupplier;

pub struct WindowsPathReplacerSupplier;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_windows_path_characters() {
        assert_eq!("a：b／c＊", WindowsPathReplacer.replace("", "a:b/c*".to_owned()));
    }
}

use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    VariableReplacer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const SUPPLIER: FullWidthReplacerSupplier = FullWidthReplacerSupplier;

pub struct FullWidthReplacerSupplier;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_full_width_characters() {
        assert_eq!(
            "ABC 123[ok]",
            FullWidthReplacer.replace("", "ＡＢＣ　１２３【ｏｋ】".to_owned())
        );
    }
}

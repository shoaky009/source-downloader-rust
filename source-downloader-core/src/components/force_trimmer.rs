use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    Trimmer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub const SUPPLIER: ForceTrimmerSupplier = ForceTrimmerSupplier;

pub struct ForceTrimmerSupplier;

impl ComponentSupplier for ForceTrimmerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trimmer("force".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(ForceTrimmer))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description:
                "Trims values to the requested size, preserving character boundaries."
                    .to_owned(),
            props_json_schema: None,
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_to_byte_limit_without_breaking_utf8() {
        assert_eq!("你", ForceTrimmer.trim("你好a".to_owned(), 4));
    }
}

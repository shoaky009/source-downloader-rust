use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent,
    FileReplacementDecider, InProcessingItem, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct AlwaysReplaceSupplier;
pub const ALWAYS_SUPPLIER: AlwaysReplaceSupplier = AlwaysReplaceSupplier;

pub struct FileSizeReplacementDeciderSupplier;
pub const SIZE_SUPPLIER: FileSizeReplacementDeciderSupplier =  FileSizeReplacementDeciderSupplier;

impl ComponentSupplier for AlwaysReplaceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_replacement_decider("always".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn source_downloader_sdk::component::SdComponent>, ComponentError> {
        Ok(Arc::new(AlwaysReplace))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

impl ComponentSupplier for FileSizeReplacementDeciderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_replacement_decider("size".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn source_downloader_sdk::component::SdComponent>, ComponentError> {
        Ok(Arc::new(FileSizeReplacementDecider))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(FileReplacementDecider)]
pub struct AlwaysReplace;

impl Display for AlwaysReplace {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("always")
    }
}

impl FileReplacementDecider for AlwaysReplace {
    fn should_replace(
        &self,
        _: &SourceItem,
        _: &FileContent,
        _: Option<&InProcessingItem>,
        _: &SourceFile,
    ) -> bool {
        true
    }
}

#[derive(Debug, SdComponent)]
#[component(FileReplacementDecider)]
pub struct FileSizeReplacementDecider;

impl Display for FileSizeReplacementDecider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("size")
    }
}

impl FileReplacementDecider for FileSizeReplacementDecider {
    fn should_replace(
        &self,
        _: &SourceItem,
        current: &FileContent,
        _: Option<&InProcessingItem>,
        existing_file: &SourceFile,
    ) -> bool {
        let Some(current_size) = current.attrs.get("size").and_then(value_as_i64) else {
            return false;
        };
        let existing_size =
            existing_file.attrs.get("size").and_then(value_as_i64).unwrap_or(i64::MAX);
        current_size > existing_size
    }
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_decider_accepts_string_sizes() {
        let mut current = FileContent::default();
        current.attrs.insert(String::from("size"), Value::String(String::from("10")));
        let mut existing = SourceFile::default();
        existing.attrs.insert(String::from("size"), Value::from(9));

        assert!(FileSizeReplacementDecider.should_replace(
            &SourceItem::default(),
            &current,
            None,
            &existing,
        ));
    }
}

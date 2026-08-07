use serde_json::{Map, Value};
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent,
    FileReplacementDecider, InProcessingItem, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct AlwaysReplaceSupplier;
pub const SUPPLIER: AlwaysReplaceSupplier = AlwaysReplaceSupplier;

impl ComponentSupplier for AlwaysReplaceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_replacement_decider("always".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn source_downloader_sdk::component::SdComponent>, ComponentError>
    {
        Ok(Arc::new(AlwaysReplace))
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

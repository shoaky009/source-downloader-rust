use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent,
    FileReplacementDecider, InProcessingItem, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct NeverReplaceDeciderSupplier;
pub const SUPPLIER: NeverReplaceDeciderSupplier = NeverReplaceDeciderSupplier;

impl ComponentSupplier for NeverReplaceDeciderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_replacement_decider("never".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn source_downloader_sdk::component::SdComponent>, ComponentError>
    {
        Ok(Arc::new(NeverReplaceDecider))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }

    fn is_support_no_props(&self) -> bool {
        true
    }
}

#[derive(Debug, SdComponent)]
#[component(FileReplacementDecider)]
pub struct NeverReplaceDecider;

impl Display for NeverReplaceDecider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "never")
    }
}

impl FileReplacementDecider for NeverReplaceDecider {
    fn should_replace(
        &self,
        _: &SourceItem,
        _: &FileContent,
        _: Option<&InProcessingItem>,
        _: &SourceFile,
    ) -> bool {
        false
    }
}

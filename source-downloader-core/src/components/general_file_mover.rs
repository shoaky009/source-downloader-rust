use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileMover, SdComponent,
    SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct GeneralFileMoverSupplier;
pub const SUPPLIER: GeneralFileMoverSupplier = GeneralFileMoverSupplier;

impl ComponentSupplier for GeneralFileMoverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_mover("general".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(GeneralFileMover))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(FileMover)]
pub struct GeneralFileMover;

impl Display for GeneralFileMover {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("general")
    }
}

impl FileMover for GeneralFileMover {}

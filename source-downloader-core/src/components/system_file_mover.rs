use serde_json::{Map, Value};
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileMover, ItemContent, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile,
};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct SystemFileMoverSupplier {}
pub const SUPPLIER: SystemFileMoverSupplier = SystemFileMoverSupplier {};
const INSTANCE: SystemFileMover = SystemFileMover {};

impl ComponentSupplier for SystemFileMoverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_mover("system-file".to_owned())]
    }

    fn apply(&self, _: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(INSTANCE))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        todo!()
    }
}

#[derive(SdComponent, Debug)]
#[component(FileMover)]
struct SystemFileMover {}

impl Display for SystemFileMover {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "system-file")
    }
}

#[allow(dead_code, unused)]
impl FileMover for SystemFileMover {
    fn move_file(
        &self,
        source_file: &SourceFile,
        download_path: &str,
    ) -> Result<(), ProcessingError> {
        todo!()
    }

    fn exists(&self, path: &Vec<&PathBuf>) -> Vec<bool> {
        todo!()
    }

    fn create_directories(&self, path: &str) -> Result<(), ProcessingError> {
        todo!()
    }

    fn replace(&self, item_content: &ItemContent) -> Result<(), ProcessingError> {
        todo!()
    }

    fn list_files(&self, path: &str) -> Vec<String> {
        todo!()
    }

    fn path_metadata(&self, path: &str) -> SourceFile {
        todo!()
    }
}

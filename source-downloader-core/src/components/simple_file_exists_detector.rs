use serde_json::{Map, Value};
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileExistsDetector, FileMover
    , SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct SimpleFileExistsDetectorSupplier {}
pub const SUPPLIER: SimpleFileExistsDetectorSupplier = SimpleFileExistsDetectorSupplier {};
const INSTANCE: SimpleFileExistsDetector = SimpleFileExistsDetector {};

impl ComponentSupplier for SimpleFileExistsDetectorSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_exists_detector("simple".to_string())]
    }

    fn apply(&self, _: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(INSTANCE))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(SdComponent, Debug)]
#[component(FileExistsDetector)]
#[allow(dead_code, unused)]
pub struct SimpleFileExistsDetector {}

impl Display for SimpleFileExistsDetector {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "simple")
    }
}

impl FileExistsDetector for SimpleFileExistsDetector {
    fn exists<'a>(
        &self,
        file_mover: &'a dyn FileMover,
        _: &'a SourceItem,
        file_contents: &'a [FileContent],
    ) -> HashMap<&'a PathBuf, Option<&'a PathBuf>> {
        let paths: Vec<&'a PathBuf> = file_contents.iter().map(|fc| fc.target_path()).collect();
        let exists = file_mover.exists(&paths);

        paths
            .into_iter()
            .zip(exists)
            .map(|(path, exist)| (path, if exist { Some(path) } else { None }))
            .collect()
    }
}

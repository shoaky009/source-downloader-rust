use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, SdComponent,
    SdComponentMetadata, SourceFile,
};
use sdk::{SdComponent, SourceItem};
use serde_json::{Map, Value};
use std::sync::Arc;

pub struct SystemFileResolverSupplier;
pub const SUPPLIER: SystemFileResolverSupplier = SystemFileResolverSupplier {};
const INSTANCE: SystemFileResolver = SystemFileResolver {};

impl ComponentSupplier for SystemFileResolverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_resolver("system-file".to_owned())]
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
#[component(ItemFileResolver)]
struct SystemFileResolver {}

impl ItemFileResolver for SystemFileResolver {
    fn resolve_files(&self, _: &SourceItem) -> Vec<SourceFile> {
        vec![]
    }
}

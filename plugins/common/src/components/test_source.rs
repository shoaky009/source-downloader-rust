use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemPointer, PointedItem, SdComponent,
    SdComponentMetadata, Source, empty_pointer,
};
use sdk::{Map, Value};
use std::sync::Arc;

#[derive(Debug)]
struct TestSource {}

impl TestSource {
    pub fn new() -> Self {
        TestSource {}
    }
}

impl SdComponent for TestSource {}

impl Source for TestSource {
    fn fetch(&self, _: &Map<String, Value>) -> Vec<PointedItem> {
        vec![]
    }

    fn default_pointer(&self) -> Box<dyn ItemPointer> {
        empty_pointer()
    }
}

pub struct TestSourceSupplier {}

pub const SUPPLIER: TestSourceSupplier = TestSourceSupplier {};

impl ComponentSupplier for TestSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("test".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let mode = props.get("mode").and_then(|v| v.as_i64()).unwrap_or(0) as i8;
        if mode == 1 {
            return Err(ComponentError::from("Mode 1 is not supported"));
        }

        Ok(Arc::new(TestSource::new()))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

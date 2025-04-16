use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PointedItem, SdComponent,
    SdComponentMetadata, Source,
};
use sdk::{Map, Value};

struct TestSource {}

impl TestSource {
    pub fn new() -> Self {
        TestSource {}
    }
}

impl SdComponent for TestSource {}

impl Source for TestSource {
    fn fetch(&self) -> Vec<PointedItem> {
        vec![]
    }
}

pub struct TestSourceSupplier {}

impl TestSourceSupplier {
    pub fn new() -> Self {
        TestSourceSupplier {}
    }
}

impl ComponentSupplier for TestSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("test".to_string())]
    }

    fn apply(&self, props: Map<String, Value>) -> Result<Box<dyn SdComponent>, ComponentError> {
        let mode = props.get("mode").and_then(|v| v.as_i64()).unwrap_or(0) as i8;
        if mode == 1 {
            return Err(ComponentError::from("Mode 1 is not supported"));
        }

        Ok(Box::new(TestSource::new()))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

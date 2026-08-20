use async_trait::async_trait;
use serde::Deserialize;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::SourceFile;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileTagger, SdComponent,
    SdComponentMetadata, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct MappedFileTaggerSupplier;
pub const SUPPLIER: MappedFileTaggerSupplier = MappedFileTaggerSupplier;

#[derive(Deserialize)]
struct MappedFileTaggerConfig {
    mapping: HashMap<String, String>,
}

impl ComponentSupplier for MappedFileTaggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_tagger("mapped".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: MappedFileTaggerConfig = deserialize_component_config(props)?;
        Ok(Arc::new(MappedFileTagger { mapping: config.mapping }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Tags files by mapping their file names to configured tags."
                .to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "mapping":{"type":"object"}
                },
                "required":["mapping"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, SdComponent)]
#[component(FileTagger)]
pub struct MappedFileTagger {
    mapping: HashMap<String, String>,
}

impl Display for MappedFileTagger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("mapped")
    }
}

#[async_trait]
impl FileTagger for MappedFileTagger {
    async fn tag(&self, source_file: &SourceFile) -> Option<String> {
        source_file
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| self.mapping.get(name))
            .cloned()
    }
}

use async_trait::async_trait;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, EmptyPointer, PointedItem,
    ProcessingError, SdComponent, SdComponentMetadata, Source, SourceItemStream,
    SourcePointer, deserialize_component_config, format_error_chain, source_item_stream,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct UriSourceSupplier;
pub const SUPPLIER: UriSourceSupplier = UriSourceSupplier;

#[derive(Deserialize)]
struct UriSourceConfig {
    uri: String,
}

impl ComponentSupplier for UriSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("uri".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: UriSourceConfig = deserialize_component_config(props)?;
        let uri = config.uri.parse().map_err(|error| {
            ComponentError::new(format!(
                "Invalid configuration at 'uri': Invalid URI '{}': {error}",
                config.uri
            ))
        })?;
        Ok(Arc::new(UriSource { uri }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Loads source items from a URI.".to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "uri":{"type":"string"}
                },
                "required":["uri"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Source)]
pub struct UriSource {
    uri: source_downloader_sdk::http::Uri,
}

impl Display for UriSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("uri")
    }
}

#[async_trait]
impl Source for UriSource {
    async fn fetch(
        &self,
        _: &dyn SourcePointer,
        limit: u32,
    ) -> Result<SourceItemStream, ProcessingError> {
        let bytes = if self.uri.scheme_str() == Some("file") {
            let url = url::Url::parse(&self.uri.to_string())
                .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
            let path = url
                .to_file_path()
                .map_err(|_| ProcessingError::non_retryable("Invalid file URI"))?;
            tokio::fs::read(path).await?
        } else {
            reqwest::get(self.uri.to_string())
                .await
                .map_err(|error| {
                    ProcessingError::non_retryable(format_error_chain(&error))
                })?
                .error_for_status()
                .map_err(|error| {
                    ProcessingError::non_retryable(format_error_chain(&error))
                })?
                .bytes()
                .await
                .map_err(|error| {
                    ProcessingError::non_retryable(format_error_chain(&error))
                })?
                .to_vec()
        };
        let parsed_items: Vec<SourceItem> = serde_json::from_slice(&bytes)
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
        Ok(source_item_stream(
            parsed_items
                .into_iter()
                .take(limit as usize)
                .map(|source_item| PointedItem {
                    source_item,
                    item_pointer: Arc::new(EmptyPointer),
                })
                .collect(),
        ))
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(EmptyPointer)
    }

    fn parse_raw_pointer(&self, _: Value) -> Box<dyn SourcePointer> {
        Box::new(EmptyPointer)
    }
}

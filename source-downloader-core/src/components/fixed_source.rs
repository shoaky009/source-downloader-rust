use async_trait::async_trait;
use serde::{Deserialize, Deserializer, de::Error as _};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, EmptyPointer, ItemFileResolver,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source, SourceFile,
    SourceItemStream, SourcePointer, deserialize_component_config, source_item_stream,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct FixedSourceSupplier;
pub const SUPPLIER: FixedSourceSupplier = FixedSourceSupplier;

fn deserialize_optional_uri<'de, D>(
    deserializer: D,
) -> Result<Option<source_downloader_sdk::http::Uri>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|uri| {
            uri.parse::<source_downloader_sdk::http::Uri>().map_err(|error| {
                D::Error::custom(format!(
                    "Invalid fixed source file URI '{uri}': {error}"
                ))
            })
        })
        .transpose()
}

#[derive(Deserialize)]
struct RawSourceItemContent {
    item: SourceItem,
    files: Vec<RawSourceFile>,
}

#[derive(Deserialize)]
struct RawSourceFile {
    path: PathBuf,
    #[serde(default)]
    attrs: Map<String, Value>,
    #[serde(
        rename = "downloadUri",
        alias = "fileUri",
        default,
        deserialize_with = "deserialize_optional_uri"
    )]
    download_uri: Option<source_downloader_sdk::http::Uri>,
    #[serde(default)]
    tags: Vec<String>,
}

impl RawSourceFile {
    fn into_source_file(self) -> SourceFile {
        SourceFile {
            path: self.path,
            attrs: self.attrs,
            download_uri: self.download_uri,
            tags: self.tags,
            data: None,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FixedSourceConfig {
    content: Vec<RawSourceItemContent>,
    #[serde(default)]
    offset_mode: bool,
}

impl ComponentSupplier for FixedSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::source("fixed".to_owned()),
            ComponentType::file_resolver("fixed".to_owned()),
        ]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<FixedSourceConfig>(props)?;
        let mut converted = Vec::with_capacity(config.content.len());
        for item in config.content {
            converted.push(SourceItemContent {
                item: item.item,
                files: item
                    .files
                    .into_iter()
                    .map(RawSourceFile::into_source_file)
                    .collect(),
            });
        }
        Ok(Arc::new(FixedSource { content: converted, offset_mode: config.offset_mode }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Provides a fixed set of source items and files.".to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "content":{
                        "type":"array",
                        "items":{
                            "type":"object",
                            "properties":{
                                "item":{"type":"object"},
                                "files":{
                                    "type":"array",
                                    "items":{
                                        "type":"object",
                                        "properties":{
                                            "path":{"type":"string"},
                                            "attrs":{"type":"object"},
                                            "downloadUri":{"type":"string"},
                                            "tags":{
                                                "type":"array",
                                                "items":{"type":"string"}
                                            }
                                        },
                                        "required":["path"]
                                    }
                                }
                            },
                            "required":["item","files"]
                        }
                    },
                    "offset-mode":{"type":"boolean","default":false}
                },
                "required":["content"]
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(source_downloader_sdk::SdComponent)]
#[component(Source, ItemFileResolver)]
pub struct FixedSource {
    content: Vec<SourceItemContent>,
    offset_mode: bool,
}

impl Debug for FixedSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedSource")
            .field("content_count", &self.content.len())
            .field("offset_mode", &self.offset_mode)
            .finish()
    }
}
impl Display for FixedSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("fixed")
    }
}

#[async_trait]
impl Source for FixedSource {
    async fn fetch(
        &self,
        source_pointer: &dyn SourcePointer,
        limit: u32,
    ) -> Result<SourceItemStream, ProcessingError> {
        let offset = source_pointer
            .as_any()
            .downcast_ref::<OffsetPointer>()
            .map(|pointer| pointer.offset)
            .unwrap_or(0);
        let items = self.content.iter().map(|content| PointedItem {
            source_item: content.item.clone(),
            item_pointer: Arc::new(EmptyPointer),
        });
        let items = if self.offset_mode {
            items.skip(offset).take(limit as usize).collect()
        } else {
            items.take(limit as usize).collect()
        };
        Ok(source_item_stream(items))
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(OffsetPointer { offset: 0 })
    }

    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
        serde_json::from_value::<OffsetPointer>(value)
            .map(|pointer| Box::new(pointer) as Box<dyn SourcePointer>)
            .unwrap_or_else(|_| Box::new(OffsetPointer { offset: 0 }))
    }
}

#[async_trait]
impl ItemFileResolver for FixedSource {
    async fn resolve_files(
        &self,
        source_item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        Ok(self
            .content
            .iter()
            .find(|content| content.item == *source_item)
            .map(|content| content.files.clone())
            .unwrap_or_default())
    }
}

#[derive(Clone)]
pub struct SourceItemContent {
    pub item: SourceItem,
    pub files: Vec<SourceFile>,
}

impl Debug for SourceItemContent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceItemContent")
            .field("item", &self.item)
            .field("file_count", &self.files.len())
            .finish()
    }
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct OffsetPointer {
    pub offset: usize,
}

impl SourcePointer for OffsetPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| Value::Object(Map::new()))
    }

    fn update(
        &mut self,
        _: &SourceItem,
        _: &dyn source_downloader_sdk::component::ItemPointer,
    ) {
        self.offset += 1;
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

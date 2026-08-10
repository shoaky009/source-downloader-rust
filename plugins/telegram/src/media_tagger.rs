use crate::source::MEDIA_TYPE_ATTR;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentCreateContext, ComponentError, ComponentSupplier, ComponentType, FileTagger,
    SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct TelegramMediaTaggerSupplier;
pub const MEDIA_TAGGER_SUPPLIER: TelegramMediaTaggerSupplier =
    TelegramMediaTaggerSupplier;

impl ComponentSupplier for TelegramMediaTaggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_tagger("telegram".into())]
    }

    fn apply(
        &self,
        _: &dyn ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(TelegramMediaTagger))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Tags Telegram media files with their media type.".into(),
            props_json_schema: Some(
                source_downloader_sdk::serde_json::json!({"type": "object", "properties": {}}),
            ),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(FileTagger)]
struct TelegramMediaTagger;

impl Display for TelegramMediaTagger {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("telegram")
    }
}

#[async_trait]
impl FileTagger for TelegramMediaTagger {
    async fn tag(&self, source_file: &SourceFile) -> Option<String> {
        source_file.attrs.get(MEDIA_TYPE_ATTR).and_then(Value::as_str).map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn tags_from_telegram_media_type_attribute() {
        let mut file = SourceFile::new(PathBuf::from("photo.jpg"));
        file.attrs.insert(MEDIA_TYPE_ATTR.into(), Value::String("photo".into()));
        assert_eq!(TelegramMediaTagger.tag(&file).await.as_deref(), Some("photo"));
    }
}

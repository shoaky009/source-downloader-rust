use crate::client::TelegramClientInstance;
use grammers_client::media::Media;
use serde::de::{self, Unexpected, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentCreateContext, ComponentError, ComponentSupplier, ComponentType,
    ItemPointer, PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source,
    SourcePointer, deserialize_component_config,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value};
use source_downloader_sdk::time::{Date, OffsetDateTime, Time};
use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;
use time::UtcOffset;

pub const MEDIA_TYPE_ATTR: &str = "mediaType";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ChatConfig {
    #[serde(deserialize_with = "deserialize_i64_from_number_or_string")]
    chat_id: i64,
    #[serde(default)]
    begin_date: Option<Date>,
}

fn deserialize_i64_from_number_or_string<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: Deserializer<'de>,
{
    struct I64Visitor;

    impl Visitor<'_> for I64Visitor {
        type Value = i64;

        fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an i64 or a string containing an i64")
        }

        fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            i64::try_from(value)
                .map_err(|_| E::invalid_value(Unexpected::Unsigned(value), &self))
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            value.parse().map_err(|_| E::invalid_value(Unexpected::Str(value), &self))
        }
    }

    deserializer.deserialize_any(I64Visitor)
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct TelegramSourceConfig {
    client: String,
    chats: Vec<ChatConfig>,
    #[serde(default = "default_sites")]
    sites: HashSet<String>,
    #[serde(default)]
    include_non_media: bool,
}

fn default_sites() -> HashSet<String> {
    HashSet::from(["Telegraph".to_string()])
}

pub struct TelegramSourceSupplier;
pub const SOURCE_SUPPLIER: TelegramSourceSupplier = TelegramSourceSupplier;

impl ComponentSupplier for TelegramSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("telegram".into())]
    }

    fn apply(
        &self,
        context: &dyn ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: TelegramSourceConfig = deserialize_component_config(props)?;
        if config.chats.is_empty() {
            return Err(ComponentError::new(
                "Invalid configuration at 'chats': must not be empty",
            ));
        }
        let instance = context
            .get_instance(&config.client, TypeId::of::<TelegramClientInstance>())?;
        let client = instance.downcast::<TelegramClientInstance>().map_err(|_| {
            ComponentError::new(format!(
                "Telegram instance '{}' has an incompatible type",
                config.client
            ))
        })?;
        Ok(Arc::new(TelegramSource {
            client,
            chats: config.chats,
            sites: config.sites,
            include_non_media: config.include_non_media,
        }))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Provides Telegram chats as a source.".into(),
            props_json_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "client": {"type": "string"},
                    "chats": {"type": "array", "minItems": 1, "items": {"type": "object", "properties": {"chat-id": {"type": ["integer", "string"]}, "begin-date": {"type": ["string", "null"], "format": "date"}}, "required": ["chat-id"]}},
                    "sites": {"type": "array", "items": {"type": "string"}, "default": ["Telegraph"]},
                    "include-non-media": {"type": "boolean", "default": false}
                },
                "required": ["client", "chats"]
            })),
            props_ui_schema: Some(serde_json::json!({
                "client": {
                    "ui:field": "instanceField",
                    "ui:options": {"factoryType": std::any::type_name::<crate::client::TelegramClientInstanceFactory>()}
                }
            })),
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {
                    "chatLastMessageIds": {"type": "object", "additionalProperties": {"type": "integer"}}
                },
                "required": ["chatLastMessageIds"]
            })),
        }))
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TelegramPointer {
    #[serde(default)]
    chat_last_message_ids: HashMap<i64, i32>,
}

#[derive(Debug)]
struct ChatPointer {
    chat_id: i64,
    message_id: i32,
}

impl ItemPointer for ChatPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl SourcePointer for TelegramPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or_default()
    }

    fn update(&mut self, _: &SourceItem, pointer: &dyn ItemPointer) {
        if let Some(pointer) = pointer.as_any().downcast_ref::<ChatPointer>() {
            self.chat_last_message_ids.insert(pointer.chat_id, pointer.message_id);
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(source_downloader_sdk::SdComponent)]
#[component(Source)]
struct TelegramSource {
    client: Arc<TelegramClientInstance>,
    chats: Vec<ChatConfig>,
    sites: HashSet<String>,
    include_non_media: bool,
}

impl std::fmt::Debug for TelegramSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramSource")
            .field("chats", &self.chats)
            .field("sites", &self.sites)
            .field("include_non_media", &self.include_non_media)
            .finish_non_exhaustive()
    }
}

impl Display for TelegramSource {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("telegram")
    }
}

#[async_trait]
impl Source for TelegramSource {
    async fn fetch<'pointer>(
        &self,
        pointer: &'pointer dyn SourcePointer,
        limit: u32,
    ) -> Result<source_downloader_sdk::component::SourceItems<'pointer>, ProcessingError>
    {
        let pointer =
            pointer.as_any().downcast_ref::<TelegramPointer>().ok_or_else(|| {
                ProcessingError::non_retryable("Invalid Telegram source pointer")
            })?;
        let client = self.client.client().await?;
        let mut items = Vec::new();
        for chat in &self.chats {
            if items.len() >= limit as usize {
                break;
            }
            let (peer, chat_name) = self.client.chat(chat.chat_id).await?;
            let offset = pointer
                .chat_last_message_ids
                .get(&chat.chat_id)
                .copied()
                .unwrap_or_default();
            let mut messages = client.iter_messages(peer).offset_id(offset).reverse(true);
            if offset == 0
                && let Some(begin_date) = chat.begin_date
            {
                let timestamp = i32::try_from(
                    begin_date.with_time(Time::MIDNIGHT).assume_utc().unix_timestamp(),
                )
                .map_err(|_| {
                    ProcessingError::non_retryable(
                        "Telegram begin-date is outside the supported range",
                    )
                })?;
                messages = messages.offset_date(timestamp);
            }
            while let Some(message) =
                messages.next().await.map_err(crate::client::telegram_error)?
            {
                if let Some(begin_date) = chat.begin_date
                    && message.date().timestamp()
                        < begin_date
                            .with_time(Time::MIDNIGHT)
                            .assume_utc()
                            .unix_timestamp()
                {
                    continue;
                }
                let message_id = message.id();
                if let Some(source_item) =
                    self.convert_message(chat.chat_id, &chat_name, &message)?
                {
                    items.push(PointedItem {
                        source_item,
                        item_pointer: Arc::new(ChatPointer {
                            chat_id: chat.chat_id,
                            message_id,
                        }),
                    });
                }
                if items.len() >= limit as usize {
                    break;
                }
            }
        }
        Ok(source_downloader_sdk::component::source_items(items))
    }

    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        let chat_last_message_ids =
            self.chats.iter().map(|chat| (chat.chat_id, 0)).collect();
        Box::new(TelegramPointer { chat_last_message_ids })
    }

    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
        let mut pointer =
            serde_json::from_value::<TelegramPointer>(value).unwrap_or_default();
        pointer
            .chat_last_message_ids
            .retain(|id, _| self.chats.iter().any(|chat| chat.chat_id == *id));
        for chat in &self.chats {
            pointer.chat_last_message_ids.entry(chat.chat_id).or_default();
        }
        Box::new(pointer)
    }
}

impl TelegramSource {
    fn convert_message(
        &self,
        configured_chat_id: i64,
        chat_name: &str,
        message: &grammers_client::message::Message,
    ) -> Result<Option<SourceItem>, ProcessingError> {
        let message_id = message.id();
        let chat_id = configured_chat_id.unsigned_abs();
        let link = uri(format!("tg://privatepost?channel={chat_id}&post={message_id}"))?;
        let download_uri = uri(format!(
            "tg://privatepost?channel={configured_chat_id}&post={message_id}"
        ))?;
        let datetime = OffsetDateTime::from_unix_timestamp(message.date().timestamp())
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?
            .to_offset(
                UtcOffset::current_local_offset()
                    .map_err(|error| ProcessingError::non_retryable(error.to_string()))?,
            );
        let mut attrs = Map::from_iter([
            ("messageId".into(), Value::from(message_id)),
            ("chatId".into(), Value::from(chat_id)),
            ("chatName".into(), Value::String(chat_name.to_string())),
        ]);
        if let Some(sender_id) = message.sender_id().and_then(|id| id.bare_id()) {
            attrs.insert("fromId".into(), Value::from(sender_id));
        }

        let Some(media) = message.media() else {
            return Ok(self.include_non_media.then(|| SourceItem {
                title: format!("message-{message_id}"),
                link,
                datetime,
                content_type: "message".into(),
                download_uri,
                attrs,
                tags: vec![],
                identity: None,
            }));
        };
        match media {
            Media::Photo(photo) => {
                attrs.insert(MEDIA_TYPE_ATTR.into(), Value::String("photo".into()));
                if let Some(size) = Media::Photo(photo).size() {
                    attrs.insert("size".into(), Value::from(size));
                }
                Ok(Some(SourceItem {
                    title: format!("{chat_id}-{message_id}.jpg"),
                    link,
                    datetime,
                    content_type: "image/jpeg".into(),
                    download_uri,
                    attrs,
                    tags: vec![],
                    identity: None,
                }))
            }
            Media::Document(document) => {
                let title = document
                    .name()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{chat_id}-{message_id}"));
                let content_type = document
                    .mime_type()
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let identity = Some(document.id().to_string());
                attrs.insert(MEDIA_TYPE_ATTR.into(), Value::String("document".into()));
                if let Some(size) = document.size() {
                    attrs.insert("size".into(), Value::from(size));
                }
                Ok(Some(SourceItem {
                    title,
                    link,
                    datetime,
                    content_type,
                    download_uri,
                    attrs,
                    tags: vec![],
                    identity,
                }))
            }
            Media::Sticker(sticker) => {
                let document = sticker.document;
                attrs.insert(MEDIA_TYPE_ATTR.into(), Value::String("document".into()));
                Ok(Some(SourceItem {
                    title: document
                        .name()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{chat_id}-{message_id}.webp")),
                    link,
                    datetime,
                    content_type: document
                        .mime_type()
                        .unwrap_or("image/webp")
                        .to_string(),
                    download_uri,
                    attrs,
                    tags: vec![],
                    identity: Some(document.id().to_string()),
                }))
            }
            Media::WebPage(webpage) => self.convert_webpage(
                webpage,
                message.text(),
                link,
                datetime,
                download_uri,
                attrs,
            ),
            _ => Ok(None),
        }
    }

    fn convert_webpage(
        &self,
        webpage: grammers_client::media::WebPage,
        message_text: &str,
        link: Uri,
        datetime: OffsetDateTime,
        fallback_download_uri: Uri,
        mut attrs: Map<String, Value>,
    ) -> Result<Option<SourceItem>, ProcessingError> {
        let grammers_client::tl::enums::WebPage::Page(page) = webpage.raw.webpage else {
            return Ok(None);
        };
        let Some(site_name) = page.site_name else {
            return Ok(None);
        };
        if !self.sites.contains(&site_name) {
            tracing::debug!(site = site_name, "Ignoring Telegram web page site");
            return Ok(None);
        }
        attrs.insert(MEDIA_TYPE_ATTR.into(), Value::String("webpage".into()));
        attrs.insert("site".into(), Value::String(site_name));
        Ok(Some(SourceItem {
            title: page.title.unwrap_or_else(|| message_text.to_string()),
            link,
            datetime,
            content_type: page.r#type.unwrap_or_else(|| "webpage".into()),
            download_uri: Uri::from_str(&page.url).unwrap_or(fallback_download_uri),
            attrs,
            tags: vec![],
            identity: None,
        }))
    }
}

fn uri(value: String) -> Result<Uri, ProcessingError> {
    Uri::from_str(&value).map_err(|error| {
        ProcessingError::non_retryable(format!("Invalid Telegram URI: {error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_round_trips_and_refreshes_configured_chats() {
        let source = TelegramSource {
            client: Arc::new(TelegramClientInstance::disconnected(
                crate::client::TelegramClientConfig {
                    api_id: 1,
                    api_hash: "hash".into(),
                    metadata_path: "session".into(),
                    proxy: None,
                    timeout: 5,
                },
            )),
            chats: vec![ChatConfig { chat_id: -7, begin_date: None }],
            sites: default_sites(),
            include_non_media: false,
        };
        let pointer =
            source.parse_raw_pointer(source_downloader_sdk::serde_json::json!({
                "chatLastMessageIds": {"-7": 12, "9": 2}
            }));
        assert_eq!(pointer.dump()["chatLastMessageIds"]["-7"], 12);
        assert!(pointer.dump()["chatLastMessageIds"].get("9").is_none());
    }

    #[test]
    fn source_config_accepts_numeric_and_string_chat_ids() {
        let props = source_downloader_sdk::serde_json::json!({
            "client": "telegram",
            "chats": [
                {"chat-id": -2637843147_i64},
                {"chat-id": "-2567094752"},
            ],
        })
        .as_object()
        .unwrap()
        .clone();

        let config: TelegramSourceConfig = deserialize_component_config(&props).unwrap();

        assert_eq!(config.chats[0].chat_id, -2_637_843_147);
        assert_eq!(config.chats[1].chat_id, -2_567_094_752);
    }

    #[test]
    fn supplier_reports_source_path_for_empty_chats() {
        let props = source_downloader_sdk::serde_json::json!({
            "client": "telegram",
            "chats": [],
        })
        .as_object()
        .unwrap()
        .clone();

        let error = match SOURCE_SUPPLIER.apply(
            &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
            &props,
        ) {
            Err(error) => error,
            Ok(_) => panic!("invalid source configuration was accepted"),
        };

        assert_eq!(error.message, "Invalid configuration at 'chats': must not be empty");
    }

    #[test]
    fn supplier_reports_source_field_path_for_invalid_top_level_value() {
        let props = source_downloader_sdk::serde_json::json!({
            "client": 1,
            "chats": [],
        })
        .as_object()
        .unwrap()
        .clone();

        let error = match SOURCE_SUPPLIER.apply(
            &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
            &props,
        ) {
            Err(error) => error,
            Ok(_) => panic!("invalid source configuration was accepted"),
        };

        assert_eq!(
            error.message,
            "Invalid configuration at 'client': invalid type: integer `1`, expected a string"
        );
    }

    #[test]
    fn supplier_reports_full_path_for_invalid_chat_element() {
        let props = source_downloader_sdk::serde_json::json!({
            "client": "telegram",
            "chats": [{"chat-id": "invalid"}],
        })
        .as_object()
        .unwrap()
        .clone();

        let error = match SOURCE_SUPPLIER.apply(
            &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
            &props,
        ) {
            Err(error) => error,
            Ok(_) => panic!("invalid chat configuration was accepted"),
        };

        assert_eq!(
            error.message,
            "Invalid configuration at 'chats[0].chat-id': invalid value: string \"invalid\", expected an i64 or a string containing an i64"
        );
    }
}

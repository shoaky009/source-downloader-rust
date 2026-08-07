use crate::http;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, EmptyPointer, ItemPointer,
    PointedItem, ProcessingError, SdComponent, SdComponentMetadata, Source,
    SourcePointer,
};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::{self, Map, Value};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::Any;
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;
use time::format_description::well_known::Rfc2822;
use time::{PrimitiveDateTime, UtcOffset};
#[derive(Debug, Clone, PartialEq, Eq)]
struct Extension {
    namespace: String,
    name: String,
    attributes: HashMap<String, String>,
    value: String,
}
pub struct RssSourceSupplier;
pub const SUPPLIER: RssSourceSupplier = RssSourceSupplier;
impl ComponentSupplier for RssSourceSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source("rss".into())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let url = p
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::new("Missing or invalid 'url' property"))?
            .to_string();
        reqwest::Url::parse(&url)
            .map_err(|e| ComponentError::new(format!("Invalid RSS URL: {e}")))?;
        let tags = strings(p.get("tags"), "tags")?;
        let attributes = p
            .get("attributes")
            .map(|v| {
                serde_json::from_value::<HashMap<String, String>>(v.clone()).map_err(
                    |e| {
                        ComponentError::new(format!("Invalid 'attributes' property: {e}"))
                    },
                )
            })
            .transpose()?
            .unwrap_or_default();
        let date_format =
            p.get("date-format").and_then(Value::as_str).map(str::to_string);
        let client = if url.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|e| ComponentError::new(e.to_string()))?
        } else {
            http::build_client()?
        };
        let group =
            reqwest::Url::parse(&url).ok().and_then(|u| u.host_str().map(str::to_string));
        Ok(Arc::new(RssSource { url, tags, attributes, date_format, client, group }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
fn strings(v: Option<&Value>, name: &str) -> Result<Vec<String>, ComponentError> {
    v.map(|v| {
        serde_json::from_value(v.clone())
            .map_err(|e| ComponentError::new(format!("Invalid '{name}' property: {e}")))
    })
    .transpose()
    .map(|v| v.unwrap_or_default())
}
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Source)]
struct RssSource {
    url: String,
    tags: Vec<String>,
    attributes: HashMap<String, String>,
    date_format: Option<String>,
    client: reqwest::Client,
    group: Option<String>,
}

impl Display for RssSource {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "rss")
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct LatestPointer {
    #[serde(default)]
    latest: Option<OffsetDateTime>,
}
impl SourcePointer for LatestPointer {
    fn dump(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
    fn update(&mut self, item: &SourceItem, _: &dyn ItemPointer) {
        if self.latest.is_none_or(|v| item.datetime > v) {
            self.latest = Some(item.datetime)
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
#[async_trait]
impl Source for RssSource {
    async fn fetch<'p>(
        &self,
        p: &'p dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        let latest = p
            .as_any()
            .downcast_ref::<LatestPointer>()
            .ok_or_else(|| ProcessingError::non_retryable("Invalid RSS source pointer"))?
            .latest;
        let r =
            http::execute(&self.client, self.client.get(&self.url), "Fetch RSS").await?;
        let body =
            r.bytes().await.map_err(|e| http::map_error(e, "Read RSS response"))?;
        let items =
            parse_feed(&body, &self.tags, &self.attributes, self.date_format.as_deref())?;
        Ok(items
            .into_iter()
            .filter(|i| latest.is_none_or(|v| i.datetime > v))
            .take(limit as usize)
            .map(|source_item| PointedItem {
                source_item,
                item_pointer: Arc::new(EmptyPointer),
            })
            .collect())
    }
    fn default_pointer(&self) -> Box<dyn SourcePointer> {
        Box::new(LatestPointer::default())
    }
    fn parse_raw_pointer(&self, v: Value) -> Box<dyn SourcePointer> {
        Box::new(serde_json::from_value::<LatestPointer>(v).unwrap_or_default())
    }
    fn group(&self) -> Option<String> {
        self.group.clone()
    }
}
#[derive(Default)]
struct RawItem {
    title: Option<String>,
    link: Option<String>,
    pub_date: Option<String>,
    enclosure_url: Option<String>,
    enclosure_type: String,
    extensions: Vec<Extension>,
}
fn parse_feed(
    xml: &[u8],
    tags: &[String],
    attrs: &HashMap<String, String>,
    date_format: Option<&str>,
) -> Result<Vec<SourceItem>, ProcessingError> {
    let mut r = Reader::from_reader(xml);
    r.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut item: Option<RawItem> = None;
    let mut field: Option<(String, String, HashMap<String, String>)> = None;
    let mut out = Vec::new();
    let mut namespaces = HashMap::new();
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let q = String::from_utf8_lossy(e.name().as_ref()).to_string();
                for a in e.attributes().flatten() {
                    let k = String::from_utf8_lossy(a.key.as_ref()).to_string();
                    if (k == "xmlns" || k.starts_with("xmlns:"))
                        && let Ok(v) = a
                            .decoded_and_normalized_value(Default::default(), r.decoder())
                    {
                        namespaces.insert(
                            k.strip_prefix("xmlns:").unwrap_or("").to_string(),
                            v.into_owned(),
                        );
                    }
                }
                let name = q.rsplit(':').next().unwrap_or(&q).to_string();
                if name == "item" {
                    item = Some(RawItem::default())
                } else if item.is_some() {
                    let mut aa = HashMap::new();
                    for a in e.attributes().flatten() {
                        if let Ok(v) = a
                            .decoded_and_normalized_value(Default::default(), r.decoder())
                        {
                            aa.insert(
                                String::from_utf8_lossy(a.key.as_ref()).to_string(),
                                v.into_owned(),
                            );
                        }
                    }
                    if name == "enclosure"
                        && let Some(i) = item.as_mut()
                    {
                        i.enclosure_url = aa.get("url").cloned();
                        i.enclosure_type = aa.get("type").cloned().unwrap_or_default();
                    }
                    field = Some((q, name, aa));
                }
            }
            Ok(Event::Empty(e)) => {
                if let Some(item) = item.as_mut() {
                    let name = String::from_utf8_lossy(e.name().as_ref())
                        .rsplit(':')
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    if name == "enclosure" {
                        for attribute in e.attributes().flatten() {
                            let key = String::from_utf8_lossy(attribute.key.as_ref());
                            if let Ok(value) = attribute.decoded_and_normalized_value(
                                Default::default(),
                                r.decoder(),
                            ) {
                                match key.as_ref() {
                                    "url" => {
                                        item.enclosure_url = Some(value.into_owned())
                                    }
                                    "type" => item.enclosure_type = value.into_owned(),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if let (Some(i), Some((q, name, aa))) = (item.as_mut(), field.as_ref()) {
                    let text = t
                        .xml_content(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|e| {
                            ProcessingError::non_retryable(format!(
                                "Invalid RSS text: {e}"
                            ))
                        })?
                        .into_owned();
                    match name.as_str() {
                        "title" => i.title = Some(text),
                        "link" => i.link = Some(text),
                        "pubDate" => i.pub_date = Some(text),
                        _ => {
                            let prefix = q.split_once(':').map(|x| x.0).unwrap_or("");
                            i.extensions.push(Extension {
                                namespace: namespaces
                                    .get(prefix)
                                    .cloned()
                                    .unwrap_or_default(),
                                name: name.clone(),
                                attributes: aa.clone(),
                                value: text,
                            })
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref())
                    .rsplit(':')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                if name == "item"
                    && let Some(i) = item.take()
                {
                    out.push(convert(i, tags, attrs, date_format)?);
                }
                field = None
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(ProcessingError::non_retryable(format!(
                    "Failed to parse RSS XML: {e}"
                )));
            }
            _ => {}
        }
        buf.clear()
    }
    Ok(out)
}
fn convert(
    i: RawItem,
    tags: &[String],
    attrs: &HashMap<String, String>,
    format: Option<&str>,
) -> Result<SourceItem, ProcessingError> {
    let title = i
        .title
        .ok_or_else(|| ProcessingError::non_retryable("RSS item title is missing"))?;
    let link = i
        .link
        .ok_or_else(|| ProcessingError::non_retryable("RSS item link is missing"))?;
    let link_uri = Uri::from_str(&link).map_err(|e| {
        ProcessingError::non_retryable(format!("Invalid RSS item link: {e}"))
    })?;
    let download =
        i.enclosure_url.filter(|s| !s.is_empty()).unwrap_or_else(|| link.clone());
    let download_uri = Uri::from_str(&download).map_err(|e| {
        ProcessingError::non_retryable(format!("Invalid RSS enclosure URL: {e}"))
    })?;
    let datetime = i
        .pub_date
        .as_deref()
        .map(|v| parse_date(v, format))
        .transpose()?
        .unwrap_or_else(OffsetDateTime::now_utc);
    let mut item_tags = Vec::new();
    let mut item_attrs = Map::new();
    for e in i.extensions {
        if tags.iter().any(|n| n == &e.name) {
            item_tags.push(e.value.clone())
        }
        for (k, n) in attrs {
            if n == &e.name {
                item_attrs.insert(k.clone(), Value::String(e.value.clone()));
            }
        }
    }
    Ok(SourceItem {
        title,
        link: link_uri,
        datetime,
        content_type: i.enclosure_type,
        download_uri,
        attrs: item_attrs,
        tags: item_tags,
        identity: None,
    })
}
fn parse_date(v: &str, format: Option<&str>) -> Result<OffsetDateTime, ProcessingError> {
    if let Ok(d) = OffsetDateTime::parse(v, &Rfc2822) {
        return Ok(d);
    }
    if let Ok(d) =
        OffsetDateTime::parse(v, &time::format_description::well_known::Rfc3339)
    {
        return Ok(d);
    }
    if let Some(f) = format {
        let desc = time::format_description::parse(f).map_err(|e| {
            ProcessingError::non_retryable(format!("Invalid RSS date format: {e}"))
        })?;
        return PrimitiveDateTime::parse(v, &desc)
            .map(|d| d.assume_offset(UtcOffset::UTC))
            .map_err(|e| {
                ProcessingError::non_retryable(format!("Invalid RSS date '{v}': {e}"))
            });
    }
    Err(ProcessingError::non_retryable(format!("Invalid RSS date '{v}'")))
}
#[cfg(test)]
mod tests {
    use super::*;
    const XML: &str = r#"<rss xmlns:x="urn:test"><channel><item><title>A</title><link>https://e/a</link><pubDate>Thu, 07 Aug 2025 01:02:03 +0000</pubDate><enclosure url="https://e/a.jpg" type="image/jpeg"/><x:tag role="main">anime</x:tag><x:code>42</x:code></item></channel></rss>"#;
    #[test]
    fn parses_standard_and_extensions() {
        let v = parse_feed(
            XML.as_bytes(),
            &["tag".into()],
            &HashMap::from([("id".into(), "code".into())]),
            None,
        )
        .unwrap();
        assert_eq!("anime", v[0].tags[0]);
        assert_eq!(Some("42"), v[0].attrs.get("id").and_then(Value::as_str));
        assert_eq!("image/jpeg", v[0].content_type);
    }
    #[test]
    fn pointer_round_trip_filters_latest() {
        let mut p = LatestPointer::default();
        let i = parse_feed(XML.as_bytes(), &[], &HashMap::new(), None).unwrap().remove(0);
        p.update(&i, &EmptyPointer);
        let raw = p.dump();
        let parsed: LatestPointer = serde_json::from_value(raw).unwrap();
        assert_eq!(Some(i.datetime), parsed.latest);
    }
    #[test]
    fn validates_url() {
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_err()
        );
    }
}

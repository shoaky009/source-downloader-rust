use crate::http;
use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;
use parking_lot::Mutex;
use regex::Regex;
use scraper::{Html, Selector};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct GetchuVariableProviderSupplier;
pub const SUPPLIER: GetchuVariableProviderSupplier = GetchuVariableProviderSupplier;
impl ComponentSupplier for GetchuVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("getchu".into())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let base = p
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://www.getchu.com")
            .trim_end_matches('/')
            .to_string();
        let client = http::build_client()?;
        Ok(Arc::new(GetchuVariableProvider {
            client,
            base,
            cache: Mutex::new(HashMap::new()),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Resolves Getchu work variables from its website.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "base-url":{"type":"string","default":"https://www.getchu.com"}
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
struct GetchuVariableProvider {
    client: reqwest::Client,
    base: String,
    cache: Mutex<HashMap<String, PatternVariables>>,
}

impl Display for GetchuVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "getchu")
    }
}

static ISBN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-zA-Z]+-[a-zA-Z0-9]+").unwrap());
impl GetchuVariableProvider {
    async fn resolve(&self, text: &str) -> Result<PatternVariables, ProcessingError> {
        if let Some(v) = self.cache.lock().get(text).cloned() {
            return Ok(v);
        }
        let query = ISBN.find(text).map(|m| m.as_str()).unwrap_or(text);
        let vars = self.search(query).await?.unwrap_or_default();
        let mut c = self.cache.lock();
        if c.len() >= 500
            && let Some(k) = c.keys().next().cloned()
        {
            c.remove(&k);
        }
        c.insert(text.into(), vars.clone());
        Ok(vars)
    }
    async fn search(&self, q: &str) -> Result<Option<PatternVariables>, ProcessingError> {
        let r = http::execute(
            &self.client,
            self.client
                .get(format!("{}/php/search.phtml", self.base))
                .query(&[("search_keyword", q)])
                .header("Cookie", "getchu_adalt_flag=getchu.com"),
            "Search Getchu",
        )
        .await?;
        let html = decode_response(r).await?;
        let url = {
            let document = Html::parse_document(&html);
            let selector =
                Selector::parse(".search_container .display li #detail_block .blueb")
                    .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
            document
                .select(&selector)
                .filter_map(|element| {
                    Some((
                        element.text().collect::<String>(),
                        element.value().attr("href")?.to_string(),
                    ))
                })
                .min_by_key(|(title, _)| title.chars().count())
                .map(|(_, url)| url)
        };
        match url {
            Some(url) => self.detail(&url).await,
            None => Ok(None),
        }
    }
    async fn detail(
        &self,
        url: &str,
    ) -> Result<Option<PatternVariables>, ProcessingError> {
        let url = reqwest::Url::parse(&self.base)
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?
            .join(url)
            .map_err(|e| ProcessingError::non_retryable(e.to_string()))?;
        let id = url.query_pairs().find(|(k, _)| k == "id").map(|(_, v)| v.to_string());
        let Some(id) = id else {
            return Ok(None);
        };
        let r = http::execute(
            &self.client,
            self.client.get(url).header("Cookie", "getchu_adalt_flag=getchu.com"),
            "Fetch Getchu item",
        )
        .await?;
        Ok(Some(parse_detail(&decode_response(r).await?, &id)))
    }
}
async fn decode_response(r: reqwest::Response) -> Result<String, ProcessingError> {
    let charset = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("charset=").nth(1))
        .and_then(|v| Encoding::for_label(v.trim().as_bytes()));
    let b = r
        .bytes()
        .await
        .map_err(|error| http::map_error(error, "Read Getchu response"))?;
    let enc = charset.unwrap_or_else(|| {
        let mut d = EncodingDetector::new(Iso2022JpDetection::Allow);
        d.feed(&b, true);
        d.guess(None, Utf8Detection::Allow)
    });
    let (text, _, errors) = enc.decode(&b);
    if errors {
        Err(ProcessingError::non_retryable("Invalid Getchu response encoding"))
    } else {
        Ok(text.into_owned())
    }
}
fn parse_detail(html: &str, id: &str) -> PatternVariables {
    let d = Html::parse_document(html);
    let text = |s: &str| {
        Selector::parse(s)
            .ok()
            .and_then(|sel| d.select(&sel).next())
            .map(|e| e.text().collect::<String>().trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let mut v = HashMap::from([("getchuId".into(), id.into())]);
    if let Some(x) = text("#soft-title") {
        v.insert("title".into(), x);
    }
    if let Some(x) = text("#brandsite") {
        v.insert("brand".into(), x);
    }
    let td = Selector::parse("#soft_table td").unwrap();
    let cells = d
        .select(&td)
        .map(|e| e.text().collect::<String>().trim().to_string())
        .collect::<Vec<_>>();
    for (i, x) in cells.iter().enumerate() {
        if x == "発売日："
            && let Some(y) = cells.get(i + 1)
        {
            v.insert("releaseDate".into(), y.clone());
        }
        if x == "品番："
            && let Some(y) = cells.get(i + 1)
        {
            v.insert("isbn".into(), y.clone());
        }
    }
    v
}
#[async_trait]
impl VariableProvider for GetchuVariableProvider {
    async fn item_variables(
        &self,
        i: &SourceItem,
    ) -> Result<HashMap<String, String>, ProcessingError> {
        self.resolve(&i.title).await
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        _: &[SourceFile],
    ) -> Result<Vec<PatternVariables>, ProcessingError> {
        Ok(vec![])
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        v: &str,
    ) -> Result<Option<HashMap<String, Value>>, ProcessingError> {
        let r = self.resolve(v).await?;
        Ok((!r.is_empty())
            .then(|| r.into_iter().map(|(k, v)| (k, Value::String(v))).collect()))
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("title".into())
    }
}

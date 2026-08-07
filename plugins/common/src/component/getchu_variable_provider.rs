use crate::http;
use chardetng::EncodingDetector;
use encoding_rs::Encoding;
use parking_lot::Mutex;
use regex::Regex;
use scraper::{Html, Selector};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
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
        let client = if base.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|e| ComponentError::new(e.to_string()))?
        } else {
            http::build_client()?
        };
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
        None
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
    async fn resolve(&self, text: &str) -> PatternVariables {
        if let Some(v) = self.cache.lock().get(text).cloned() {
            return v;
        }
        let query = ISBN.find(text).map(|m| m.as_str()).unwrap_or(text);
        let vars = self.search(query).await.unwrap_or_default();
        let mut c = self.cache.lock();
        if c.len() >= 500
            && let Some(k) = c.keys().next().cloned()
        {
            c.remove(&k);
        }
        c.insert(text.into(), vars.clone());
        vars
    }
    async fn search(&self, q: &str) -> Option<PatternVariables> {
        let r = http::execute(
            &self.client,
            self.client
                .get(format!("{}/php/search.phtml", self.base))
                .query(&[("search_keyword", q)])
                .header("Cookie", "getchu_adalt_flag=getchu.com"),
            "Search Getchu",
        )
        .await
        .ok()?;
        let html = decode_response(r).await?;
        let links = {
            let d = Html::parse_document(&html);
            let s = Selector::parse(".search_container .display li #detail_block .blueb")
                .ok()?;
            d.select(&s)
                .filter_map(|e| {
                    let title = e.text().collect::<String>();
                    let href = e.value().attr("href")?;
                    Some((title, href.to_string()))
                })
                .collect::<Vec<_>>()
        };
        let url = links.into_iter().min_by_key(|(title, _)| title.chars().count())?.1;
        self.detail(&url).await
    }
    async fn detail(&self, url: &str) -> Option<PatternVariables> {
        let url = reqwest::Url::parse(&self.base).ok()?.join(url).ok()?;
        let id = url.query_pairs().find(|(k, _)| k == "id")?.1.to_string();
        let r = http::execute(
            &self.client,
            self.client.get(url).header("Cookie", "getchu_adalt_flag=getchu.com"),
            "Fetch Getchu item",
        )
        .await
        .ok()?;
        let html = decode_response(r).await?;
        Some(parse_detail(&html, &id))
    }
}
async fn decode_response(r: reqwest::Response) -> Option<String> {
    let charset = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("charset=").nth(1))
        .and_then(|v| Encoding::for_label(v.trim().as_bytes()));
    let b = r.bytes().await.ok()?;
    let enc = charset.unwrap_or_else(|| {
        let mut d = EncodingDetector::new();
        d.feed(&b, true);
        d.guess(None, true)
    });
    let (text, _, errors) = enc.decode(&b);
    (!errors).then(|| text.into_owned())
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
    async fn item_variables(&self, i: &SourceItem) -> HashMap<String, String> {
        self.resolve(&i.title).await
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        _: &[SourceFile],
    ) -> Vec<PatternVariables> {
        vec![]
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        v: &str,
    ) -> Option<HashMap<String, Value>> {
        let r = self.resolve(v).await;
        if r.is_empty() {
            None
        } else {
            Some(r.into_iter().map(|(k, v)| (k, Value::String(v))).collect())
        }
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("title".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_detail_fields() {
        let v = parse_detail(
            "<h1 id='soft-title'>作品</h1><div id='brandsite'>Brand</div><table id='soft_table'><tr><td>発売日：</td><td>2024/01/02</td></tr><tr><td>品番：</td><td>ABC-1</td></tr></table>",
            "1",
        );
        assert_eq!(Some("作品"), v.get("title").map(String::as_str));
        assert_eq!(Some("ABC-1"), v.get("isbn").map(String::as_str));
    }
    #[test]
    fn detects_identifier() {
        assert_eq!(Some("ABC-123"), ISBN.find("title ABC-123").map(|m| m.as_str()));
    }
}

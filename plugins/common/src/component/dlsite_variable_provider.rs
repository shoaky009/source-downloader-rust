use crate::api::dlsite::DlsiteClient;
use crate::http::HttpClient;
use parking_lot::Mutex;
use regex::Regex;
use scraper::{Html, Selector};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct DlsiteVariableProviderSupplier;
pub const SUPPLIER: DlsiteVariableProviderSupplier = DlsiteVariableProviderSupplier;
impl ComponentSupplier for DlsiteVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("dlsite".into())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let locale =
            p.get("locale").and_then(Value::as_str).unwrap_or("ja-jp").to_string();
        let only = p
            .get("only-extract-id")
            .map(|value| {
                serde_json::from_value::<bool>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'only-extract-id': {error}"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(false);
        let prefer = p
            .get("prefer-suggest")
            .map(|value| {
                serde_json::from_value::<bool>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'prefer-suggest': {error}"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(true);
        let base = p
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://www.dlsite.com")
            .trim_end_matches('/')
            .to_string();
        let http = HttpClient::new()?;
        Ok(Arc::new(DlsiteVariableProvider {
            client: DlsiteClient::new(http, base, &locale),
            only,
            prefer,
            cache: Mutex::new(HashMap::new()),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Resolves DLsite work variables from its web API.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "locale":{"type":"string","default":"ja-jp"},
                    "only-extract-id":{"type":"boolean","default":false},
                    "prefer-suggest":{"type":"boolean","default":true},
                    "base-url":{"type":"string","default":"https://www.dlsite.com"}
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
struct DlsiteVariableProvider {
    client: DlsiteClient,
    only: bool,
    prefer: bool,
    cache: Mutex<HashMap<String, PatternVariables>>,
}

impl Display for DlsiteVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "dlsite")
    }
}

static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:RJ|VJ)\d+").unwrap());
impl DlsiteVariableProvider {
    async fn resolve(&self, text: &str) -> PatternVariables {
        let key = text.to_string();
        if let Some(v) = self.cache.lock().get(&key).cloned() {
            return v;
        }
        let id = ID.find(text).map(|m| m.as_str().to_string());
        if self.only && id.is_none() {
            return HashMap::new();
        }
        let id = match id {
            Some(v) => Some(v),
            None => self.keyword(text).await,
        };
        let vars = match id {
            Some(id) => self
                .detail(&id)
                .await
                .unwrap_or_else(|| HashMap::from([("dlsiteId".into(), id)])),
            None => HashMap::new(),
        };
        let mut c = self.cache.lock();
        if c.len() >= 500
            && let Some(k) = c.keys().next().cloned()
        {
            c.remove(&k);
        }
        c.insert(key, vars.clone());
        vars
    }
    async fn keyword(&self, text: &str) -> Option<String> {
        if self.prefer
            && let Some(v) = self.suggest(text).await
        {
            return Some(v);
        }
        let html = self.client.search(text).await.ok()?;
        let found = {
            let doc = Html::parse_document(&html);
            let sel = Selector::parse("#search_result_img_box .work_name a").ok()?;
            doc.select(&sel).find_map(|element| {
                element
                    .value()
                    .attr("href")
                    .and_then(|href| ID.find(href))
                    .map(|id| id.as_str().to_string())
            })
        };
        if found.is_some() {
            return found;
        }
        if !self.prefer { self.suggest(text).await } else { None }
    }
    async fn suggest(&self, text: &str) -> Option<String> {
        self.client.suggest(text).await.ok().flatten()
    }
    async fn detail(&self, id: &str) -> Option<PatternVariables> {
        let html = self.client.work(id).await.ok()?;
        Some(parse_detail(&html, id))
    }
}
fn parse_detail(html: &str, id: &str) -> PatternVariables {
    let d = Html::parse_document(html);
    let text = |s: &str| {
        Selector::parse(s)
            .ok()
            .and_then(|sel| d.select(&sel).next())
            .map(|e| e.text().collect::<String>().trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut v = HashMap::from([("dlsiteId".into(), id.to_string())]);
    if let Some(x) = text("#work_name") {
        v.insert("title".into(), x);
    }
    if let Some(x) = text("#work_maker .maker_name") {
        v.insert("maker".into(), x);
    }
    let tr = Selector::parse("#work_outline tbody tr").unwrap();
    let date_regex = Regex::new(r"(\d{4}).?(\d{2}).?(\d{2})").unwrap();
    for row in d.select(&tr) {
        let cells =
            row.text().map(str::trim).filter(|x| !x.is_empty()).collect::<Vec<_>>();
        if cells.len() < 2 {
            continue;
        }
        let val = cells[1..].join(" ");
        match cells[0] {
            "販売日" | "贩卖日" => {
                v.insert("releaseDate".into(), val.clone());
                if let Some(c) = date_regex.captures(&val) {
                    v.insert("year".into(), c[1].into());
                    v.insert("month".into(), c[2].trim_start_matches('0').into());
                    v.insert("day".into(), c[3].trim_start_matches('0').into());
                }
            }
            "シリーズ名" | "系列名" => {
                v.insert("seriesName".into(), val);
            }
            "作品形式" | "作品类型" => {
                v.insert("productFormat".into(), val);
            }
            "作者" => {
                v.insert("author".into(), val);
            }
            _ => {}
        }
    }
    v
}
#[async_trait]
impl VariableProvider for DlsiteVariableProvider {
    async fn item_variables(&self, i: &SourceItem) -> HashMap<String, String> {
        let link = i.link.to_string();
        let source = ID.find(&link).map(|m| m.as_str()).unwrap_or(&i.title);
        self.resolve(source).await
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
    fn parses_id_and_detail_fixture() {
        assert_eq!(Some("RJ123"), ID.find("x/RJ123.html").map(|m| m.as_str()));
        let v = parse_detail(
            r#"<h1 id='work_name'>作品</h1><div id='work_maker'><span class='maker_name'>社团</span></div><table id='work_outline'><tbody><tr><th>販売日</th><td>2024年05月06日</td></tr></tbody></table>"#,
            "RJ123",
        );
        assert_eq!(Some("作品"), v.get("title").map(String::as_str));
        assert_eq!(Some("2024"), v.get("year").map(String::as_str));
    }
}

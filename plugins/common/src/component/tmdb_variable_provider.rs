use crate::http;
use parking_lot::Mutex;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
pub struct TmdbVariableProviderSupplier;
pub const SUPPLIER: TmdbVariableProviderSupplier = TmdbVariableProviderSupplier;
impl ComponentSupplier for TmdbVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("tmdb".into())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let base = p
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://api.themoviedb.org")
            .trim_end_matches('/')
            .to_string();
        let key = p
            .get("api-key")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::new("Missing or invalid 'api-key' property"))?
            .to_string();
        let language =
            p.get("language").and_then(Value::as_str).unwrap_or("zh-CN").to_string();
        let client = if base.starts_with("http://127.0.0.1:") {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|e| ComponentError::new(e.to_string()))?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(TmdbVariableProvider {
            client,
            base,
            key,
            language,
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
struct TmdbVariableProvider {
    client: reqwest::Client,
    base: String,
    key: String,
    language: String,
    cache: Mutex<HashMap<String, PatternVariables>>,
}

impl Display for TmdbVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "tmdb")
    }
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    results: Vec<ResultItem>,
}
#[derive(Deserialize)]
struct ResultItem {
    id: i64,
    name: String,
    original_name: String,
}
impl TmdbVariableProvider {
    async fn search(&self, q: &str) -> PatternVariables {
        if let Some(v) = self.cache.lock().get(q).cloned() {
            return v;
        }
        let req = self.client.get(format!("{}/3/search/tv", self.base)).query(&[
            ("api_key", self.key.as_str()),
            ("query", q),
            ("language", self.language.as_str()),
            ("page", "1"),
            ("include_adult", "true"),
        ]);
        let vars = match http::execute(&self.client, req, "Search TMDB TV").await {
            Ok(r) => r
                .json::<Page>()
                .await
                .ok()
                .and_then(|p| p.results.into_iter().next())
                .map(|x| {
                    HashMap::from([
                        ("tmdbId".into(), x.id.to_string()),
                        ("tmdbName".into(), x.name),
                        ("originalName".into(), x.original_name),
                    ])
                })
                .unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error=%e,"TMDB search failed");
                HashMap::new()
            }
        };
        let mut c = self.cache.lock();
        if c.len() >= 500
            && let Some(k) = c.keys().next().cloned()
        {
            c.remove(&k);
        }
        c.insert(q.into(), vars.clone());
        vars
    }
}
#[async_trait]
impl VariableProvider for TmdbVariableProvider {
    async fn item_variables(&self, i: &SourceItem) -> HashMap<String, String> {
        self.search(&i.title).await
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
        let mut r = self.search(v).await;
        if r.is_empty()
            && let Some(first) = v.split(' ').next()
        {
            r = self.search(first).await
        }
        if r.is_empty() {
            None
        } else {
            Some(r.into_iter().map(|(k, v)| (k, Value::String(v))).collect())
        }
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("originalName".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, serde_json, time::OffsetDateTime};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    fn item(t: &str) -> SourceItem {
        SourceItem {
            title: t.into(),
            link: Uri::from_static("https://example.com"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: String::new(),
            download_uri: Uri::from_static("https://example.com/file"),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        }
    }
    #[tokio::test]
    async fn searches_maps_and_caches() {
        let s = MockServer::start().await;
        Mock::given(method("GET")).and(path("/3/search/tv")).and(query_param("api_key","key")).and(query_param("query","Frieren")).and(query_param("language","zh-CN")).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"results":[{"id":1,"name":"葬送的芙莉莲","original_name":"葬送のフリーレン"}]}))).expect(1).mount(&s).await;
        let p = Map::from_iter([
            ("base-url".into(), Value::String(s.uri())),
            ("api-key".into(), Value::String("key".into())),
        ]);
        let v = SUPPLIER
            .apply(&source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT, &p)
            .unwrap()
            .as_variable_provider()
            .unwrap();
        assert_eq!(
            Some("葬送のフリーレン"),
            v.item_variables(&item("Frieren"))
                .await
                .get("originalName")
                .map(String::as_str)
        );
        assert_eq!(
            Some("葬送のフリーレン"),
            v.item_variables(&item("Frieren"))
                .await
                .get("originalName")
                .map(String::as_str)
        );
    }
    #[test]
    fn requires_key() {
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

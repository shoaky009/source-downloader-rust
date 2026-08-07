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
use std::collections::{HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct BgmTvVariableProviderSupplier;
pub const SUPPLIER: BgmTvVariableProviderSupplier = BgmTvVariableProviderSupplier;
impl ComponentSupplier for BgmTvVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("bgmtv".to_string())]
    }
    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let base_url = props
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://api.bgm.tv")
            .trim_end_matches('/')
            .to_string();
        let token = props
            .get("token")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| ComponentError::new("Invalid 'token' property"))
            })
            .transpose()?
            .map(str::to_string);
        let client = if base_url.starts_with("http://127.0.0.1:") {
            http::client_builder().no_proxy().build().map_err(|error| {
                ComponentError::new(format!("Failed to build Bangumi client: {error}"))
            })?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(BgmTvVariableProvider {
            client,
            base_url,
            token,
            cache: Mutex::new(Cache::default()),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
#[derive(Debug, Default)]
struct Cache {
    values: HashMap<String, PatternVariables>,
    order: VecDeque<String>,
}
struct BgmTvVariableProvider {
    client: reqwest::Client,
    base_url: String,
    token: Option<String>,
    cache: Mutex<Cache>,
}
impl std::fmt::Debug for BgmTvVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BgmTvVariableProvider").field("base_url", &self.base_url).finish()
    }
}
impl Display for BgmTvVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "bgmtv")
    }
}
impl SdComponent for BgmTvVariableProvider {
    fn as_variable_provider(
        self: Arc<Self>,
    ) -> Result<Arc<dyn VariableProvider>, ComponentError> {
        Ok(self)
    }
}
#[derive(Deserialize)]
struct SearchBody {
    #[serde(default)]
    list: Vec<SubjectItem>,
}
#[derive(Deserialize)]
struct SubjectItem {
    name: String,
}
impl BgmTvVariableProvider {
    async fn search(&self, title: &str) -> PatternVariables {
        if title.trim().is_empty() {
            return HashMap::new();
        }
        if let Some(value) = self.cache.lock().values.get(title).cloned() {
            return value;
        }
        let encoded: String =
            url::form_urlencoded::byte_serialize(title.as_bytes()).collect();
        let mut request = self
            .client
            .get(format!("{}/search/subject/{encoded}", self.base_url))
            .query(&[("type", 2), ("responseGroup", 0)]);
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let variables =
            match http::execute(&self.client, request, "Search Bangumi subject").await {
                Ok(response) => match response.json::<SearchBody>().await {
                    Ok(body) => body
                        .list
                        .first()
                        .map(|subject| {
                            HashMap::from([(
                                "nativeName".to_string(),
                                subject.name.clone(),
                            )])
                        })
                        .unwrap_or_default(),
                    Err(_) => HashMap::new(),
                },
                Err(error) => {
                    tracing::warn!(error = %error, "Bangumi search failed");
                    HashMap::new()
                }
            };
        let mut cache = self.cache.lock();
        if cache.values.len() == 500
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.values.remove(&oldest);
        }
        cache.order.push_back(title.to_string());
        cache.values.insert(title.to_string(), variables.clone());
        variables
    }
}
#[async_trait]
impl VariableProvider for BgmTvVariableProvider {
    async fn item_variables(&self, item: &SourceItem) -> HashMap<String, String> {
        self.search(item.title.trim()).await
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
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        Some(
            self.search(value)
                .await
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        )
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("nativeName".to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    fn item(title: &str) -> SourceItem {
        SourceItem {
            title: title.to_string(),
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
    async fn searches_first_subject_with_token_and_caches() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/search/subject/Frieren")).and(query_param("type", "2")).and(query_param("responseGroup", "0")).and(header("authorization", "Bearer token")).respond_with(ResponseTemplate::new(200).set_body_json(source_downloader_sdk::serde_json::json!({"results":1,"list":[{"id":1,"name":"葬送のフリーレン","name_cn":"葬送的芙莉莲","url":"/subject/1"}]}))).expect(1).mount(&server).await;
        let props = Map::from_iter([
            ("base-url".to_string(), Value::String(server.uri())),
            ("token".to_string(), Value::String("token".to_string())),
        ]);
        let provider = SUPPLIER.apply(&props).unwrap().as_variable_provider().unwrap();
        assert_eq!(
            Some("葬送のフリーレン"),
            provider
                .item_variables(&item("Frieren"))
                .await
                .get("nativeName")
                .map(String::as_str)
        );
        assert_eq!(
            Some("葬送のフリーレン"),
            provider
                .item_variables(&item("Frieren"))
                .await
                .get("nativeName")
                .map(String::as_str)
        );
    }
    #[tokio::test]
    async fn empty_input_does_not_request() {
        let server = MockServer::start().await;
        let props =
            Map::from_iter([("base-url".to_string(), Value::String(server.uri()))]);
        let provider = SUPPLIER.apply(&props).unwrap().as_variable_provider().unwrap();
        assert!(provider.item_variables(&item(" ")).await.is_empty());
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

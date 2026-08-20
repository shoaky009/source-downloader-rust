use crate::api::bangumi::BangumiClient;
use crate::http::{self, HttpClient};
use parking_lot::Mutex;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{self, Map, Value, json};
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
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
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
                serde_json::from_value::<String>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'token': {error}"
                    ))
                })
            })
            .transpose()?;
        let http = if base_url.starts_with("http://127.0.0.1:") {
            HttpClient::from_reqwest(http::client_builder().no_proxy().build().map_err(
                |error| {
                    ComponentError::new(format!(
                        "Failed to build Bangumi client: {error}"
                    ))
                },
            )?)
        } else {
            HttpClient::new()?
        };
        Ok(Arc::new(BgmTvVariableProvider {
            client: BangumiClient::new(http, base_url, token),
            cache: Mutex::new(Cache::default()),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Resolves variables through the Bangumi API.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "base-url":{"type":"string","default":"https://api.bgm.tv"},
                    "token":{"type":"string"}
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}
#[derive(Debug, Default)]
struct Cache {
    values: HashMap<String, PatternVariables>,
    order: VecDeque<String>,
}
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
struct BgmTvVariableProvider {
    client: BangumiClient,
    cache: Mutex<Cache>,
}

impl Display for BgmTvVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "bgmtv")
    }
}

impl BgmTvVariableProvider {
    async fn search(&self, title: &str) -> PatternVariables {
        if title.trim().is_empty() {
            return HashMap::new();
        }
        if let Some(value) = self.cache.lock().values.get(title).cloned() {
            return value;
        }
        let variables = match self.client.search_legacy_subject(title).await {
            Ok(Some(name)) => HashMap::from([("nativeName".to_string(), name)]),
            Ok(None) => HashMap::new(),
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
        let provider = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap()
            .as_variable_provider()
            .unwrap();
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
        let provider = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap()
            .as_variable_provider()
            .unwrap();
        assert!(provider.item_variables(&item(" ")).await.is_empty());
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

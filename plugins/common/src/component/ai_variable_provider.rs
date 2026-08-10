use crate::http;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct AiVariableProviderSupplier;
pub const SUPPLIER: AiVariableProviderSupplier = AiVariableProviderSupplier;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AiConfig {
    api_keys: Vec<String>,
    #[serde(default)]
    resolve_variables: Vec<String>,
    #[serde(default = "default_api_host")]
    api_host: String,
    system_role: Option<String>,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default = "default_temperature")]
    temperature: f64,
    primary: Option<String>,
}
fn default_api_host() -> String {
    "https://api.openai.com".to_string()
}
fn default_model() -> String {
    "gpt-3.5-turbo".to_string()
}
fn default_temperature() -> f64 {
    0.85
}

impl ComponentSupplier for AiVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("ai".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: AiConfig = deserialize_component_config(props)?;
        if config.api_keys.is_empty() {
            return Err(ComponentError::new(
                "Invalid configuration at 'api-keys': must not be empty",
            ));
        }
        let system_role = config.system_role.unwrap_or_else(|| format!("你现在是一个文件解析器，从文件名中解析信息\n需要的信息有:{:?}\n如果不存在字段无需返回，以json的格式返回", config.resolve_variables));
        let client = if config.api_host.starts_with("http://127.0.0.1:") {
            http::client_builder().no_proxy().build().map_err(|error| {
                ComponentError::new(format!("Failed to build AI HTTP client: {error}"))
            })?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(AiVariableProvider {
            client,
            endpoint: format!(
                "{}/v1/chat/completions",
                config.api_host.trim_end_matches('/')
            ),
            api_keys: config.api_keys,
            system_role,
            model: config.model,
            temperature: config.temperature,
            primary: config.primary,
            next_key: AtomicUsize::new(0),
            cache: Mutex::new(Cache::default()),
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Uses an OpenAI-compatible API to resolve filename variables."
                .into(),
            props_json_schema: Some(
                json!({"type":"object","properties":{"api-keys":{"type":"array","items":{"type":"string"},"minItems":1},"resolve-variables":{"type":"array","items":{"type":"string"},"default":[]},"api-host":{"type":"string","default":"https://api.openai.com"},"system-role":{"type":"string"},"model":{"type":"string","default":"gpt-3.5-turbo"},"temperature":{"type":"number","default":0.85},"primary":{"type":"string"}},"required":["api-keys"]}),
            ),
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
struct AiVariableProvider {
    client: reqwest::Client,
    endpoint: String,
    api_keys: Vec<String>,
    system_role: String,
    model: String,
    temperature: f64,
    primary: Option<String>,
    next_key: AtomicUsize,
    cache: Mutex<Cache>,
}

impl Display for AiVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ai")
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    messages: [ChatMessage<'a>; 2],
    model: &'a str,
    temperature: f64,
    stream: bool,
    response_format: ResponseFormat,
}
#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}
#[derive(Serialize)]
struct ResponseFormat {
    r#type: &'static str,
}
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}
#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

impl AiVariableProvider {
    async fn resolve(&self, content: &str) -> PatternVariables {
        if let Some(value) = self.cache.lock().values.get(content).cloned() {
            return value;
        }
        let key_index =
            self.next_key.fetch_add(1, Ordering::Relaxed) % self.api_keys.len();
        let body = ChatRequest {
            messages: [
                ChatMessage { role: "system", content: &self.system_role },
                ChatMessage { role: "user", content },
            ],
            model: &self.model,
            temperature: self.temperature,
            stream: false,
            response_format: ResponseFormat { r#type: "json_object" },
        };
        let response = match http::execute(
            &self.client,
            self.client
                .post(&self.endpoint)
                .bearer_auth(&self.api_keys[key_index])
                .json(&body),
            "Resolve AI variables",
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "AI variable request failed");
                return HashMap::new();
            }
        };
        let response: ChatResponse = match response.json().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "Invalid AI response");
                return HashMap::new();
            }
        };
        let Some(content_value) =
            response.choices.first().map(|choice| choice.message.content.as_str())
        else {
            return HashMap::new();
        };
        let variables: PatternVariables = match serde_json::from_str(content_value) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "Invalid AI variable JSON content");
                return HashMap::new();
            }
        };
        let mut cache = self.cache.lock();
        if cache.values.len() == 500
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.values.remove(&oldest);
        }
        cache.order.push_back(content.to_string());
        cache.values.insert(content.to_string(), variables.clone());
        variables
    }
}

#[async_trait]
impl VariableProvider for AiVariableProvider {
    async fn item_variables(&self, item: &SourceItem) -> HashMap<String, String> {
        self.resolve(&item.title).await
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        files.iter().map(|_| HashMap::new()).collect()
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        Some(
            self.resolve(value)
                .await
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        )
    }
    fn primary_variable_name(&self) -> Option<String> {
        self.primary.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
    use wiremock::matchers::{method, path};
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
    fn props(server: &MockServer) -> Map<String, Value> {
        Map::from_iter([
            ("api-keys".to_string(), serde_json::json!(["secret"])),
            ("api-host".to_string(), Value::String(server.uri())),
            ("system-role".to_string(), Value::String("extract".to_string())),
            ("primary".to_string(), Value::String("title".to_string())),
        ])
    }
    #[test]
    fn validates_required_nonempty_keys() {
        let error = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::new(),
            )
            .unwrap_err();
        assert_eq!(
            error.message,
            "Invalid configuration at '<root>': missing field `api-keys`"
        );
        let error = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::from_iter([("api-keys".to_string(), serde_json::json!([]))]),
            )
            .unwrap_err();
        assert_eq!(
            error.message,
            "Invalid configuration at 'api-keys': must not be empty"
        );
    }
    #[tokio::test]
    async fn requests_json_and_caches_title() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices":[{"message":{"content":"{\"title\":\"Parsed\"}"}}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let provider = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props(&server),
            )
            .unwrap()
            .as_variable_provider()
            .unwrap();
        let variables = provider.item_variables(&item("Show")).await;
        let requests = server.received_requests().await.unwrap();
        assert_eq!(1, requests.len(), "requests={requests:?}");
        assert_eq!(Some("Parsed"), variables.get("title").map(String::as_str));
        assert_eq!(
            Some("Parsed"),
            provider.item_variables(&item("Show")).await.get("title").map(String::as_str)
        );
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert_eq!(
            Some("Bearer secret"),
            requests[0]
                .headers
                .get("authorization")
                .and_then(|value| value.to_str().ok())
        );
        assert_eq!(
            serde_json::json!({"messages":[{"role":"system","content":"extract"},{"role":"user","content":"Show"}],"model":"gpt-3.5-turbo","temperature":0.85,"stream":false,"response_format":{"type":"json_object"}}),
            body,
        );
        assert_eq!(Some("title".to_string()), provider.primary_variable_name());
    }
}

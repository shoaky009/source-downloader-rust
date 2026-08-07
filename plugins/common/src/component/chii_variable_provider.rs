use crate::http;
use serde::{Deserialize, Serialize};
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

pub struct ChiiVariableProviderSupplier;
pub const SUPPLIER: ChiiVariableProviderSupplier = ChiiVariableProviderSupplier;
impl ComponentSupplier for ChiiVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("chii".to_string())]
    }
    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let base_url = props
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://chii.ai")
            .trim_end_matches('/')
            .to_string();
        let client = if base_url.starts_with("http://127.0.0.1:") {
            http::client_builder().no_proxy().build().map_err(|error| {
                ComponentError::new(format!("Failed to build Chii client: {error}"))
            })?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(ChiiVariableProvider {
            client,
            endpoint: format!("{base_url}/graphql"),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
struct ChiiVariableProvider {
    client: reqwest::Client,
    endpoint: String,
}
impl std::fmt::Debug for ChiiVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChiiVariableProvider").field("endpoint", &self.endpoint).finish()
    }
}
impl Display for ChiiVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "chii")
    }
}
impl SdComponent for ChiiVariableProvider {
    fn as_variable_provider(
        self: Arc<Self>,
    ) -> Result<Arc<dyn VariableProvider>, ComponentError> {
        Ok(self)
    }
}
const QUERY: &str = "query SubjectSearch($q: String, $type: String) {\n  querySubjectSearch(q: $q, type: $type) {\n    result {\n      ... on Subject {\n        id\n        name\n        nameCN\n        nsfw\n        date\n      }\n    }\n  }\n}";
#[derive(Serialize)]
struct Request<'a> {
    #[serde(rename = "operationName")]
    operation_name: &'static str,
    query: &'static str,
    variables: Variables<'a>,
}
#[derive(Serialize)]
struct Variables<'a> {
    q: &'a str,
    r#type: &'static str,
}
#[derive(Deserialize)]
struct Response {
    data: Data,
}
#[derive(Deserialize)]
struct Data {
    #[serde(rename = "querySubjectSearch")]
    query_subject_search: Search,
}
#[derive(Deserialize)]
struct Search {
    result: Vec<Subject>,
}
#[derive(Deserialize)]
struct Subject {
    id: String,
    name: String,
    #[serde(rename = "nameCN")]
    name_cn: String,
}
impl ChiiVariableProvider {
    async fn request(&self, text: &str) -> PatternVariables {
        let body = Request {
            operation_name: "SubjectSearch",
            query: QUERY,
            variables: Variables { q: text, r#type: "anime" },
        };
        let response = match http::execute(
            &self.client,
            self.client.post(&self.endpoint).json(&body),
            "Search Chii subject",
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(error = %error, "Chii search failed");
                return HashMap::new();
            }
        };
        response
            .json::<Response>()
            .await
            .ok()
            .and_then(|response| {
                response.data.query_subject_search.result.into_iter().next()
            })
            .map(|subject| {
                HashMap::from([
                    ("bgmtvId".to_string(), subject.id),
                    ("subjectName".to_string(), subject.name),
                    ("subjectNameCn".to_string(), subject.name_cn),
                ])
            })
            .unwrap_or_default()
    }
}
#[async_trait]
impl VariableProvider for ChiiVariableProvider {
    async fn item_variables(&self, item: &SourceItem) -> HashMap<String, String> {
        self.request(&item.title).await
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
            self.request(value)
                .await
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        )
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("subjectName".to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, serde_json, time::OffsetDateTime};
    use wiremock::matchers::{body_partial_json, method, path};
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
    async fn posts_graphql_and_maps_first_subject() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/graphql")).and(body_partial_json(serde_json::json!({"operationName":"SubjectSearch","variables":{"q":"Frieren","type":"anime"}}))).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":{"querySubjectSearch":{"result":[{"id":"1","name":"葬送のフリーレン","nameCN":"葬送的芙莉莲"}]}}}))).mount(&server).await;
        let provider = SUPPLIER
            .apply(&Map::from_iter([(
                "base-url".to_string(),
                Value::String(server.uri()),
            )]))
            .unwrap()
            .as_variable_provider()
            .unwrap();
        let variables = provider.item_variables(&item("Frieren")).await;
        assert_eq!(Some("1"), variables.get("bgmtvId").map(String::as_str));
        assert_eq!(
            Some("葬送のフリーレン"),
            variables.get("subjectName").map(String::as_str)
        );
        assert_eq!(
            Some("葬送的芙莉莲"),
            variables.get("subjectNameCn").map(String::as_str)
        );
    }
    #[tokio::test]
    async fn empty_result_is_empty() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"data":{"querySubjectSearch":{"result":[]}}}),
            ))
            .mount(&server)
            .await;
        let provider = SUPPLIER
            .apply(&Map::from_iter([(
                "base-url".to_string(),
                Value::String(server.uri()),
            )]))
            .unwrap()
            .as_variable_provider()
            .unwrap();
        assert!(provider.item_variables(&item("none")).await.is_empty());
    }
}

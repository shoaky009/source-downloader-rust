use crate::api::chii::ChiiClient;
use crate::http::HttpClient;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
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
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let base_url = props
            .get("base-url")
            .and_then(Value::as_str)
            .unwrap_or("https://chii.ai")
            .trim_end_matches('/')
            .to_string();
        let http = HttpClient::new()?;
        Ok(Arc::new(ChiiVariableProvider { client: ChiiClient::new(http, base_url) }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Resolves variables through the Chii GraphQL service.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "base-url":{"type":"string","default":"https://chii.ai"}
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
struct ChiiVariableProvider {
    client: ChiiClient,
}

impl Display for ChiiVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "chii")
    }
}

impl ChiiVariableProvider {
    async fn request(&self, text: &str) -> Result<PatternVariables, ProcessingError> {
        match self.client.search_subject(text).await? {
            Some(subject) => Ok(HashMap::from([
                ("bgmtvId".to_string(), subject.id),
                ("subjectName".to_string(), subject.name),
                ("subjectNameCn".to_string(), subject.name_cn),
            ])),
            None => Ok(HashMap::new()),
        }
    }
}

#[async_trait]
impl VariableProvider for ChiiVariableProvider {
    async fn item_variables(
        &self,
        item: &SourceItem,
    ) -> Result<HashMap<String, String>, ProcessingError> {
        self.request(&item.title).await
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
        value: &str,
    ) -> Result<Option<HashMap<String, Value>>, ProcessingError> {
        Ok(Some(
            self.request(value)
                .await?
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        ))
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
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::from_iter([("base-url".to_string(), Value::String(server.uri()))]),
            )
            .unwrap()
            .as_variable_provider()
            .unwrap();
        let variables = provider.item_variables(&item("Frieren")).await.unwrap();
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
    async fn service_failure_reaches_item_and_extraction_callers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(503))
            .expect(2)
            .mount(&server)
            .await;
        let provider = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::from_iter([("base-url".to_owned(), Value::String(server.uri()))]),
            )
            .unwrap()
            .as_variable_provider()
            .unwrap();
        assert!(matches!(
            provider.item_variables(&item("Frieren")).await,
            Err(ProcessingError::Retryable { .. })
        ));
        assert!(matches!(
            provider.extract_from(&item("Frieren"), "Frieren").await,
            Err(ProcessingError::Retryable { .. })
        ));
    }
}

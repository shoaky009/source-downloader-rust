use reqwest::{Client, ClientBuilder, IntoUrl, RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use source_downloader_sdk::component::{
    ComponentError, ProcessingError, format_error_chain,
};
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub(crate) struct HttpClient {
    inner: Client,
}

impl HttpClient {
    pub(crate) fn new() -> Result<Self, ComponentError> {
        client_builder().build().map(Self::from_reqwest).map_err(|error| {
            ComponentError::new(format!(
                "Failed to build common HTTP client: {}",
                format_error_chain(&error)
            ))
        })
    }

    pub(crate) fn from_reqwest(inner: Client) -> Self {
        Self { inner }
    }

    pub(crate) fn get<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.inner.get(url)
    }

    pub(crate) fn post<U: IntoUrl>(&self, url: U) -> RequestBuilder {
        self.inner.post(url)
    }

    pub(crate) async fn send(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<Response, ProcessingError> {
        execute(&self.inner, request, operation).await
    }

    pub(crate) async fn json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<T, ProcessingError> {
        self.send(request, operation)
            .await?
            .json()
            .await
            .map_err(|error| map_error(error, &format!("Decode {operation} response")))
    }

    pub(crate) async fn text(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<String, ProcessingError> {
        self.send(request, operation)
            .await?
            .text()
            .await
            .map_err(|error| map_error(error, &format!("Read {operation} response")))
    }

    pub(crate) async fn bytes(
        &self,
        request: RequestBuilder,
        operation: &str,
    ) -> Result<bytes::Bytes, ProcessingError> {
        self.send(request, operation)
            .await?
            .bytes()
            .await
            .map_err(|error| map_error(error, &format!("Read {operation} response")))
    }
}

pub(crate) fn client_builder() -> ClientBuilder {
    Client::builder().timeout(DEFAULT_TIMEOUT).cookie_store(true)
}

pub(crate) fn build_client() -> Result<Client, ComponentError> {
    HttpClient::new().map(|client| client.inner)
}

pub(crate) async fn execute(
    client: &Client,
    request: RequestBuilder,
    operation: &str,
) -> Result<Response, ProcessingError> {
    let request = request.build().map_err(|error| map_error(error, operation))?;
    let method = request.method().clone();
    let url = request.url().clone();
    client
        .execute(request)
        .await
        .and_then(Response::error_for_status)
        .map_err(|error| map_error(error, &format!("{operation}: {method} {url}")))
}

pub(crate) fn map_error(error: reqwest::Error, operation: &str) -> ProcessingError {
    let message = format!("{operation}: {}", format_error_chain(&error));
    if error.is_timeout()
        || error.is_connect()
        || error.status().is_some_and(is_retryable_status)
    {
        return ProcessingError::retryable(message);
    }
    ProcessingError::non_retryable(message)
}

fn is_retryable_status(status: StatusCode) -> bool {
    status == StatusCode::REQUEST_TIMEOUT
        || status == StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::serde_json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn execute_returns_response_for_successful_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_builder().no_proxy().build().unwrap();
        let response = execute(
            &client,
            client.get(format!("{}/items", server.uri())),
            "Fetch items",
        )
        .await
        .unwrap();

        assert_eq!(response.text().await.unwrap(), "ok");
    }

    #[tokio::test]
    async fn execute_classifies_server_error_as_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = client_builder().no_proxy().build().unwrap();
        let error = execute(
            &client,
            client.get(format!("{}/items", server.uri())),
            "Fetch items",
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ProcessingError::Retryable { .. }));
        assert!(error.message().contains("GET"));
        assert!(error.message().contains("/items"));
    }

    #[tokio::test]
    async fn helpers_decode_response_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": 7
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/text"))
            .respond_with(ResponseTemplate::new(200).set_body_string("body"))
            .expect(2)
            .mount(&server)
            .await;

        let client =
            HttpClient::from_reqwest(client_builder().no_proxy().build().unwrap());
        let value: serde_json::Value = client
            .json(client.get(format!("{}/json", server.uri())), "Fetch JSON")
            .await
            .unwrap();
        assert_eq!(value["value"], 7);
        assert_eq!(
            client
                .text(client.get(format!("{}/text", server.uri())), "Fetch text")
                .await
                .unwrap(),
            "body"
        );
        assert_eq!(
            client
                .bytes(client.get(format!("{}/text", server.uri())), "Fetch bytes")
                .await
                .unwrap()
                .as_ref(),
            b"body"
        );
    }

    #[tokio::test]
    async fn execute_classifies_client_error_as_non_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/items"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = client_builder().no_proxy().build().unwrap();
        let error = execute(
            &client,
            client.get(format!("{}/items", server.uri())),
            "Fetch items",
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ProcessingError::NonRetryable { .. }));
    }
}

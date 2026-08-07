use reqwest::{Client, ClientBuilder, RequestBuilder, Response, StatusCode};
use source_downloader_sdk::component::{ComponentError, ProcessingError};
use std::time::Duration;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn client_builder() -> ClientBuilder {
    Client::builder().timeout(DEFAULT_TIMEOUT).cookie_store(true)
}

pub(crate) fn build_client() -> Result<Client, ComponentError> {
    client_builder().build().map_err(|error| {
        ComponentError::new(format!("Failed to build common HTTP client: {error}"))
    })
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
    let message = format!("{operation}: {error}");
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

use crate::http::HttpClient;
use source_downloader_sdk::component::ProcessingError;
use source_downloader_sdk::serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(crate) struct PatreonClient {
    http: HttpClient,
    base_url: String,
    headers: HashMap<String, String>,
}

impl PatreonClient {
    pub(crate) fn new(
        http: HttpClient,
        base_url: String,
        session_id: &str,
        mut headers: HashMap<String, String>,
    ) -> Self {
        headers.entry("Cookie".to_string()).or_insert_with(|| {
            format!(
                "session_id={session_id}; \
                 patreon_location_country_code=CN; patreon_locale_code=zh-CN;"
            )
        });
        Self { http, base_url: base_url.trim_end_matches('/').to_string(), headers }
    }

    pub(crate) fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.headers.iter().fold(
            self.http.get(format!("{}{}", self.base_url, path)),
            |request, (key, value)| request.header(key, value),
        )
    }

    async fn json(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<Value, ProcessingError> {
        self.http.json(request, operation).await.map_err(|error| {
            ProcessingError::non_retryable(format!(
                "Invalid Patreon response: {}",
                error.message()
            ))
        })
    }

    pub(crate) async fn pledges(&self) -> Result<Value, ProcessingError> {
        self.json(self.request("/api/pledges"), "Fetch Patreon pledges").await
    }

    pub(crate) async fn post_tags(
        &self,
        campaign_id: i64,
    ) -> Result<Value, ProcessingError> {
        self.json(
            self.request(&format!("/api/campaigns/{campaign_id}/post-tags")),
            "Fetch Patreon post tags",
        )
        .await
    }

    pub(crate) async fn posts(
        &self,
        campaign_id: i64,
        month: &str,
    ) -> Result<Value, ProcessingError> {
        let request = self.request("/api/posts").query(&[
            ("filter[campaign_id]", campaign_id.to_string()),
            ("filter[month]", month.to_string()),
            ("sort", "published_at".to_string()),
        ]);
        self.json(request, "Fetch Patreon posts").await
    }

    pub(crate) async fn post(&self, post_id: &str) -> Result<Value, ProcessingError> {
        self.json(self.request(&format!("/api/posts/{post_id}")), "Fetch Patreon post")
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http;
    use source_downloader_sdk::serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> PatreonClient {
        PatreonClient::new(
            HttpClient::from_reqwest(http::client_builder().no_proxy().build().unwrap()),
            server.uri(),
            "session_token",
            HashMap::new(),
        )
    }

    #[tokio::test]
    async fn posts_send_authentication_and_filters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/posts"))
            .and(query_param("filter[campaign_id]", "42"))
            .and(query_param("filter[month]", "2026-08"))
            .and(query_param("sort", "published_at"))
            .and(header(
                "cookie",
                "session_id=session_token; patreon_location_country_code=CN; \
                 patreon_locale_code=zh-CN;",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .expect(1)
            .mount(&server)
            .await;

        assert!(
            client(&server).posts(42, "2026-08").await.unwrap()["data"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn post_uses_custom_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/posts/7"))
            .and(header("cookie", "custom-cookie"))
            .and(header("x-test", "custom-value"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {}})))
            .expect(1)
            .mount(&server)
            .await;
        let client = PatreonClient::new(
            HttpClient::from_reqwest(http::client_builder().no_proxy().build().unwrap()),
            server.uri(),
            "ignored",
            HashMap::from([
                ("Cookie".to_string(), "custom-cookie".to_string()),
                ("X-Test".to_string(), "custom-value".to_string()),
            ]),
        );

        client.post("7").await.unwrap();
    }
}

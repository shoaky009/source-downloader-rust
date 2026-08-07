use crate::http::HttpClient;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use source_downloader_sdk::component::{ComponentError, ProcessingError};
use source_downloader_sdk::serde_json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(crate) struct PixivClient {
    http: HttpClient,
    base_url: String,
    headers: HashMap<String, String>,
}

#[derive(Debug)]
pub(crate) struct RemoteFile {
    pub(crate) name: String,
    pub(crate) size: Option<u64>,
    pub(crate) bytes: bytes::Bytes,
}

#[derive(Deserialize)]
struct Response<T> {
    body: T,
    error: bool,
    message: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Bookmarks {
    #[serde(default)]
    pub(crate) works: Vec<Illustration>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Illustration {
    pub(crate) id: i64,
    pub(crate) title: String,
    pub(crate) illust_type: i64,
    #[serde(default)]
    pub(crate) tags: Vec<String>,
    pub(crate) user_id: i64,
    pub(crate) user_name: String,
    pub(crate) url: String,
    pub(crate) x_restrict: i64,
    pub(crate) create_date: String,
    pub(crate) bookmark_data: Option<Bookmark>,
    #[serde(default)]
    pub(crate) is_masked: bool,
}

#[derive(Deserialize)]
pub(crate) struct Bookmark {
    pub(crate) id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Page {
    pub(crate) urls: HashMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Ugoira {
    pub(crate) original_src: String,
}

impl PixivClient {
    pub(crate) fn new(
        http: HttpClient,
        base_url: String,
        session_id: &str,
    ) -> Result<Self, ComponentError> {
        if session_id.trim().is_empty() {
            return Err(ComponentError::new("Invalid empty Pixiv session-id"));
        }
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            headers: HashMap::from([
                ("Cookie".to_string(), format!("PHPSESSID={session_id}; ")),
                ("Referer".to_string(), "https://www.pixiv.net/".to_string()),
            ]),
        })
    }

    pub(crate) fn headers(&self) -> HashMap<String, String> {
        self.headers.clone()
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        self.authenticated(self.http.get(format!("{}{}", self.base_url, path)))
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        self.headers
            .iter()
            .fold(request, |request, (key, value)| request.header(key, value))
    }

    async fn json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
    ) -> Result<T, ProcessingError> {
        let response =
            self.http.json::<Response<T>>(request, operation).await.map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Invalid Pixiv response: {}",
                    error.message()
                ))
            })?;
        if response.error {
            Err(ProcessingError::non_retryable(
                response.message.unwrap_or_else(|| "Pixiv API error".to_string()),
            ))
        } else {
            Ok(response.body)
        }
    }

    pub(crate) async fn bookmarks(
        &self,
        user_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<Bookmarks, ProcessingError> {
        let request =
            self.request(&format!("/ajax/user/{user_id}/illusts/bookmarks")).query(&[
                ("tag", "".to_string()),
                ("offset", offset.to_string()),
                ("limit", limit.to_string()),
                ("rest", "show".to_string()),
                ("lang", "zh".to_string()),
            ]);
        self.json(request, "Fetch Pixiv bookmarks").await
    }

    pub(crate) async fn following(
        &self,
        user_id: i64,
        offset: u32,
        limit: u32,
    ) -> Result<Value, ProcessingError> {
        let request = self.request(&format!("/ajax/user/{user_id}/following")).query(&[
            ("offset", offset.to_string()),
            ("limit", limit.to_string()),
            ("rest", "show".to_string()),
        ]);
        self.json(request, "Fetch Pixiv followings").await
    }

    pub(crate) async fn pages(
        &self,
        illustration_id: i64,
    ) -> Result<Vec<Page>, ProcessingError> {
        self.json(
            self.request(&format!("/ajax/illust/{illustration_id}/pages")),
            "Fetch Pixiv pages",
        )
        .await
    }

    pub(crate) async fn ugoira_metadata(
        &self,
        illustration_id: i64,
    ) -> Result<Ugoira, ProcessingError> {
        self.json(
            self.request(&format!("/ajax/illust/{illustration_id}/ugoira_meta")),
            "Fetch Pixiv ugoira metadata",
        )
        .await
    }

    pub(crate) async fn download(
        &self,
        url: &str,
    ) -> Result<RemoteFile, ProcessingError> {
        let response = self
            .http
            .send(self.authenticated(self.http.get(url)), "Fetch Pixiv file")
            .await?;
        let size = response.content_length();
        let bytes = response.bytes().await.map_err(|error| {
            ProcessingError::non_retryable(format!("Read Pixiv file: {error}"))
        })?;
        let name = reqwest::Url::parse(url)
            .ok()
            .and_then(|url| url.path_segments()?.next_back().map(str::to_string))
            .ok_or_else(|| ProcessingError::non_retryable("Pixiv filename missing"))?;
        Ok(RemoteFile { name, size, bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http;
    use source_downloader_sdk::serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> PixivClient {
        PixivClient::new(
            HttpClient::from_reqwest(http::client_builder().no_proxy().build().unwrap()),
            server.uri(),
            "123_token",
        )
        .unwrap()
    }

    #[tokio::test]
    async fn bookmarks_send_authentication_and_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax/user/123/illusts/bookmarks"))
            .and(query_param("offset", "50"))
            .and(query_param("limit", "25"))
            .and(header("cookie", "PHPSESSID=123_token;"))
            .and(header("referer", "https://www.pixiv.net/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": false,
                "message": null,
                "body": {"works": []}
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert!(client(&server).bookmarks(123, 50, 25).await.unwrap().works.is_empty());
    }

    #[tokio::test]
    async fn reports_pixiv_error_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/ajax/illust/42/pages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": true,
                "message": "not available",
                "body": []
            })))
            .mount(&server)
            .await;

        let error = client(&server).pages(42).await.unwrap_err();
        assert!(error.message().contains("not available"));
    }

    #[tokio::test]
    async fn downloads_authenticated_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/files/42.zip"))
            .and(header("cookie", "PHPSESSID=123_token;"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"zip"))
            .expect(1)
            .mount(&server)
            .await;

        let file = client(&server)
            .download(&format!("{}/files/42.zip", server.uri()))
            .await
            .unwrap();
        assert_eq!(file.name, "42.zip");
        assert_eq!(file.bytes.as_ref(), b"zip");
    }
}

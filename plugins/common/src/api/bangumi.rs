use crate::http::HttpClient;
use serde::Deserialize;
use source_downloader_sdk::component::ProcessingError;
use source_downloader_sdk::serde_json::json;

#[derive(Clone, Debug)]
pub(crate) struct BangumiClient {
    http: HttpClient,
    base_url: String,
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<BangumiSubject>,
}

#[derive(Debug, Deserialize)]
struct LegacySearchResponse {
    #[serde(default)]
    list: Vec<LegacyBangumiSubject>,
}

#[derive(Debug, Deserialize)]
struct LegacyBangumiSubject {
    name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct BangumiSubject {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) name_cn: String,
    pub(crate) date: Option<String>,
}

impl BangumiClient {
    pub(crate) fn new(http: HttpClient, base_url: String, token: Option<String>) -> Self {
        Self { http, base_url: base_url.trim_end_matches('/').to_string(), token }
    }

    fn authenticate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token),
            None => request,
        }
    }

    pub(crate) async fn search_subjects(
        &self,
        keyword: &str,
    ) -> Result<Vec<BangumiSubject>, ProcessingError> {
        let request = self.authenticate(
            self.http.post(format!("{}/v0/search/subjects", self.base_url)).json(
                &json!({
                    "keyword": keyword,
                    "filter": {"type": [2], "nsfw": true}
                }),
            ),
        );
        self.http
            .json::<SearchResponse>(request, "Search Bangumi subjects")
            .await
            .map(|response| response.data)
    }

    pub(crate) async fn search_legacy_subject(
        &self,
        keyword: &str,
    ) -> Result<Option<String>, ProcessingError> {
        let encoded: String =
            url::form_urlencoded::byte_serialize(keyword.as_bytes()).collect();
        let request = self.authenticate(
            self.http
                .get(format!("{}/search/subject/{encoded}", self.base_url))
                .query(&[("type", 2), ("responseGroup", 0)]),
        );
        self.http
            .json::<LegacySearchResponse>(request, "Search legacy Bangumi subject")
            .await
            .map(|response| response.list.into_iter().next().map(|subject| subject.name))
    }

    pub(crate) async fn get_subject(
        &self,
        subject_id: &str,
    ) -> Result<BangumiSubject, ProcessingError> {
        let request = self.authenticate(
            self.http.get(format!("{}/v0/subjects/{subject_id}", self.base_url)),
        );
        self.http.json(request, "Fetch Bangumi subject").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> BangumiClient {
        let http =
            HttpClient::from_reqwest(http::client_builder().no_proxy().build().unwrap());
        BangumiClient::new(http, server.uri(), Some("token".to_string()))
    }

    #[tokio::test]
    async fn searches_v0_subjects_with_authentication() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v0/search/subjects"))
            .and(header("authorization", "Bearer token"))
            .and(body_json(json!({
                "keyword": "Frieren",
                "filter": {"type": [2], "nsfw": true}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"name": "葬送のフリーレン", "name_cn": "", "date": null}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let subjects = client(&server).search_subjects("Frieren").await.unwrap();
        assert_eq!(subjects[0].name, "葬送のフリーレン");
    }

    #[tokio::test]
    async fn searches_legacy_subject_with_query_parameters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/subject/Frieren"))
            .and(query_param("type", "2"))
            .and(query_param("responseGroup", "0"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "list": [{"name": "葬送のフリーレン"}]
            })))
            .expect(1)
            .mount(&server)
            .await;

        assert_eq!(
            client(&server).search_legacy_subject("Frieren").await.unwrap().as_deref(),
            Some("葬送のフリーレン")
        );
    }

    #[tokio::test]
    async fn gets_subject_with_authentication() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v0/subjects/42"))
            .and(header("authorization", "Bearer token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "Frieren",
                "name_cn": "芙莉莲",
                "date": "2023-09-29"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let subject = client(&server).get_subject("42").await.unwrap();
        assert_eq!(subject.name_cn, "芙莉莲");
        assert_eq!(subject.date.as_deref(), Some("2023-09-29"));
    }
}

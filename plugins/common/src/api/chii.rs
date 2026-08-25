use crate::http::HttpClient;
use serde::{Deserialize, Serialize};
use source_downloader_sdk::component::ProcessingError;

const QUERY: &str = "query SubjectSearch($q: String, $type: String) {\n  querySubjectSearch(q: $q, type: $type) {\n    result {\n      ... on Subject {\n        id\n        name\n        nameCN\n        nsfw\n        date\n      }\n    }\n  }\n}";

#[derive(Clone, Debug)]
pub(crate) struct ChiiClient {
    http: HttpClient,
    endpoint: String,
}

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
    result: Vec<ChiiSubject>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ChiiSubject {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(rename = "nameCN")]
    pub(crate) name_cn: String,
}

impl ChiiClient {
    pub(crate) fn new(http: HttpClient, base_url: String) -> Self {
        Self { http, endpoint: format!("{}/graphql", base_url.trim_end_matches('/')) }
    }

    pub(crate) async fn search_subject(
        &self,
        text: &str,
    ) -> Result<Option<ChiiSubject>, ProcessingError> {
        let body = Request {
            operation_name: "SubjectSearch",
            query: QUERY,
            variables: Variables { q: text, r#type: "anime" },
        };
        self.http
            .json::<Response>(
                self.http.post(&self.endpoint).json(&body),
                "Search Chii subject",
            )
            .await
            .map(|response| response.data.query_subject_search.result.into_iter().next())
    }
}

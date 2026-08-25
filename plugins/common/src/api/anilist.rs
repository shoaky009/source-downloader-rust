use crate::http::HttpClient;
use serde::Deserialize;
use source_downloader_sdk::component::ProcessingError;
use source_downloader_sdk::serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct AniListClient {
    http: HttpClient,
    endpoint: String,
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<Value>,
}

#[derive(Deserialize)]
struct Data {
    #[serde(rename = "Page")]
    page: Page,
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    media: Vec<Media>,
}

#[derive(Deserialize)]
struct Media {
    title: AniListTitle,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct AniListTitle {
    pub(crate) romaji: Option<String>,
    pub(crate) native: Option<String>,
}

impl AniListClient {
    pub(crate) fn new(http: HttpClient, endpoint: String) -> Self {
        Self { http, endpoint: endpoint.trim_end_matches('/').to_string() }
    }

    pub(crate) async fn search(
        &self,
        title: &str,
    ) -> Result<Option<AniListTitle>, ProcessingError> {
        let request = self.http.post(&self.endpoint).json(&json!({
            "query": "query ($search: String) { Page(page: 1, perPage: 10) { media(search: $search, type: ANIME) { title { romaji native } } } }",
            "variables": { "search": title }
        }));
        let response = self.http.json::<Response>(request, "Search AniList").await?;
        if !response.errors.is_empty() {
            return Ok(None);
        }
        Ok(response
            .data
            .and_then(|data| data.page.media.into_iter().next().map(|media| media.title)))
    }
}

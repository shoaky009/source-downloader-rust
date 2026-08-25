use crate::http::HttpClient;
use serde::Deserialize;
use source_downloader_sdk::component::ProcessingError;

#[derive(Clone, Debug)]
pub(crate) struct DlsiteClient {
    http: HttpClient,
    base_url: String,
    cookie: String,
}

#[derive(Deserialize)]
struct SuggestResponse {
    #[serde(default)]
    work: Vec<SuggestWork>,
}

#[derive(Deserialize)]
struct SuggestWork {
    #[serde(rename = "workno")]
    id: String,
}

impl DlsiteClient {
    pub(crate) fn new(http: HttpClient, base_url: String, locale: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            cookie: format!("locale={locale}; adultchecked=1"),
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{}", self.base_url, path)).header("Cookie", &self.cookie)
    }

    pub(crate) async fn search(&self, keyword: &str) -> Result<String, ProcessingError> {
        let encoded: String =
            url::form_urlencoded::byte_serialize(keyword.as_bytes()).collect();
        self.http
            .text(
                self.get(&format!("/maniax/fsr/=/language/jp/keyword/{encoded}")),
                "Search DLsite",
            )
            .await
    }

    pub(crate) async fn suggest(
        &self,
        keyword: &str,
    ) -> Result<Option<String>, ProcessingError> {
        self.http
            .json::<SuggestResponse>(
                self.get("/suggest/").query(&[("term", keyword), ("site", "pro")]),
                "Suggest DLsite work",
            )
            .await
            .map(|response| response.work.into_iter().next().map(|work| work.id))
    }

    pub(crate) async fn work(&self, id: &str) -> Result<String, ProcessingError> {
        self.http
            .text(
                self.get(&format!("/home/work/=/product_id/{id}.html")),
                "Fetch DLsite work",
            )
            .await
    }
}

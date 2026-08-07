use crate::http;
use scraper::{Html, Selector};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct HtmlFileResolverSupplier;
pub const SUPPLIER: HtmlFileResolverSupplier = HtmlFileResolverSupplier;
impl ComponentSupplier for HtmlFileResolverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_resolver("html".into())]
    }
    fn apply(
        &self,
        p: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let css = p.get("css-selector").and_then(Value::as_str).ok_or_else(|| {
            ComponentError::new("Missing or invalid 'css-selector' property")
        })?;
        let selector = Selector::parse(css)
            .map_err(|e| ComponentError::new(format!("Invalid 'css-selector': {e}")))?;
        let attr = p
            .get("extract-attribute")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ComponentError::new("Missing or invalid 'extract-attribute' property")
            })?
            .to_string();
        let direct = p
            .get("direct-mode")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| ComponentError::new("Invalid 'direct-mode' property"))
            })
            .transpose()?
            .unwrap_or(false);
        let no_proxy = p.get("no-proxy").and_then(Value::as_bool).unwrap_or(false);
        let client = if no_proxy {
            http::client_builder().no_proxy().build().map_err(|error| {
                ComponentError::new(format!("Failed to build HTML client: {error}"))
            })?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(HtmlFileResolver { client, selector, attr, direct }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(ItemFileResolver)]
struct HtmlFileResolver {
    client: reqwest::Client,
    selector: Selector,
    attr: String,
    direct: bool,
}

impl Display for HtmlFileResolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "html")
    }
}

#[async_trait]
impl ItemFileResolver for HtmlFileResolver {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let page_url =
            reqwest::Url::parse(&item.download_uri.to_string()).map_err(|e| {
                ProcessingError::non_retryable(format!("Invalid HTML page URL: {e}"))
            })?;
        let response = http::execute(
            &self.client,
            self.client.get(page_url.clone()),
            "Fetch HTML page",
        )
        .await?;
        let html =
            response.text().await.map_err(|e| http::map_error(e, "Read HTML page"))?;
        let targets = {
            let doc = Html::parse_document(&html);
            doc.select(&self.selector)
                .filter_map(|e| e.value().attr(&self.attr))
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let mut files = Vec::with_capacity(targets.len());
        for (index, target) in targets.into_iter().enumerate() {
            let uri = page_url.join(&target).map_err(|e| {
                ProcessingError::non_retryable(format!(
                    "Invalid extracted URL '{target}': {e}"
                ))
            })?;
            let filename = uri
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| s.contains('.'))
                .map(str::to_string)
                .unwrap_or_else(|| format!("{}_{}.html", item.hashing(), index));
            let mut file = SourceFile::new(PathBuf::from(filename));
            if self.direct {
                let response = http::execute(
                    &self.client,
                    self.client.get(uri),
                    "Fetch extracted HTML file",
                )
                .await?;
                file.data = Some(Arc::from(
                    response
                        .bytes()
                        .await
                        .map_err(|e| http::map_error(e, "Read extracted file"))?
                        .as_ref(),
                ));
            } else {
                file.download_uri = Some(uri.as_str().parse().map_err(|e| {
                    ProcessingError::non_retryable(format!("Invalid extracted URI: {e}"))
                })?);
            }
            files.push(file)
        }
        Ok(files)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, serde_json, time::OffsetDateTime};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    fn item(url: &str) -> SourceItem {
        SourceItem {
            title: "page".into(),
            link: Uri::from_static("https://example.com"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: "text/html".into(),
            download_uri: url.parse().unwrap(),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        }
    }
    #[tokio::test]
    async fn resolves_absolute_and_relative_links() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page/index.html"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<a class='file' href='../a.jpg'></a><a class='file' href='/asset'></a>",
            ))
            .mount(&s)
            .await;
        let p = Map::from_iter([
            ("css-selector".into(), Value::String("a.file".into())),
            ("extract-attribute".into(), Value::String("href".into())),
            ("no-proxy".into(), Value::Bool(true)),
        ]);
        let r = SUPPLIER.apply(&p).unwrap().as_item_file_resolver().unwrap();
        let f = r
            .resolve_files(&item(&format!("{}/page/index.html", s.uri())))
            .await
            .unwrap();
        assert_eq!(PathBuf::from("a.jpg"), f[0].path);
        assert_eq!(
            format!("{}/a.jpg", s.uri()),
            f[0].download_uri.as_ref().unwrap().to_string()
        );
        assert!(f[1].path.to_string_lossy().ends_with("_1.html"));
    }
    #[tokio::test]
    async fn direct_mode_fetches_bytes_with_same_client() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/page"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string("<img src='/image.png'>"),
            )
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/image.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"PNG"))
            .mount(&s)
            .await;
        let p = Map::from_iter([
            ("css-selector".into(), Value::String("img".into())),
            ("extract-attribute".into(), Value::String("src".into())),
            ("direct-mode".into(), Value::Bool(true)),
            ("no-proxy".into(), Value::Bool(true)),
        ]);
        let r = SUPPLIER.apply(&p).unwrap().as_item_file_resolver().unwrap();
        let f = r.resolve_files(&item(&format!("{}/page", s.uri()))).await.unwrap();
        assert_eq!(Some(&b"PNG"[..]), f[0].data.as_deref());
        assert!(f[0].download_uri.is_none());
    }
    #[test]
    fn validates_selector_and_required_props() {
        assert!(SUPPLIER.apply(&Map::new()).is_err());
        let p = Map::from_iter([
            ("css-selector".into(), Value::String("[".into())),
            ("extract-attribute".into(), Value::String("href".into())),
        ]);
        assert!(SUPPLIER.apply(&p).is_err());
        let _ = serde_json::json!({});
    }
}

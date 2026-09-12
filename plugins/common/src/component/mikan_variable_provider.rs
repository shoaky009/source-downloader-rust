use super::cache::new_cache;
use crate::api::bangumi::BangumiClient;
use crate::http::HttpClient;
use moka::sync::Cache;
use regex::Regex;
use scraper::{Html, Selector};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{self, Map, Value, json};
use std::collections::HashMap;
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, LazyLock};

static SEASON: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)S(\d{1,2})|Season\s*(\d{1,2})|第([一二三四五六七八九十]+|\d+)[季期]")
        .unwrap()
});

pub struct MikanVariableProviderSupplier;
pub const SUPPLIER: MikanVariableProviderSupplier = MikanVariableProviderSupplier;

impl ComponentSupplier for MikanVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("mikan".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let mikan_base = prop(props, "mikan-base-url", "https://mikanani.me")?;
        let bangumi_base = prop(props, "bgmtv-base-url", "https://api.bgm.tv")?;
        let token = optional_string(props, "token")?;
        let bangumi_token = optional_string(props, "bgmtv-token")?;
        let http = HttpClient::new()?;
        Ok(Arc::new(MikanVariableProvider {
            bangumi: BangumiClient::new(http.clone(), bangumi_base, bangumi_token),
            http,
            mikan_base,
            token,
            cache: new_cache(),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Resolves anime variables using Mikanani and Bangumi.".into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "mikan-base-url":{"type":"string","default":"https://mikanani.me"},
                    "bgmtv-base-url":{"type":"string","default":"https://api.bgm.tv"},
                    "token":{"type":"string"},
                    "bgmtv-token":{"type":"string"}
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

fn optional_string(
    props: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ComponentError> {
    props
        .get(key)
        .map(|value| {
            serde_json::from_value::<String>(value.clone()).map_err(|error| {
                ComponentError::new(format!("Invalid configuration at '{key}': {error}"))
            })
        })
        .transpose()
}
fn prop(
    props: &Map<String, Value>,
    key: &str,
    default: &str,
) -> Result<String, ComponentError> {
    Ok(optional_string(props, key)?
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| default.to_string()))
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
struct MikanVariableProvider {
    http: HttpClient,
    bangumi: BangumiClient,
    mikan_base: String,
    token: Option<String>,
    cache: Cache<String, PatternVariables>,
}

impl Display for MikanVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "mikan")
    }
}

impl MikanVariableProvider {
    fn mikan_request(&self, url: &str) -> reqwest::RequestBuilder {
        let url = if url.starts_with("http://") || url.starts_with("https://") {
            url.to_string()
        } else {
            format!("{}{}", self.mikan_base, url)
        };
        let request = self.http.get(url);
        match &self.token {
            Some(token) => request
                .header("Cookie", format!(".AspNetCore.Identity.Application={token}")),
            None => request,
        }
    }

    async fn load(&self, item: &SourceItem) -> Result<PatternVariables, ProcessingError> {
        let key = item.link.to_string();
        if let Some(value) = self.cache.get(&key) {
            return Ok(value);
        }
        let variables = self.fetch_variables(item).await?;
        self.cache.insert(key, variables.clone());
        Ok(variables)
    }

    async fn fetch_variables(
        &self,
        item: &SourceItem,
    ) -> Result<PatternVariables, source_downloader_sdk::component::ProcessingError> {
        let Some((mikan_title, href)) = self
            .find_in_response(
                self.mikan_request(&item.link.to_string()),
                "Fetch Mikan episode",
                find_mikan_title,
            )
            .await?
        else {
            return Ok(HashMap::new());
        };
        let Some(subject_id) = self
            .find_in_response(
                self.mikan_request(&href),
                "Fetch Mikan bangumi",
                find_subject_id,
            )
            .await?
        else {
            return Ok(HashMap::new());
        };
        let subject = self.bangumi.get_subject(&subject_id).await?;
        let name_cn = if subject.name_cn.trim().is_empty() {
            subject.name.clone()
        } else {
            subject.name_cn
        };
        let season = parse_season(&item.title)
            .or_else(|| parse_season(&subject.name))
            .or_else(|| parse_season(&name_cn))
            .unwrap_or(1);
        let mut variables = HashMap::from([
            ("name".to_string(), subject.name),
            ("nameCn".to_string(), name_cn),
            ("season".to_string(), format!("{season:02}")),
        ]);
        if let Some(title) = mikan_title {
            variables.insert("mikanTitle".to_string(), title);
        }
        if let Some(date) = subject.date {
            variables.insert("date".to_string(), date.clone());
            if let Some((year, month)) = date.split_once('-') {
                variables.insert("year".to_string(), year.to_string());
                variables.insert("month".to_string(), month.to_string());
            }
        }
        Ok(variables)
    }
    async fn find_in_response<T>(
        &self,
        request: reqwest::RequestBuilder,
        operation: &str,
        find: impl Fn(&str) -> Option<T>,
    ) -> Result<Option<T>, ProcessingError> {
        let mut response = self.http.send(request, operation).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            crate::http::map_error(error, &format!("Read {operation} response"))
        })? {
            body.extend_from_slice(&chunk);
            if let Some(value) = find(&String::from_utf8_lossy(&body)) {
                return Ok(Some(value));
            }
        }
        Ok(find(&String::from_utf8_lossy(&body)))
    }
}

#[async_trait]
impl VariableProvider for MikanVariableProvider {
    fn accuracy(&self) -> i32 {
        3
    }
    async fn item_variables(
        &self,
        item: &SourceItem,
    ) -> Result<PatternVariables, ProcessingError> {
        if item.link.host().is_none_or(|host| !host.contains("mikan"))
            && !self.mikan_base.contains(item.link.host().unwrap_or_default())
        {
            return Ok(HashMap::new());
        }
        self.load(item).await
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        item_variables: &PatternVariables,
        files: &[SourceFile],
    ) -> Result<Vec<PatternVariables>, ProcessingError> {
        Ok(files
            .iter()
            .map(|_| {
                item_variables
                    .get("season")
                    .map(|season| HashMap::from([("season".to_string(), season.clone())]))
                    .unwrap_or_default()
            })
            .collect())
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        _: &str,
    ) -> Result<Option<HashMap<String, Value>>, ProcessingError> {
        Ok(None)
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("name".to_string())
    }
}
fn parse_season(value: &str) -> Option<u32> {
    let captures = SEASON.captures(value)?;
    let value =
        (1..=3).find_map(|index| captures.get(index).map(|value| value.as_str()))?;
    value.parse().ok().or(match value {
        "一" => Some(1),
        "二" => Some(2),
        "三" => Some(3),
        "四" => Some(4),
        "五" => Some(5),
        "六" => Some(6),
        "七" => Some(7),
        "八" => Some(8),
        "九" => Some(9),
        "十" => Some(10),
        _ => None,
    })
}

fn class_fragment<'a>(html: &'a str, class: &str, end: &str) -> Option<&'a str> {
    let class_start = html.find(class)?;
    let element_start = html[..class_start].rfind('<')?;
    let element_end = html[class_start..].find(end)? + class_start + end.len();
    Some(&html[element_start..element_end])
}

fn find_mikan_title(html: &str) -> Option<(Option<String>, String)> {
    let fragment = class_fragment(html, "bangumi-title", "</a>")?;
    let document = Html::parse_fragment(fragment);
    let selector = Selector::parse("a").unwrap();
    let title = document.select(&selector).next()?;
    let href = title.value().attr("href")?.to_string();
    let text = title.text().collect::<String>();
    let text = text.trim();
    Some(((!text.is_empty()).then(|| text.to_string()), href))
}

fn find_subject_id(html: &str) -> Option<String> {
    let class_start = html.find("bangumi-info")?;
    let subject_start =
        html[class_start..].find("/subject/")? + class_start + "/subject/".len();
    let id = html[subject_start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!id.is_empty()).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::client_builder;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};

    async fn delayed_response(prefix: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                .await
                .unwrap();
            stream
                .write_all(format!("{:X}\r\n{prefix}\r\n", prefix.len()).as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
            let _ = stream.write_all(b"4\r\ntail\r\n0\r\n\r\n").await;
        });
        format!("http://{address}")
    }

    fn provider() -> MikanVariableProvider {
        let http = HttpClient::from_reqwest(client_builder().no_proxy().build().unwrap());
        MikanVariableProvider {
            bangumi: BangumiClient::new(http.clone(), "http://unused".into(), None),
            http,
            mikan_base: "http://unused".into(),
            token: None,
            cache: new_cache(),
        }
    }

    #[tokio::test]
    async fn returns_subject_before_stream_finishes() {
        let url = delayed_response(
            r#"<p class="bangumi-info">Bangumi番组计划链接：<br/><a class="w-other-c" target="_blank" href="https://bgm.tv/subject/530725">https://bgm.tv/subject/530725</a></p>"#,
        )
        .await;
        let provider = provider();

        let subject_id = timeout(
            Duration::from_millis(500),
            provider.find_in_response(
                provider.mikan_request(&url),
                "Fetch delayed Mikan bangumi",
                find_subject_id,
            ),
        )
        .await
        .expect("target should be returned without waiting for the response tail")
        .unwrap();

        assert_eq!(subject_id.as_deref(), Some("530725"));
    }

    #[test]
    fn parses_season() {
        assert_eq!(Some(2), parse_season("Show S02"));
        assert_eq!(Some(3), parse_season("动画 第三季"));
    }

    #[test]
    fn supplier_defaults() {
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_ok()
        );
    }
}

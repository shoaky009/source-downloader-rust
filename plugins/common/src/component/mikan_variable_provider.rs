use crate::api::bangumi::BangumiClient;
use crate::http::{self, HttpClient};
use parking_lot::Mutex;
use regex::Regex;
use scraper::{Html, Selector};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::{HashMap, VecDeque};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, LazyLock};

static TITLE_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".bangumi-title a").unwrap());
static SUBJECT_SELECTOR: LazyLock<Selector> =
    LazyLock::new(|| Selector::parse(".bangumi-info a").unwrap());
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
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let mikan_base = prop(props, "mikan-base-url", "https://mikanani.me")?;
        let bangumi_base = prop(props, "bgmtv-base-url", "https://api.bgm.tv")?;
        let token = props
            .get("token")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ComponentError::new("Invalid 'token' property"))
            })
            .transpose()?;
        let bangumi_token = props
            .get("bgmtv-token")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ComponentError::new("Invalid 'bgmtv-token' property"))
            })
            .transpose()?;
        let http = if mikan_base.starts_with("http://127.0.0.1:")
            || bangumi_base.starts_with("http://127.0.0.1:")
        {
            HttpClient::from_reqwest(
                http::client_builder()
                    .no_proxy()
                    .build()
                    .map_err(|error| ComponentError::new(error.to_string()))?,
            )
        } else {
            HttpClient::new()?
        };
        Ok(Arc::new(MikanVariableProvider {
            bangumi: BangumiClient::new(http.clone(), bangumi_base, bangumi_token),
            http,
            mikan_base,
            token,
            cache: Mutex::new(Cache::default()),
        }))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

fn prop(
    props: &Map<String, Value>,
    key: &str,
    default: &str,
) -> Result<String, ComponentError> {
    props
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(|value| value.trim_end_matches('/').to_string())
                .ok_or_else(|| ComponentError::new(format!("Invalid '{key}' property")))
        })
        .transpose()
        .map(|value| value.unwrap_or_else(|| default.to_string()))
}

#[derive(Debug, Default)]
struct Cache {
    values: HashMap<String, PatternVariables>,
    order: VecDeque<String>,
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
struct MikanVariableProvider {
    http: HttpClient,
    bangumi: BangumiClient,
    mikan_base: String,
    token: Option<String>,
    cache: Mutex<Cache>,
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

    async fn load(&self, item: &SourceItem) -> PatternVariables {
        let key = item.link.to_string();
        if let Some(value) = self.cache.lock().values.get(&key).cloned() {
            return value;
        }
        let variables = self.fetch_variables(item).await.unwrap_or_else(|error| {
            tracing::warn!(error = %error, link = %item.link, "Mikan metadata failed");
            HashMap::new()
        });
        let mut cache = self.cache.lock();
        if cache.values.len() == 500
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.values.remove(&oldest);
        }
        cache.order.push_back(key.clone());
        cache.values.insert(key, variables.clone());
        variables
    }

    async fn fetch_variables(
        &self,
        item: &SourceItem,
    ) -> Result<PatternVariables, source_downloader_sdk::component::ProcessingError> {
        let episode = self
            .http
            .text(self.mikan_request(&item.link.to_string()), "Fetch Mikan episode")
            .await?;
        let (mikan_title, href) = {
            let document = Html::parse_document(&episode);
            let title = document.select(&TITLE_SELECTOR).next();
            let mikan_title = title
                .as_ref()
                .map(|element| element.text().collect::<String>().trim().to_string());
            let href = title
                .and_then(|element| element.value().attr("href"))
                .map(str::to_string);
            (mikan_title, href)
        };
        let Some(href) = href else {
            return Ok(HashMap::new());
        };
        let bangumi_page =
            self.http.text(self.mikan_request(&href), "Fetch Mikan bangumi").await?;
        let subject_id = {
            let page = Html::parse_document(&bangumi_page);
            page.select(&SUBJECT_SELECTOR)
                .flat_map(|element| element.text())
                .find_map(|text| text.split("/subject/").nth(1).map(str::trim))
                .filter(|id| !id.is_empty())
                .map(str::to_string)
        };
        let Some(subject_id) = subject_id else {
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
}

#[async_trait]
impl VariableProvider for MikanVariableProvider {
    fn accuracy(&self) -> i32 {
        3
    }

    async fn item_variables(&self, item: &SourceItem) -> PatternVariables {
        if item.link.host().is_none_or(|host| !host.contains("mikan"))
            && !self.mikan_base.contains(item.link.host().unwrap_or_default())
        {
            return HashMap::new();
        }
        self.load(item).await
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        item_variables: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        files
            .iter()
            .map(|_| {
                item_variables
                    .get("season")
                    .map(|season| HashMap::from([("season".to_string(), season.clone())]))
                    .unwrap_or_default()
            })
            .collect()
    }

    async fn extract_from(
        &self,
        _: &SourceItem,
        _: &str,
    ) -> Option<HashMap<String, Value>> {
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_season() {
        assert_eq!(Some(2), parse_season("Show S02"));
        assert_eq!(Some(3), parse_season("动画 第三季"));
    }

    #[test]
    fn supplier_defaults() {
        assert!(SUPPLIER.apply(&Map::new()).is_ok());
    }
}

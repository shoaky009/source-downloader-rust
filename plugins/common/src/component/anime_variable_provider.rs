use crate::http;
use parking_lot::Mutex;
use regex::Regex;
use serde::Deserialize;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::collections::{HashMap, VecDeque};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, LazyLock};

static CLEANERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"\d{2}-\d{2}|全\d+[话話]", ""),
        (r"\+(?:OVA|OAD)", ""),
        (r"[【（(]", "["),
        (r"[】）)]", "]"),
        (r"[。，～]", " "),
        (r"[~！～+]", ""),
        (r"(?i)Special|\bSP\b|\bTV\b|\bS0?1\b|Season 0?1|BDBOX|BD-BOX", ""),
    ]
    .into_iter()
    .map(|(pattern, replacement)| (Regex::new(pattern).unwrap(), replacement))
    .collect()
});
static BRACKETS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[.*?\]").unwrap());
static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s{2,}").unwrap());
static TRAILING_DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+$").unwrap());

pub struct AnimeVariableProviderSupplier;
pub const SUPPLIER: AnimeVariableProviderSupplier = AnimeVariableProviderSupplier;

impl ComponentSupplier for AnimeVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("anime".to_string())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let anilist_url =
            string_prop(props, "anilist-base-url", "https://graphql.anilist.co")?;
        let bangumi_url = string_prop(props, "bgmtv-base-url", "https://api.bgm.tv")?;
        let token = props
            .get("bgmtv-token")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| ComponentError::new("Invalid 'bgmtv-token' property"))
            })
            .transpose()?;
        let prefer_bangumi = props
            .get("prefer-bgm-tv")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    ComponentError::new("Invalid 'prefer-bgm-tv' property")
                })
            })
            .transpose()?
            .unwrap_or(false);
        let client = if anilist_url.starts_with("http://127.0.0.1:")
            || bangumi_url.starts_with("http://127.0.0.1:")
        {
            http::client_builder()
                .no_proxy()
                .build()
                .map_err(|error| ComponentError::new(error.to_string()))?
        } else {
            http::build_client()?
        };
        Ok(Arc::new(AnimeVariableProvider {
            client,
            anilist_url,
            bangumi_url,
            token,
            prefer_bangumi,
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

fn string_prop(
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

#[derive(Default)]
struct Cache {
    values: HashMap<String, PatternVariables>,
    order: VecDeque<String>,
}

struct AnimeVariableProvider {
    client: reqwest::Client,
    anilist_url: String,
    bangumi_url: String,
    token: Option<String>,
    prefer_bangumi: bool,
    cache: Mutex<Cache>,
}

impl Debug for AnimeVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimeVariableProvider")
            .field("prefer_bangumi", &self.prefer_bangumi)
            .finish()
    }
}

impl Display for AnimeVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anime")
    }
}

impl SdComponent for AnimeVariableProvider {
    fn as_variable_provider(
        self: Arc<Self>,
    ) -> Result<Arc<dyn VariableProvider>, ComponentError> {
        Ok(self)
    }
}

#[derive(Deserialize)]
struct AniListResponse {
    data: Option<AniListData>,
    #[serde(default)]
    errors: Vec<Value>,
}
#[derive(Deserialize)]
struct AniListData {
    #[serde(rename = "Page")]
    page: AniListPage,
}
#[derive(Deserialize)]
struct AniListPage {
    #[serde(default)]
    media: Vec<AniListMedia>,
}
#[derive(Deserialize)]
struct AniListMedia {
    title: AniListTitle,
}
#[derive(Clone, Deserialize)]
struct AniListTitle {
    romaji: Option<String>,
    native: Option<String>,
}
#[derive(Deserialize)]
struct BangumiResponse {
    #[serde(default)]
    data: Vec<BangumiSubject>,
}
#[derive(Deserialize)]
struct BangumiSubject {
    name: String,
    #[serde(default)]
    name_cn: String,
}

impl AnimeVariableProvider {
    async fn variables(&self, raw_title: &str) -> PatternVariables {
        let title = extract_title(raw_title);
        if title.is_empty() {
            return HashMap::new();
        }
        if let Some(value) = self.cache.lock().values.get(&title).cloned() {
            return value;
        }
        let variables = self.search(&title).await;
        let mut cache = self.cache.lock();
        if cache.values.len() == 500
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.values.remove(&oldest);
        }
        cache.order.push_back(title.clone());
        cache.values.insert(title, variables.clone());
        variables
    }

    async fn search(&self, title: &str) -> PatternVariables {
        let japanese = title.chars().any(is_kana);
        let chinese = title.chars().any(is_han);
        let mut anilist = if japanese || !chinese {
            self.search_anilist(&reformat_anilist(title)).await
        } else {
            None
        };
        if !self.prefer_bangumi && anilist.is_some() {
            return anime_variables(anilist.as_ref(), None);
        }
        let keyword = anilist
            .as_ref()
            .and_then(|title| title.native.as_deref())
            .unwrap_or(title)
            .replace('-', "");
        let bangumi = self.search_bangumi(&keyword).await;
        if anilist.is_none()
            && let Some(subject) = &bangumi
        {
            anilist = self.search_anilist(&subject.name).await;
        }
        anime_variables(anilist.as_ref(), bangumi.as_ref())
    }

    async fn search_anilist(&self, title: &str) -> Option<AniListTitle> {
        let request = self.client.post(&self.anilist_url).json(&json!({
            "query": "query ($search: String) { Page(page: 1, perPage: 10) { media(search: $search, type: ANIME) { title { romaji native } } } }",
            "variables": { "search": title }
        }));
        let response =
            http::execute(&self.client, request, "Search AniList").await.ok()?;
        let body = response.json::<AniListResponse>().await.ok()?;
        if !body.errors.is_empty() {
            return None;
        }
        body.data?.page.media.into_iter().next().map(|media| media.title)
    }

    async fn search_bangumi(&self, title: &str) -> Option<BangumiSubject> {
        let mut request = self
            .client
            .post(format!("{}/v0/search/subjects", self.bangumi_url))
            .json(&json!({"keyword": title, "filter": {"type": [2], "nsfw": true}}));
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response =
            http::execute(&self.client, request, "Search Bangumi anime").await.ok()?;
        response.json::<BangumiResponse>().await.ok()?.data.into_iter().next()
    }
}

fn anime_variables(
    anilist: Option<&AniListTitle>,
    bangumi: Option<&BangumiSubject>,
) -> PatternVariables {
    let mut variables = HashMap::new();
    if let Some(romaji) = anilist.and_then(|title| title.romaji.as_ref()) {
        variables.insert("romajiName".to_string(), romaji.clone());
    }
    let native = bangumi
        .map(|subject| subject.name.as_str())
        .or_else(|| anilist.and_then(|title| title.native.as_deref()));
    if let Some(native) = native {
        variables.insert("nativeName".to_string(), native.to_string());
    }
    variables
}

#[async_trait]
impl VariableProvider for AnimeVariableProvider {
    async fn item_variables(&self, item: &SourceItem) -> PatternVariables {
        self.variables(&item.title).await
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        let mut variables = Vec::with_capacity(files.len());
        for file in files {
            let title = if file.path.is_absolute() {
                None
            } else {
                file.path
                    .components()
                    .nth(1)
                    .map(|component| component.as_os_str())
                    .filter(|name| {
                        name.len() >= 10 && file.path.file_name() != Some(*name)
                    })
                    .map(|name| name.to_string_lossy())
            };
            variables.push(match title {
                Some(title) => self.variables(&title).await,
                None => HashMap::new(),
            });
        }
        variables
    }

    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        Some(
            self.variables(value)
                .await
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        )
    }

    fn primary_variable_name(&self) -> Option<String> {
        Some("nativeName".to_string())
    }
}

fn extract_title(raw: &str) -> String {
    let mut text = raw.to_string();
    for (pattern, replacement) in CLEANERS.iter() {
        text = pattern.replace_all(&text, *replacement).into_owned();
    }
    text = Regex::new(r"S(\d+)").unwrap().replace_all(&text, "Season $1").into_owned();
    let without_brackets = BRACKETS.replace_all(&text, "").trim().to_string();
    if without_brackets.len() > 12 {
        if let Some(separator) =
            ['/', '|'].into_iter().find(|ch| without_brackets.contains(*ch))
        {
            return without_brackets
                .split(separator)
                .max_by_key(|title| language_score(title))
                .unwrap_or(&without_brackets)
                .trim()
                .to_string();
        }
        if let Some(space) = MULTI_SPACE.find(&without_brackets) {
            return without_brackets[..space.start()].trim().to_string();
        }
        return without_brackets;
    }
    if !without_brackets.is_empty() {
        return without_brackets;
    }
    let brackets = BRACKETS
        .find_iter(&text)
        .map(|value| value.as_str().trim_matches(['[', ']']).to_string())
        .collect::<Vec<_>>();
    brackets.get(1).or_else(|| brackets.first()).cloned().unwrap_or(text)
}

fn language_score(title: &str) -> usize {
    title.chars().map(|ch| if is_kana(ch) { 10 } else { 1 }).sum()
}
fn is_han(ch: char) -> bool {
    matches!(ch as u32, 0x3400..=0x9fff | 0xf900..=0xfaff)
}
fn is_kana(ch: char) -> bool {
    matches!(ch as u32, 0x3040..=0x30ff | 0x31f0..=0x31ff)
}
fn reformat_anilist(title: &str) -> String {
    let Some(digits) = TRAILING_DIGITS.find(title) else {
        return title.to_string();
    };
    if digits.start() > 0 && title.as_bytes()[digits.start() - 1] != b' ' {
        let mut value = title.to_string();
        value.insert(digits.start(), ' ');
        value
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_search_title() {
        assert_eq!("葬送のフリーレン", extract_title("[Group] 葬送のフリーレン [1080P]"));
        assert_eq!("アニメ", extract_title("English Title / アニメ"));
        assert_eq!("Anime 2", reformat_anilist("Anime2"));
    }

    #[test]
    fn defaults_without_properties() {
        assert!(SUPPLIER.apply(&Map::new()).is_ok());
    }
}

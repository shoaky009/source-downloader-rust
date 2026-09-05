use crate::api::anilist::{AniListClient, AniListTitle};
use crate::api::bangumi::{BangumiClient, BangumiSubject};
use crate::http::HttpClient;
use parking_lot::Mutex;
use regex::Regex;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{self, Map, Value, json};
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
static BRACKETS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[.*?]").unwrap());
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
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let anilist_url =
            string_prop(props, "anilist-base-url", "https://graphql.anilist.co")?;
        let bangumi_url = string_prop(props, "bgmtv-base-url", "https://api.bgm.tv")?;
        let token = props
            .get("bgmtv-token")
            .map(|value| {
                serde_json::from_value::<String>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'bgmtv-token': {error}"
                    ))
                })
            })
            .transpose()?;
        let prefer_bangumi = props
            .get("prefer-bgm-tv")
            .map(|value| {
                serde_json::from_value::<bool>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'prefer-bgm-tv': {error}"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(false);
        let http = HttpClient::new()?;
        Ok(Arc::new(AnimeVariableProvider {
            anilist: AniListClient::new(http.clone(), anilist_url),
            bangumi: BangumiClient::new(http, bangumi_url, token),
            prefer_bangumi,
            cache: Mutex::new(Cache::default()),
        }))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Resolves anime title variables using AniList and Bangumi."
                .into(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "anilist-base-url":{"type":"string","default":"https://graphql.anilist.co"},
                    "bgmtv-base-url":{"type":"string","default":"https://api.bgm.tv"},
                    "bgmtv-token":{"type":"string"},
                    "prefer-bgm-tv":{"type":"boolean","default":false}
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
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
            serde_json::from_value::<String>(value.clone())
                .map(|value| value.trim_end_matches('/').to_string())
                .map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at '{key}': {error}"
                    ))
                })
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
struct AnimeVariableProvider {
    anilist: AniListClient,
    bangumi: BangumiClient,
    prefer_bangumi: bool,
    cache: Mutex<Cache>,
}

impl Display for AnimeVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anime")
    }
}

impl AnimeVariableProvider {
    async fn variables(
        &self,
        raw_title: &str,
    ) -> Result<PatternVariables, ProcessingError> {
        let title = extract_title(raw_title);
        if title.is_empty() {
            return Ok(HashMap::new());
        }
        if let Some(value) = self.cache.lock().values.get(&title).cloned() {
            return Ok(value);
        }
        let variables = self.search(&title).await?;
        let mut cache = self.cache.lock();
        if cache.values.len() == 500
            && let Some(oldest) = cache.order.pop_front()
        {
            cache.values.remove(&oldest);
        }
        cache.order.push_back(title.clone());
        cache.values.insert(title, variables.clone());
        Ok(variables)
    }

    async fn search(&self, title: &str) -> Result<PatternVariables, ProcessingError> {
        let japanese = title.chars().any(is_kana);
        let chinese = title.chars().any(is_han);
        let mut anilist = if japanese || !chinese {
            self.search_anilist(&reformat_anilist(title)).await?
        } else {
            None
        };
        if !self.prefer_bangumi && anilist.is_some() {
            return Ok(anime_variables(anilist.as_ref(), None));
        }
        let keyword = anilist
            .as_ref()
            .and_then(|title| title.native.as_deref())
            .unwrap_or(title)
            .replace('-', "");
        let bangumi = self.search_bangumi(&keyword).await?;
        if anilist.is_none()
            && let Some(subject) = &bangumi
        {
            anilist = self.search_anilist(&subject.name).await?;
        }
        Ok(anime_variables(anilist.as_ref(), bangumi.as_ref()))
    }

    async fn search_anilist(
        &self,
        title: &str,
    ) -> Result<Option<AniListTitle>, ProcessingError> {
        self.anilist.search(title).await
    }
    async fn search_bangumi(
        &self,
        title: &str,
    ) -> Result<Option<BangumiSubject>, ProcessingError> {
        Ok(self.bangumi.search_subjects(title).await?.into_iter().next())
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
    async fn item_variables(
        &self,
        item: &SourceItem,
    ) -> Result<PatternVariables, ProcessingError> {
        self.variables(&item.title).await
    }
    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Result<Vec<PatternVariables>, ProcessingError> {
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
                Some(title) => self.variables(&title).await?,
                None => HashMap::new(),
            });
        }
        Ok(variables)
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Result<Option<HashMap<String, Value>>, ProcessingError> {
        Ok(Some(
            self.variables(value)
                .await?
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        ))
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
    use crate::http;

    #[test]
    fn extracts_search_title() {
        assert_eq!("葬送のフリーレン", extract_title("[Group] 葬送のフリーレン [1080P]"));
        assert_eq!("アニメ", extract_title("English Title / アニメ"));
        assert_eq!("Anime 2", reformat_anilist("Anime2"));
        assert_eq!(
            "Kanpekisugite Kawaige ga Nai to Konyaku Haki sareta Seijo wa Ringoku ni Urareru",
            extract_title(
                "[Nekomoe kissaten&VCB-Studio] Kanpekisugite Kawaige ga Nai to Konyaku Haki sareta Seijo wa Ringoku ni Urareru [Ma10p_1080p]"
            )
        );
        assert_eq!(
            "Zenshuu  -",
            extract_title("[Moozzi2] Zenshuu [ x265-10Bit Ver. ] - TV + SP")
        );
    }

    #[tokio::test]
    async fn falls_back_to_bangumi_for_long_romaji_title() {
        use wiremock::matchers::{body_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const TITLE: &str = "Kanpekisugite Kawaige ga Nai to Konyaku Haki sareta Seijo wa Ringoku ni Urareru";
        const NATIVE_NAME: &str =
            "完璧すぎて可愛げがないと婚約破棄された聖女は隣国に売られる";

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(json!({
                "query": "query ($search: String) { Page(page: 1, perPage: 10) { media(search: $search, type: ANIME) { title { romaji native } } } }",
                "variables": {"search": TITLE}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"Page": {"media": []}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v0/search/subjects"))
            .and(header("user-agent", crate::api::bangumi::BANGUMI_USER_AGENT))
            .and(body_json(json!({
                "keyword": TITLE,
                "filter": {"type": [2], "nsfw": true}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"name": NATIVE_NAME, "name_cn": "", "date": "2025-04-09"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(wiremock::matchers::body_partial_json(
                json!({"variables": {"search": NATIVE_NAME}}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"data": {"Page": {"media": []}}})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let http =
            HttpClient::from_reqwest(http::client_builder().no_proxy().build().unwrap());
        let provider = AnimeVariableProvider {
            anilist: AniListClient::new(
                http.clone(),
                format!("{}/graphql", server.uri()),
            ),
            bangumi: BangumiClient::new(http, server.uri(), None),
            prefer_bangumi: false,
            cache: Mutex::new(Cache::default()),
        };
        let raw_title = format!("[Nekomoe kissaten&VCB-Studio] {TITLE} [Ma10p_1080p]");

        assert_eq!(
            Some(NATIVE_NAME),
            provider
                .variables(&raw_title)
                .await
                .unwrap()
                .get("nativeName")
                .map(String::as_str)
        );
    }

    #[test]
    fn defaults_without_properties() {
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

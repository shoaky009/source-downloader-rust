use regex::Regex;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct AnimeTitleVariableProviderSupplier;

pub const SUPPLIER: AnimeTitleVariableProviderSupplier =
    AnimeTitleVariableProviderSupplier;

impl ComponentSupplier for AnimeTitleVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("anime-title".to_string())]
    }

    fn apply(
        &self,
        _props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(AnimeTitleVariableProvider))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(VariableProvider)]
struct AnimeTitleVariableProvider;

impl Display for AnimeTitleVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anime-title")
    }
}

static CLEANERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"（僅限港澳台）", ""),
        (r"(?i)10-bit|8-bit|1080p|720p|HEVC|BDRip|AV1|OPUS|AVC", ""),
        (r"(?i)(GB|BIG5).?MP4|\d+X\d+|\d\.0|\d+-\d+", ""),
        (r"[(【（]", "["),
        (r"[)】）]", "]"),
        (r"(?i)\d+月新番|\[\d+]|\[END]|\[\d*v\d+]|★.*?★", ""),
        (r"\[[^]]*(简|繁|招募|翻译)[^]]*]", ""),
        (r"\[]", ""),
        (r"(?i)\|\s*$", ""),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (Regex::new(pattern).expect("static regex must compile"), replacement)
    })
    .collect()
});
static BRACKET_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[.*?]").expect("static regex must compile"));
static BRACKET_CAPTURE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[(.*?)]").expect("static regex must compile"));
static ANI_EPISODE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r" - \d+(\.\d+)? \[.*]").expect("static regex must compile")
});

#[derive(Clone, Copy)]
enum Extractor {
    AniTitle,
    Separator(&'static str),
    AllBrackets,
    Default,
}

const DEFAULT_CHAIN: &[Extractor] = &[
    Extractor::AniTitle,
    Extractor::Separator(" / "),
    Extractor::Separator(" | "),
    Extractor::Separator("\\"),
];
const FALLBACK_CHAIN: &[Extractor] = &[
    Extractor::Separator("/"),
    Extractor::Separator("|"),
    Extractor::AllBrackets,
    Extractor::Default,
];

#[async_trait]
impl VariableProvider for AnimeTitleVariableProvider {
    async fn item_variables(&self, item: &SourceItem) -> HashMap<String, String> {
        extract_titles(&clean_title(&item.title)).unwrap_or_default()
    }

    async fn file_variables(
        &self,
        _item: &SourceItem,
        _item_variables: &PatternVariables,
        _files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        vec![]
    }

    async fn extract_from(
        &self,
        _item: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        let variables = extract_titles(&clean_title(value))?;
        Some(
            variables
                .into_iter()
                .map(|(key, value)| (key, Value::String(value)))
                .collect(),
        )
    }

    fn primary_variable_name(&self) -> Option<String> {
        Some("title".to_string())
    }
}

fn clean_title(title: &str) -> String {
    CLEANERS
        .iter()
        .fold(title.to_string(), |value, (regex, replacement)| {
            regex.replace_all(&value, *replacement).into_owned()
        })
        .trim()
        .to_string()
}

fn extract_titles(raw: &str) -> Option<PatternVariables> {
    run_chain(raw, DEFAULT_CHAIN, true).or_else(|| run_chain(raw, FALLBACK_CHAIN, false))
}

fn run_chain(
    raw: &str,
    extractors: &[Extractor],
    allow_fallback: bool,
) -> Option<PatternVariables> {
    for extractor in extractors {
        let Some(titles) = extractor.extract(raw) else {
            continue;
        };
        if titles.is_empty() {
            continue;
        }
        let mut processed = Vec::with_capacity(titles.len());
        for title in titles {
            let title = BRACKET_REGEX.replace_all(title.trim(), "").trim().to_string();
            if title.is_empty() && allow_fallback {
                return run_chain(raw, FALLBACK_CHAIN, false);
            }
            if !title.contains("字幕组") {
                processed.push(title);
            }
        }
        if processed.len() == 1 {
            return Some(HashMap::from([("title".to_string(), processed.remove(0))]));
        }
        if let Some(romaji_index) = processed.iter().position(|title| title.is_ascii()) {
            let romaji_title = processed[romaji_index].clone();
            let title = processed
                .iter()
                .find(|title| **title != romaji_title)
                .unwrap_or(&romaji_title)
                .clone();
            return Some(HashMap::from([
                ("title".to_string(), title),
                ("romajiTitle".to_string(), romaji_title),
            ]));
        }
    }
    None
}

impl Extractor {
    fn extract<'a>(&self, raw: &'a str) -> Option<Vec<&'a str>> {
        match self {
            Extractor::AniTitle => extract_ani_title(raw),
            Extractor::Separator(separator) => extract_separator(raw, separator),
            Extractor::AllBrackets => {
                if !BRACKET_REGEX.replace_all(raw, "").trim().is_empty() {
                    return None;
                }
                Some(
                    BRACKET_CAPTURE_REGEX
                        .captures_iter(raw)
                        .filter_map(|capture| capture.get(1).map(|value| value.as_str()))
                        .collect(),
                )
            }
            Extractor::Default => Some(vec![raw]),
        }
    }
}

fn extract_ani_title(raw: &str) -> Option<Vec<&str>> {
    let lower = raw.to_lowercase();
    let group_index = lower.find("[ani]")?;
    let start = group_index + "[ANi]".len();
    let end = ANI_EPISODE_REGEX.find(raw)?.start();
    let title = raw.get(start..end)?;
    let split: Vec<_> = title.split(" - ").collect();
    if split.len() > 1 {
        return Some(split);
    }
    Some(title.split(" / ").collect())
}

fn extract_separator<'a>(raw: &'a str, separator: &str) -> Option<Vec<&'a str>> {
    let outside = BRACKET_REGEX.replace_all(raw, "");
    let target = if outside.trim().is_empty() {
        BRACKET_CAPTURE_REGEX
            .captures_iter(raw)
            .filter_map(|capture| capture.get(1).map(|value| value.as_str()))
            .filter(|value| value.contains(separator))
            .max_by_key(|value| value.len())?
    } else {
        raw
    };
    let split: Vec<_> = target.split(separator).collect();
    (split.len() > 1).then_some(split)
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};

    fn item(title: &str) -> SourceItem {
        SourceItem {
            title: title.to_string(),
            link: Uri::from_static("https://example.com"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: String::new(),
            download_uri: Uri::from_static("https://example.com/file"),
            attrs: Map::new(),
            tags: vec![],
            identity: None,
        }
    }

    #[test]
    fn supplier_supports_implicit_construction() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::variable_provider("anime-title".to_string())]
        );
        assert!(SUPPLIER.is_support_no_props());
        assert!(SUPPLIER.apply(&Map::new()).is_ok());
    }

    #[tokio::test]
    async fn extracts_ani_bilingual_title() {
        let variables = AnimeTitleVariableProvider
            .item_variables(&item(
                "[ANi] 葬送的芙莉莲 / Sousou no Frieren - 01 [1080P][CHT]",
            ))
            .await;
        assert_eq!(Some("葬送的芙莉莲"), variables.get("title").map(String::as_str));
        assert_eq!(
            Some("Sousou no Frieren"),
            variables.get("romajiTitle").map(String::as_str)
        );
    }

    #[tokio::test]
    async fn extracts_separator_and_bracket_fallback_titles() {
        let variables = AnimeTitleVariableProvider
            .item_variables(&item("葬送的芙莉莲 | Sousou no Frieren [1080p]"))
            .await;
        assert_eq!(Some("葬送的芙莉莲"), variables.get("title").map(String::as_str));
        assert_eq!(
            Some("Sousou no Frieren"),
            variables.get("romajiTitle").map(String::as_str)
        );

        let variables = AnimeTitleVariableProvider
            .item_variables(&item("[字幕组][葬送的芙莉莲][Sousou no Frieren]"))
            .await;
        assert_eq!(Some("葬送的芙莉莲"), variables.get("title").map(String::as_str));
        assert_eq!(
            Some("Sousou no Frieren"),
            variables.get("romajiTitle").map(String::as_str)
        );
    }

    #[tokio::test]
    async fn cleans_noise_and_handles_single_title() {
        let variables = AnimeTitleVariableProvider
            .item_variables(&item("★01月新番★ 葬送的芙莉莲 [1080p][简日双语][01]"))
            .await;
        assert_eq!(
            HashMap::from([("title".to_string(), "葬送的芙莉莲".to_string())]),
            variables
        );
    }

    #[tokio::test]
    async fn extract_from_uses_the_same_chain() {
        let variables = AnimeTitleVariableProvider
            .extract_from(&item("unused"), "Frieren / 葬送的芙莉莲")
            .await
            .unwrap();
        assert_eq!(
            Some(&Value::String("葬送的芙莉莲".to_string())),
            variables.get("title")
        );
        assert_eq!(
            Some(&Value::String("Frieren".to_string())),
            variables.get("romajiTitle")
        );
    }
}

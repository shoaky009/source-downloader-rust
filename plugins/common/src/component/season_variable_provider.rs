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

pub struct SeasonVariableProviderSupplier;
pub const SUPPLIER: SeasonVariableProviderSupplier = SeasonVariableProviderSupplier;

impl ComponentSupplier for SeasonVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("season".to_string())]
    }
    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(SeasonVariableProvider))
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
struct SeasonVariableProvider;
impl Display for SeasonVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "season")
    }
}

static SP_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)OVA|OAD|SPs|S00|SP\s*\d+|Special|extra\d+|特别篇|特別篇|\[SP]|映像特典",
    )
    .expect("static regex must compile")
});
static GENERAL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)S(\d{1,2})|Season\s*(\d{1,2})|第([一二三四五六七八九十]{1,3}|\d+)[季期]|(\d+)(?:rd|nd)").expect("static regex must compile")
});
static LAST_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\d|二|三|四|五|六|七|八|九|十|II|III|IV|V|VI|VII|VIII|IX|X|Ⅱ|Ⅲ|Ⅳ|[２-９])$").expect("static regex must compile")
});
static FRACTION_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+[/／]\d+$").expect("static regex must compile"));

#[async_trait]
impl VariableProvider for SeasonVariableProvider {
    fn accuracy(&self) -> i32 {
        2
    }
    async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
        HashMap::new()
    }
    async fn file_variables(
        &self,
        item: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        files
            .iter()
            .map(|file| {
                let filepath = file.path.to_string_lossy();
                let season = parse_season(&filepath)
                    .or_else(|| parse_season(&item.title))
                    .unwrap_or(1);
                season_variables(season)
            })
            .collect()
    }
    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        let season = parse_season(value).unwrap_or(1);
        Some(HashMap::from([(
            "season".to_string(),
            Value::String(format!("{season:02}")),
        )]))
    }
    fn primary_variable_name(&self) -> Option<String> {
        Some("season".to_string())
    }
}

fn season_variables(season: u32) -> PatternVariables {
    HashMap::from([("season".to_string(), format!("{season:02}"))])
}

fn parse_season(value: &str) -> Option<u32> {
    let value = value.trim();
    if SP_REGEX.is_match(value) {
        return Some(0);
    }
    if let Some(captures) = GENERAL_REGEX.captures(value) {
        if let Some(raw) =
            (1..=4).find_map(|index| captures.get(index).map(|capture| capture.as_str()))
        {
            return parse_season_number(raw);
        }
    }
    for (keyword, season) in [
        (" II ", 2),
        (" III ", 3),
        (" IV ", 4),
        (" V ", 5),
        (" VI ", 6),
        (" VII ", 7),
        (" VIII ", 8),
        (" IX ", 9),
        (" X ", 10),
        (" Ⅱ ", 2),
        (" Ⅲ ", 3),
        (" Ⅳ ", 4),
    ] {
        if value.contains(keyword) {
            return Some(season);
        }
    }
    if FRACTION_REGEX.is_match(value) {
        return None;
    }
    let captures = LAST_REGEX.captures(value)?;
    let found = captures.get(1)?;
    if found.start() > 0 {
        let previous = value[..found.start()].chars().next_back()?;
        if previous.is_ascii_alphabetic()
            && !value[..found.start()].to_lowercase().ends_with("season")
        {
            return None;
        }
    }
    parse_season_number(found.as_str())
}

fn parse_season_number(value: &str) -> Option<u32> {
    value.parse().ok().or_else(|| match value {
        "一" => Some(1),
        "二" | "II" | "Ⅱ" | "２" => Some(2),
        "三" | "III" | "Ⅲ" | "３" => Some(3),
        "四" | "IV" | "Ⅳ" | "４" => Some(4),
        "五" | "V" | "５" => Some(5),
        "六" | "VI" | "６" => Some(6),
        "七" | "VII" | "７" => Some(7),
        "八" | "VIII" | "８" => Some(8),
        "九" | "IX" | "９" => Some(9),
        "十" | "X" => Some(10),
        "十一" => Some(11),
        "十二" => Some(12),
        "十三" => Some(13),
        "十四" => Some(14),
        "十五" => Some(15),
        "十六" => Some(16),
        "十七" => Some(17),
        "十八" => Some(18),
        "十九" => Some(19),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
    use std::path::PathBuf;
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
    fn supplier_contract() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::variable_provider("season".to_string())]
        );
        assert!(SUPPLIER.is_support_no_props());
        assert!(SUPPLIER.apply(&Map::new()).is_ok());
    }
    #[test]
    fn parses_chain_and_default() {
        for (value, expected) in [
            ("Show SP01", 0),
            ("Show S03", 3),
            ("Show Season 4", 4),
            ("动画 第三季", 3),
            ("Show III", 3),
            ("Show Ⅱ", 2),
        ] {
            assert_eq!(Some(expected), parse_season(value), "value={value}");
        }
        assert_eq!(None, parse_season("Show 1/2"));
    }
    #[tokio::test]
    async fn file_path_precedes_title_and_values_are_padded() {
        let files = vec![
            SourceFile::new(PathBuf::from("Show S02/01.mkv")),
            SourceFile::new(PathBuf::from("Show/01.mkv")),
        ];
        let variables = SeasonVariableProvider
            .file_variables(&item("Show Season 3"), &HashMap::new(), &files)
            .await;
        assert_eq!(Some("02"), variables[0].get("season").map(String::as_str));
        assert_eq!(Some("03"), variables[1].get("season").map(String::as_str));
    }
    #[tokio::test]
    async fn extract_from_defaults_to_first_season() {
        let variables = SeasonVariableProvider
            .extract_from(&item(""), "Unmarked Show")
            .await
            .unwrap();
        assert_eq!(Some(&Value::String("01".to_string())), variables.get("season"));
        assert_eq!(2, SeasonVariableProvider.accuracy());
    }
}

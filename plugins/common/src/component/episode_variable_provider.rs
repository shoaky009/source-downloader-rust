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

pub struct EpisodeVariableProviderSupplier;
pub const SUPPLIER: EpisodeVariableProviderSupplier = EpisodeVariableProviderSupplier;

impl ComponentSupplier for EpisodeVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("episode".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(EpisodeVariableProvider))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Extracts episode variables from filenames.".into(),
            props_json_schema: None,
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, SdComponent)]
#[component(VariableProvider)]
struct EpisodeVariableProvider;

impl Display for EpisodeVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "episode")
    }
}

static CLEANERS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"_", " "),
        (r"(?i)(?:480|720|1080|2160)P", ""),
        (r"(?i)1280x720|1920x1080|3840x2160", ""),
        (r"(?i)x(?:264|265)", ""),
        (r"(?i)flacx2|ma10p|hi10p|yuv420p10|10bit|hevc10|aacx2|flac|4k", ""),
        (r"(?i)\b[A-Fa-f0-9]{8}\b|CRC32.*[0-9A-F]{8}", ""),
        (r"(?i)v\d+|\d{5,}", ""),
        (r"(?i)FIN", ""),
        (r"(?i)1st|2nd|3rd|[4-9]th", ""),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (Regex::new(pattern).expect("static regex must compile"), replacement)
    })
    .collect()
});
static PARSERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"第(\d+(?:\.\d)?)[话話集巻]",
        r"(\d+(?:\.\d)?)[話话]",
        r"(?i)(?:E|EP|Episode ?)(\d+)",
        r"(?i)S\d+E(\d+)",
        r"SP(\d+)",
        r"^\D*?(\d{1,3})\D*?$",
        r"#(\d+)",
        r"\[(\d{2})\(\d{2}\)]",
        r"(?i)(\d{2}(?:\.\d)?)?.?(?:oad|ova)",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("static regex must compile"))
    .collect()
});
static RANGE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)-(\d+)").expect("static regex must compile"));
static WORD_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"第([一二三四五六七八九十])[话話集巻]",
        r"([一二三四五六七八九十])[话話]",
        r"其\w([壱弐参肆伍陸漆捌玖拾])",
    ]
    .into_iter()
    .map(|pattern| Regex::new(pattern).expect("static regex must compile"))
    .collect()
});
static NUMBER_TOKEN_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^\d.])(\[?\d+(?:\.\d+)?]?)(?:$|[\s\[\]()（）【】])")
        .expect("static regex must compile")
});

#[async_trait]
impl VariableProvider for EpisodeVariableProvider {
    fn accuracy(&self) -> i32 {
        3
    }

    async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
        HashMap::new()
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        let mut result: Vec<_> = files
            .iter()
            .map(|file| {
                let stem =
                    file.path.file_stem().and_then(|value| value.to_str()).unwrap_or("");
                parse_episode(&clean(stem))
                    .map(|episode| {
                        HashMap::from([("episode".to_string(), pad(&episode, 2))])
                    })
                    .unwrap_or_default()
            })
            .collect();
        let parsed =
            result.iter().filter(|variables| variables.contains_key("episode")).count();
        if parsed > 100 {
            let width = parsed.to_string().len();
            for variables in &mut result {
                if let Some(episode) = variables.get_mut("episode") {
                    *episode = pad(episode, width);
                }
            }
        }
        result
    }

    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        let episode = parse_episode(value)?;
        Some(HashMap::from([("episode".to_string(), Value::String(pad(&episode, 2)))]))
    }

    fn primary_variable_name(&self) -> Option<String> {
        Some("episode".to_string())
    }
}

fn clean(value: &str) -> String {
    CLEANERS.iter().fold(value.to_string(), |current, (regex, replacement)| {
        regex.replace_all(&current, *replacement).into_owned()
    })
}

fn parse_episode(value: &str) -> Option<String> {
    for (index, regex) in PARSERS.iter().enumerate().take(5) {
        if let Some(found) = capture(regex, value) {
            if index == 2 && follows_digit_or_dash(regex, value) {
                continue;
            }
            return Some(normalize_number(found));
        }
    }
    if let Some(word) = parse_word(value) {
        return Some(word.to_string());
    }
    if let Some(found) = capture(&PARSERS[5], value) {
        return Some(normalize_number(found));
    }
    if let Some(found) = capture(&PARSERS[6], value) {
        return Some(normalize_number(found));
    }
    if let Some(range) = parse_range(value) {
        return Some(range);
    }
    if let Some(found) = capture(&PARSERS[7], value) {
        return Some(normalize_number(found));
    }
    if let Some(found) = value
        .split('[')
        .skip(1)
        .filter_map(|segment| segment.split_once(']'))
        .filter_map(|(content, _)| bracket_episode(content))
        .next()
    {
        return Some(normalize_number(found));
    }
    if let Some(common) = parse_common(value) {
        return Some(common);
    }
    for regex in PARSERS.iter().skip(8) {
        if let Some(found) = capture(regex, value) {
            return Some(normalize_number(found));
        }
    }
    None
}

fn bracket_episode(content: &str) -> Option<&str> {
    let digit_count = content.bytes().take_while(u8::is_ascii_digit).count();
    if !(2..=3).contains(&digit_count)
        || content[digit_count..].bytes().any(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(&content[..digit_count])
}

fn capture<'a>(regex: &Regex, value: &'a str) -> Option<&'a str> {
    regex
        .captures(value)?
        .get(1)
        .map(|capture| capture.as_str())
        .filter(|value| !value.is_empty())
}

fn follows_digit_or_dash(regex: &Regex, value: &str) -> bool {
    regex
        .find(value)
        .and_then(|found| value.get(found.end()..))
        .and_then(|rest| rest.chars().next())
        .is_some_and(|character| character.is_ascii_digit() || character == '-')
}

fn parse_word(value: &str) -> Option<u8> {
    const WORDS: &str = "一二三四五六七八九十壱弐参肆伍陸漆捌玖拾";
    let matched = WORD_REGEXES.iter().find_map(|regex| capture(regex, value))?;
    let index = WORDS.chars().position(|character| matched.starts_with(character))?;
    Some((index % 10 + 1) as u8)
}

fn parse_range(value: &str) -> Option<String> {
    let captures = RANGE_REGEX.captures(value)?;
    let begin = captures.get(1)?.as_str();
    let end = captures.get(2)?.as_str();
    (begin < end).then(|| format!("{begin}-{end}"))
}

static NON_NUMERIC_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\[[^\[\]]*[^\d\[\]][^\[\]]*]").expect("static regex must compile")
});

fn parse_common(value: &str) -> Option<String> {
    let value = NON_NUMERIC_BRACKET.replace_all(value, "");
    let mut best: Option<(i32, String)> = None;
    for captures in NUMBER_TOKEN_REGEX.captures_iter(&value) {
        let raw = captures.get(1)?.as_str();
        let bracketed = raw.starts_with('[') && raw.ends_with(']');
        let number = raw.trim_matches(['[', ']']);
        if number.len() <= 1 || number.parse::<f64>().is_err() {
            continue;
        }
        let mut score = i32::from(bracketed) * 2;
        if ["480", "720", "1080", "2160"].contains(&number) {
            score -= 1;
        }
        score -= match number.len() {
            3 => 2,
            4 => 3,
            5.. => i32::MAX,
            _ => 0,
        };
        if best.as_ref().is_none_or(|(best_score, _)| score > *best_score) {
            best = Some((score, normalize_number(number)));
        }
    }
    best.map(|(_, number)| number)
}

fn normalize_number(value: &str) -> String {
    if value.contains('.') {
        value
            .parse::<f32>()
            .map(|number| number.to_string())
            .unwrap_or_else(|_| value.to_string())
    } else {
        value
            .parse::<u32>()
            .map(|number| number.to_string())
            .unwrap_or_else(|_| value.to_string())
    }
}

fn pad(value: &str, width: usize) -> String {
    if let Some((integer, fraction)) = value.split_once('.') {
        format!("{integer:0>width$}.{fraction}")
    } else if let Some((begin, end)) = value.split_once('-') {
        format!("{begin:0>width$}-{end}")
    } else {
        format!("{value:0>width$}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
    use std::path::PathBuf;

    fn item() -> SourceItem {
        SourceItem {
            title: String::new(),
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
            vec![ComponentType::variable_provider("episode".to_string())]
        );
        assert!(SUPPLIER.is_support_no_props());
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_ok()
        );
    }

    #[tokio::test]
    async fn should_all_expected() {
        let item = item();
        for line in include_str!("../../test-resources/episode-test-data.csv")
            .lines()
            .filter(|line| !line.trim().is_empty())
        {
            let (expected, path) = line.split_once(',').unwrap();
            let shared_variables = EpisodeVariableProvider.item_variables(&item).await;
            let variables = EpisodeVariableProvider
                .file_variables(
                    &item,
                    &shared_variables,
                    &[SourceFile::new(PathBuf::from(path))],
                )
                .await;
            assert_eq!(
                expected,
                variables[0].get("episode").map(String::as_str).unwrap_or(""),
                "path={path}"
            );
        }
    }

    #[tokio::test]
    async fn test_episode_padding_by_files_length() {
        let files = (1..=150)
            .map(|episode| SourceFile::new(PathBuf::from(episode.to_string())))
            .collect::<Vec<_>>();
        let variables = EpisodeVariableProvider
            .file_variables(&item(), &HashMap::new(), &files)
            .await;
        assert_eq!(Some("001"), variables[0].get("episode").map(String::as_str));
    }

    #[tokio::test]
    async fn extract_from_returns_padded_value() {
        let variables =
            EpisodeVariableProvider.extract_from(&item(), "第3集").await.unwrap();
        assert_eq!(Some(&Value::String("03".to_string())), variables.get("episode"));
        assert_eq!(3, EpisodeVariableProvider.accuracy());
        assert_eq!(
            Some("episode".to_string()),
            EpisodeVariableProvider.primary_variable_name()
        );
    }
}

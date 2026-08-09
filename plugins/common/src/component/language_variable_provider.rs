use lingua::{Language, LanguageDetector, LanguageDetectorBuilder};
use regex::Regex;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{self, Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct LanguageVariableProviderSupplier;
pub const SUPPLIER: LanguageVariableProviderSupplier = LanguageVariableProviderSupplier;

impl ComponentSupplier for LanguageVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("language".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let read_content = props
            .get("read-content")
            .map(|value| {
                serde_json::from_value::<bool>(value.clone()).map_err(|error| {
                    ComponentError::new(format!(
                        "Invalid configuration at 'read-content': {error}"
                    ))
                })
            })
            .transpose()?
            .unwrap_or(true);
        Ok(Arc::new(LanguageVariableProvider { read_content }))
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
struct LanguageVariableProvider {
    read_content: bool,
}

impl Display for LanguageVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "language")
    }
}

const LANGUAGE_MARKERS: &[(&[&str], &[&str])] = &[
    (&["JPSC", "JPCHS", "CHSJP", "SCJP"], &["zh-CHS", "JP"]),
    (&["JPTC", "JPCHT", "CHTJP", "TCJP"], &["zh-CHT", "JP"]),
    (
        &["CHS", "SC", "GB", "GBK", "GB2312", "CN", "ZHCN", "ZHCHS", "简体", "简中"],
        &["zh-CHS"],
    ),
    (&["CHT", "TC", "BIG5", "TW", "HK", "ZHTW", "ZHCHT", "繁体", "繁中"], &["zh-CHT"]),
    (&["JP", "JPN", "JA", "JAPANESE", "日语", "日文"], &["JP"]),
    (&["EN", "ENG", "ENGLISH", "英语", "英文"], &["EN"]),
    (&["KR", "KOR", "KO", "KOREAN", "韩语", "韓語", "韩文", "韓文"], &["KR"]),
    (&["FR", "FRE", "FRA", "FRENCH"], &["FR"]),
    (&["DE", "GER", "DEU", "GERMAN"], &["DE"]),
    (&["ES", "SPA", "SPANISH"], &["ES"]),
    (&["IT", "ITA", "ITALIAN"], &["IT"]),
    (&["PT", "POR", "PORTUGUESE"], &["PT"]),
    (&["RU", "RUS", "RUSSIAN"], &["RU"]),
    (&["NL", "DUT", "NLD", "DUTCH"], &["NL"]),
    (&["PL", "POL", "POLISH"], &["PL"]),
    (&["TR", "TUR", "TURKISH"], &["TR"]),
    (&["SV", "SWE", "SWEDISH"], &["SV"]),
    (&["TH", "THA", "THAI"], &["TH"]),
    (&["VI", "VIE", "VIETNAMESE"], &["VI"]),
    (&["ID", "IND", "INDONESIAN"], &["ID"]),
    (&["MS", "MSA", "MAY", "MALAY"], &["MS"]),
    (&["AR", "ARA", "ARABIC"], &["AR"]),
];
const LANGUAGE_ORDER: &[&str] = &[
    "zh-CHS", "zh-CHT", "JP", "EN", "KR", "FR", "DE", "ES", "IT", "PT", "RU", "NL", "PL",
    "TR", "SV", "TH", "VI", "ID", "MS", "AR",
];
static SRT_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+$").expect("static regex must compile"));
static SRT_TIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{2}:\d{2}:\d{2},\d{3}.*\d{2}:\d{2}:\d{2},\d{3}$")
        .expect("static regex must compile")
});
static DETECTOR: LazyLock<LanguageDetector> = LazyLock::new(|| {
    LanguageDetectorBuilder::from_languages(&[Language::Chinese, Language::Japanese])
        .build()
});
const COLLECT_LINES: usize = 10;

#[async_trait]
impl VariableProvider for LanguageVariableProvider {
    async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
        HashMap::new()
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        files
            .iter()
            .map(|file| {
                language_from_name(file)
                    .or_else(|| {
                        self.read_content.then(|| language_from_file(file)).flatten()
                    })
                    .map(|language| HashMap::from([("language".to_string(), language)]))
                    .unwrap_or_default()
            })
            .collect()
    }

    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        detect_language(value).map(|language| {
            HashMap::from([("language".to_string(), Value::String(language))])
        })
    }

    fn primary_variable_name(&self) -> Option<String> {
        Some("language".to_string())
    }
}

fn language_from_name(file: &SourceFile) -> Option<String> {
    let stem = file.path.file_stem()?.to_str()?.to_uppercase();
    let mut detected = [false; LANGUAGE_ORDER.len()];
    for marker in stem.split(|character: char| !character.is_alphanumeric()) {
        if marker.is_empty() {
            continue;
        }
        if let Some((_, languages)) =
            LANGUAGE_MARKERS.iter().find(|(aliases, _)| aliases.contains(&marker))
        {
            for language in *languages {
                if let Some(index) =
                    LANGUAGE_ORDER.iter().position(|candidate| candidate == language)
                {
                    detected[index] = true;
                }
            }
        }
    }
    let mut value = String::new();
    for (language, detected) in LANGUAGE_ORDER.iter().zip(detected) {
        if detected {
            if !value.is_empty() {
                value.push('.');
            }
            value.push_str(language);
        }
    }
    (!value.is_empty()).then_some(value)
}

fn language_from_file(file: &SourceFile) -> Option<String> {
    let extension = file.path.extension()?.to_str()?;
    if !["ass", "srt"].contains(&extension) {
        return None;
    }
    let bytes = std::fs::read(&file.path).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let lines: Vec<_> = text.lines().collect();
    let collected: Vec<_> = match extension {
        "ass" => lines
            .iter()
            .skip_while(|line| line.trim() != "[Events]")
            .filter(|line| line.starts_with("Dialogue"))
            .filter_map(|line| line.rsplit(',').next())
            .filter(|line| !line.trim().is_empty())
            .take(COLLECT_LINES)
            .collect(),
        "srt" => lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .filter(|line| !SRT_NUMBER.is_match(line) && !SRT_TIME.is_match(line))
            .take(COLLECT_LINES)
            .copied()
            .collect(),
        _ => vec![],
    };
    detect_language(&collected.join(","))
}

fn detect_language(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    if contains_traditional_marker(text) {
        return Some("zh-CHT".to_string());
    }
    if contains_simplified_marker(text) {
        return Some("zh-CHS".to_string());
    }
    match DETECTOR.detect_language_of(text)? {
        Language::Chinese => Some("zh-CHS".to_string()),
        _ => None,
    }
}

fn contains_traditional_marker(text: &str) -> bool {
    text.chars()
        .any(|character| "體臺灣與為這個們說來時後學國語麼還裡".contains(character))
}

fn contains_simplified_marker(text: &str) -> bool {
    text.chars().any(|character| "体台与为这个们说来时后学国语么还里".contains(character))
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
    fn supplier_defaults_and_validates_read_content() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::variable_provider("language".to_string())]
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
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::from_iter([(
                        "read-content".to_string(),
                        Value::String("yes".to_string()),
                    )]),
                )
                .is_err()
        );
    }

    #[tokio::test]
    async fn filename_rules_take_priority() {
        let files = [
            "Show.CHS.ass",
            "Show_tc.srt",
            "Show-JPSC.ass",
            "Show.JPTC.ass",
            "Show BIG5.ass",
        ]
        .into_iter()
        .map(|path| SourceFile::new(PathBuf::from(path)))
        .collect::<Vec<_>>();
        let provider = LanguageVariableProvider { read_content: false };
        let variables = provider.file_variables(&item(), &HashMap::new(), &files).await;
        assert_eq!(Some("zh-CHS"), variables[0].get("language").map(String::as_str));
        assert_eq!(Some("zh-CHT"), variables[1].get("language").map(String::as_str));
        assert_eq!(Some("zh-CHS.JP"), variables[2].get("language").map(String::as_str));
        assert_eq!(Some("zh-CHT.JP"), variables[3].get("language").map(String::as_str));
        assert_eq!(Some("zh-CHT"), variables[4].get("language").map(String::as_str));
    }

    #[test]
    fn recognizes_common_filename_language_markers() {
        let cases = [
            ("Show.JPSC.ass", "zh-CHS.JP"),
            ("Show.JPTC.ass", "zh-CHT.JP"),
            (
                r"Z:\anime-temp\[Nekomoe kissaten&VCB-Studio] Kanpekisugite Kawaige ga Nai to Konyaku Haki sareta Seijo wa Ringoku ni Urareru [Ma10p_1080p]\[Nekomoe kissaten&VCB-Studio] Kanpekiseijo [01][Ma10p_1080p][x265_flac].JPSC.ass",
                "zh-CHS.JP",
            ),
            (
                r"Z:\anime-temp\[Nekomoe kissaten&VCB-Studio] Kanpekisugite Kawaige ga Nai to Konyaku Haki sareta Seijo wa Ringoku ni Urareru [Ma10p_1080p]\[Nekomoe kissaten&VCB-Studio] Kanpekiseijo [01][Ma10p_1080p][x265_flac].JPTC.ass",
                "zh-CHT.JP",
            ),
            ("Show.CHS.JP.ass", "zh-CHS.JP"),
            ("Show.JPN.ass", "JP"),
            ("Show.ENG.ass", "EN"),
            ("Show.KOR.ass", "KR"),
            ("Show.FRE.ass", "FR"),
            ("Show.GER.ass", "DE"),
            ("Show.SPA.ass", "ES"),
            ("Show.ITA.ass", "IT"),
            ("Show.POR.ass", "PT"),
            ("Show.RUS.ass", "RU"),
            ("Show.DUT.ass", "NL"),
            ("Show.POL.ass", "PL"),
            ("Show.TUR.ass", "TR"),
            ("Show.SWE.ass", "SV"),
            ("Show.THA.ass", "TH"),
            ("Show.VIE.ass", "VI"),
            ("Show.IND.ass", "ID"),
            ("Show.ARA.ass", "AR"),
        ];

        for (name, expected) in cases {
            let file = SourceFile::new(PathBuf::from(name));
            assert_eq!(Some(expected), language_from_name(&file).as_deref(), "{name}");
        }
    }

    #[tokio::test]
    async fn reads_ass_and_srt_dialogue_content() {
        let dir = tempfile::tempdir().unwrap();
        let ass = dir.path().join("plain.ass");
        let srt = dir.path().join("plain.srt");
        std::fs::write(
            &ass,
            "[Events]\nDialogue: 0,0,0,Default,,0,0,0,,這個故事發生在臺灣\n",
        )
        .unwrap();
        std::fs::write(&srt, "1\n00:00:00,000 --> 00:00:02,000\n这个故事发生在中国\n")
            .unwrap();
        let files = vec![SourceFile::new(ass), SourceFile::new(srt)];
        let variables = LanguageVariableProvider { read_content: true }
            .file_variables(&item(), &HashMap::new(), &files)
            .await;
        assert_eq!(Some("zh-CHT"), variables[0].get("language").map(String::as_str));
        assert_eq!(Some("zh-CHS"), variables[1].get("language").map(String::as_str));
    }

    #[tokio::test]
    async fn safely_ignores_disabled_missing_binary_and_unsupported_content() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("binary.srt");
        std::fs::write(&binary, [0xff, 0xfe]).unwrap();
        let files = vec![
            SourceFile::new(binary),
            SourceFile::new(dir.path().join("missing.ass")),
            SourceFile::new(dir.path().join("plain.txt")),
        ];
        let variables = LanguageVariableProvider { read_content: true }
            .file_variables(&item(), &HashMap::new(), &files)
            .await;
        assert!(variables.iter().all(HashMap::is_empty));
        assert_eq!(
            Some("language".to_string()),
            LanguageVariableProvider { read_content: false }.primary_variable_name()
        );
    }
}

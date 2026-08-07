use async_trait::async_trait;
use parking_lot::RwLock;
use regex::RegexBuilder;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceItemFilter, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

pub struct KeywordIntegrationSupplier;
pub const SUPPLIER: KeywordIntegrationSupplier = KeywordIntegrationSupplier;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct KeywordIntegrationConfig {
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default, alias = "keywordFile")]
    keyword_file: Option<PathBuf>,
}

const DEFAULT_REGEX_PATTERN: &str = r"[()\[](@keyword)[()\]]";

impl ComponentSupplier for KeywordIntegrationSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![
            ComponentType::variable_provider("keyword".to_owned()),
            ComponentType::item_filter("keyword".to_owned()),
        ]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: KeywordIntegrationConfig =
            serde_json::from_value(Value::Object(props.clone())).map_err(|error| {
                ComponentError::new(format!("Invalid keyword config: {error}"))
            })?;
        Ok(Arc::new(KeywordIntegration::new(config.keywords, config.keyword_file)?))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider, SourceItemFilter)]
pub struct KeywordIntegration {
    words: Arc<RwLock<Vec<Word>>>,
    stop: Arc<AtomicBool>,
    watcher: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Word {
    value: String,
    match_title_mode: i32,
    alias: Option<String>,
}

impl Display for KeywordIntegration {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("keyword")
    }
}

impl KeywordIntegration {
    fn new(
        keywords: Vec<String>,
        keyword_file: Option<PathBuf>,
    ) -> Result<Self, ComponentError> {
        let initial_words = parse_keywords(&keywords, keyword_file.as_deref())?;
        let words = Arc::new(RwLock::new(initial_words));
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = keyword_file.as_ref().map(|path| {
            let words = Arc::clone(&words);
            let stop = Arc::clone(&stop);
            let keywords = keywords.clone();
            let path = path.clone();
            thread::spawn(move || watch_keyword_file(stop, words, keywords, path))
        });
        Ok(Self { words, stop, watcher })
    }

    fn match_word(&self, text: &str) -> Option<Word> {
        let words = self.words.read();
        words.iter().find_map(|word| {
            let matched = if word.match_title_mode == 1 {
                text.to_lowercase().contains(&word.value.to_lowercase())
            } else {
                let pattern = DEFAULT_REGEX_PATTERN.replace("@keyword", &word.value);
                RegexBuilder::new(&pattern)
                    .case_insensitive(true)
                    .build()
                    .is_ok_and(|regex| regex.is_match(text))
            };
            matched.then(|| word.clone())
        })
    }

    fn keyword_variables(&self, text: &str) -> PatternVariables {
        self.match_word(text)
            .map(|word| {
                let value = word.alias.unwrap_or(word.value);
                PatternVariables::from([(String::from("keyword"), value)])
            })
            .unwrap_or_default()
    }
}

#[async_trait]
impl VariableProvider for KeywordIntegration {
    async fn item_variables(&self, source_item: &SourceItem) -> PatternVariables {
        self.keyword_variables(&source_item.title)
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        source_files: &[source_downloader_sdk::component::SourceFile],
    ) -> Vec<PatternVariables> {
        vec![PatternVariables::default(); source_files.len()]
    }

    async fn extract_from(
        &self,
        _: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>> {
        let vars = self.keyword_variables(value);
        (!vars.is_empty()).then(|| {
            vars.into_iter().map(|(key, value)| (key, Value::String(value))).collect()
        })
    }

    fn primary_variable_name(&self) -> Option<String> {
        Some(String::from("keyword"))
    }
}

#[async_trait]
impl SourceItemFilter for KeywordIntegration {
    async fn filter(&self, item: &SourceItem) -> bool {
        self.match_word(&item.title).is_some()
    }
}

impl Drop for KeywordIntegration {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

fn parse_keywords(
    keywords: &[String],
    keyword_file: Option<&Path>,
) -> Result<Vec<Word>, ComponentError> {
    let mut values = keywords.to_vec();
    if let Some(path) = keyword_file {
        let content = std::fs::read_to_string(path).map_err(|error| {
            ComponentError::new(format!(
                "Failed to read keyword file '{}': {error}",
                path.display()
            ))
        })?;
        values.extend(content.lines().map(str::to_owned));
    }
    let mut words = Vec::new();
    for value in values {
        let mut parts = value.split('|');
        let Some(word) = parts.next() else {
            continue;
        };
        let mode = parts.next().and_then(|value| value.parse::<i32>().ok()).unwrap_or(0);
        let alias = parts.next().map(str::to_owned);
        let candidate = Word { value: word.to_owned(), match_title_mode: mode, alias };
        if !words.contains(&candidate) {
            words.push(candidate);
        }
    }
    Ok(words)
}

fn watch_keyword_file(
    stop: Arc<AtomicBool>,
    words: Arc<RwLock<Vec<Word>>>,
    keywords: Vec<String>,
    path: PathBuf,
) {
    let mut modified = file_modified_time(&path);
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(250));
        let current = file_modified_time(&path);
        if current != modified {
            modified = current;
            match parse_keywords(&keywords, Some(&path)) {
                Ok(next) => {
                    let mut current_words = words.write();
                    *current_words = next;
                    tracing::info!(path = %path.display(), "Reloaded keywords");
                }
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "Failed to reload keywords");
                }
            }
        }
    }
}

fn file_modified_time(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|metadata| metadata.modified()).ok()
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keywords_deduplicates_entries_and_preserves_aliases() {
        let values = vec![
            String::from("Foo|1|Alias"),
            String::from("Foo|1|Alias"),
            String::from("Bar"),
        ];

        let words = parse_keywords(&values, None).unwrap();

        assert_eq!(
            words,
            vec![
                Word {
                    value: String::from("Foo"),
                    match_title_mode: 1,
                    alias: Some(String::from("Alias")),
                },
                Word { value: String::from("Bar"), match_title_mode: 0, alias: None },
            ]
        );
    }

    #[test]
    fn keyword_variables_use_alias_for_case_insensitive_title_matches() {
        let integration =
            KeywordIntegration::new(vec![String::from("Foo|1|Alias")], None).unwrap();

        let variables = integration.keyword_variables("a title containing FOO");

        assert_eq!(variables.get("keyword").map(String::as_str), Some("Alias"));
    }

    #[test]
    fn regex_mode_matches_keyword_wrapped_in_parentheses() {
        let integration =
            KeywordIntegration::new(vec![String::from("Foo")], None).unwrap();

        assert!(integration.match_word("a title containing (foo)").is_some());
        assert!(integration.match_word("a title containing foo").is_none());
    }
}

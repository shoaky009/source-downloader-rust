use regex::Regex;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent,
    FileReplacementDecider, InProcessingItem, SdComponent, SdComponentMetadata,
    SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct AnimeReplacementDeciderSupplier;

pub const SUPPLIER: AnimeReplacementDeciderSupplier = AnimeReplacementDeciderSupplier;

impl ComponentSupplier for AnimeReplacementDeciderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_replacement_decider("anime".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(AnimeReplacementDecider))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Chooses whether an anime file should replace an existing file."
                .into(),
            props_json_schema: None,
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, SdComponent)]
#[component(FileReplacementDecider)]
struct AnimeReplacementDecider;

impl Display for AnimeReplacementDecider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anime")
    }
}

static VERSION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\[\d{0,4}v(\d+)]").expect("static regex must compile")
});

#[derive(Debug, PartialEq, Eq)]
struct Rating {
    bilibili: bool,
    prerelease: bool,
    version: Option<i32>,
}

impl Rating {
    fn from_title(title: &str) -> Self {
        let lower = title.to_lowercase();
        let bilibili = ["bilibili", "仅限港澳台地区", "仅限台湾地区", "b-global"]
            .iter()
            .any(|marker| lower.contains(marker));
        let prerelease = title.contains("偷跑") || title.contains("先行");
        let version = VERSION_REGEX
            .captures(title)
            .and_then(|captures| captures.get(1))
            .and_then(|value| value.as_str().parse().ok());
        Self { bilibili, prerelease, version }
    }

    fn score(&self) -> i32 {
        if self.prerelease {
            return -1;
        }
        self.version.unwrap_or_default() - i32::from(self.bilibili)
    }
}

impl FileReplacementDecider for AnimeReplacementDecider {
    fn should_replace(
        &self,
        source_item: &SourceItem,
        _current_file: &FileContent,
        before: Option<&InProcessingItem>,
        _existing_file: &SourceFile,
    ) -> bool {
        let current = Rating::from_title(&source_item.title);
        let Some(before) = before else {
            return current.score() > 0;
        };
        let previous = Rating::from_title(&before.source_item.title);
        if previous.prerelease && current.prerelease {
            return false;
        }
        if current.bilibili && !previous.bilibili {
            return false;
        }
        current.score() > previous.score()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplier_supports_implicit_construction() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::file_replacement_decider("anime".to_string())]
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

    #[test]
    fn parses_version_and_source_markers() {
        assert_eq!(
            Rating::from_title("Show [v2]"),
            Rating { bilibili: false, prerelease: false, version: Some(2) }
        );
        assert_eq!(
            Rating::from_title("Show [1080V3] B-Global"),
            Rating { bilibili: true, prerelease: false, version: Some(3) }
        );
        for title in ["BILIBILI Show", "仅限港澳台地区 Show", "仅限台湾地区 Show"]
        {
            assert!(Rating::from_title(title).bilibili);
        }
    }

    #[test]
    fn scores_versions_bilibili_and_prereleases() {
        assert_eq!(0, Rating::from_title("Show").score());
        assert_eq!(2, Rating::from_title("Show [v2]").score());
        assert_eq!(1, Rating::from_title("Bilibili Show [v2]").score());
        assert_eq!(-1, Rating::from_title("Show [v9] 偷跑").score());
        assert_eq!(-1, Rating::from_title("Show [v9] 先行").score());
    }

    #[test]
    fn applies_replacement_precedence() {
        assert!(!should_replace("Show", None));
        assert!(should_replace("Show [v2]", None));
        assert!(should_replace("Show [v2]", Some("Show [v1]")));
        assert!(!should_replace("Show [v1]", Some("Show [v2]")));
        assert!(!should_replace("Bilibili Show [v9]", Some("Show [v1]")));
        assert!(!should_replace("Show [v3] 偷跑", Some("Show [v2] 偷跑")));
        assert!(should_replace("Show [v2]", Some("Bilibili Show [v2]")));
    }

    fn should_replace(current_title: &str, previous_title: Option<&str>) -> bool {
        use source_downloader_sdk::component::{FileContentStatus, PatternVariables};
        use source_downloader_sdk::storage::ProcessingStatus;
        use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
        use std::path::PathBuf;
        use std::sync::OnceLock;

        fn item(title: &str) -> SourceItem {
            SourceItem {
                title: title.to_string(),
                link: Uri::from_static("https://example.com/item"),
                datetime: OffsetDateTime::UNIX_EPOCH,
                content_type: "video/x-matroska".to_string(),
                download_uri: Uri::from_static("https://example.com/file.mkv"),
                attrs: Map::new(),
                tags: vec![],
                identity: None,
            }
        }
        fn content() -> FileContent {
            FileContent {
                download_path: PathBuf::new(),
                file_download_path: PathBuf::new(),
                source_save_path: PathBuf::new(),
                pattern_variables: PatternVariables::new(),
                file_save_path_pattern: String::new(),
                filename_pattern: String::new(),
                tags: vec![],
                attrs: Map::new(),
                file_uri: None,
                target_save_path: PathBuf::new(),
                target_filename: String::new(),
                exist_target_path: None,
                errors: vec![],
                status: FileContentStatus::Undetected,
                target_path: OnceLock::new(),
                data: None,
                processed_variables: None,
            }
        }

        let current = item(current_title);
        let current_file = content();
        let existing = SourceFile::new(PathBuf::from("existing.mkv"));
        let previous = previous_title.map(item);
        let id = None;
        let identity = None;
        let variables = PatternVariables::new();
        let files = vec![];
        let rename_times = 0;
        let status = ProcessingStatus::Renamed;
        let before = previous.as_ref().map(|source_item| InProcessingItem {
            id: &id,
            processor_name: "test",
            item_hash: "hash",
            item_identity: &identity,
            source_item,
            item_variables: &variables,
            file_contents: &files,
            rename_times: &rename_times,
            status: &status,
            failure_reason: None,
        });
        AnimeReplacementDecider.should_replace(
            &current,
            &current_file,
            before.as_ref(),
            &existing,
        )
    }
}

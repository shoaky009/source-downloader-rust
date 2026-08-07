use regex::Regex;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    SourceFile, SourceFileFilter,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::{Arc, LazyLock};

pub struct AnimeFileFilterSupplier;

pub const SUPPLIER: AnimeFileFilterSupplier = AnimeFileFilterSupplier;

impl ComponentSupplier for AnimeFileFilterSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::source_file_filter("anime".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(AnimeFileFilter))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(SourceFileFilter)]
struct AnimeFileFilter;

impl Display for AnimeFileFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anime")
    }
}

const MUST_FILTER_DIR_NAMES: &[&str] = &[
    "ncop",
    "nced",
    "trailer",
    "menu",
    "pv",
    "cm",
    "cd",
    "cds",
    "scan",
    "scans",
    "ed",
    "op",
    "fonts",
    "audio commentary",
    "preview",
    "event",
    "lecture",
    "making",
    "teaser",
];
const SPECIAL_DIR_NAMES: &[&str] = &[
    "sps", "sp", "special", "ncop", "nced", "menu", "pv", "cm", "cd", "cds", "scan",
    "scans", "extra", "特典",
];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "webm", "avi", "flv", "mov", "wmv", "ts", "m2ts", "m4v", "rmvb", "mpg",
    "mpeg", "vob", "divx", "xvid", "3gp", "3g2", "asf", "ogm", "ogv", "rm", "ram", "swf",
    "f4v", "dat", "m2v", "m2p", "m2t", "mts", "mxf", "iso", "img", "bin", "cue", "nrg",
    "ccd", "sub", "idx",
];
const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "rar", "7z", "tar", "gz"];
const SUBTITLE_EXTENSIONS: &[&str] = &["ass", "srt", "ssa", "vtt"];

static SPECIAL_REGEXES: LazyLock<[Regex; 2]> = LazyLock::new(|| {
    [
        Regex::new(
            "(?i)NCOP|NCED|MENU|Fonts|Scan|Event|Lecture|Preview|特典|Other|Teaser",
        )
        .expect("static regex must compile"),
        Regex::new("PV|CM|IV|Info|INFO|OP|ED|Cast| Program | MV |Making")
            .expect("static regex must compile"),
    ]
});
static NORMAL_REGEXES: LazyLock<[Regex; 3]> = LazyLock::new(|| {
    [
        Regex::new("(?i)preview|fonts|nced|ncop|font|audio commentary|trailer")
            .expect("static regex must compile"),
        Regex::new("(?i)Info(\\d+)|ed(\\d+)|op(\\d+)|event(\\d+)")
            .expect("static regex must compile"),
        Regex::new(
            "(?i)\\b\\s+OP\\b|\\b\\s+ED\\b|\\s+MENU|\\s+PV|\\s+CM|\\s+Fonts|^MENU(\\d+)?$|^PV(\\d+)?$|映像特典|^MENU ",
        )
        .expect("static regex must compile"),
    ]
});
static CRC_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("(?i)\\b[A-Fa-f0-9]{8}\\b").expect("static regex must compile")
});
static SUBTITLE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)subtitle|字幕").expect("static regex must compile"));

impl SourceFileFilter for AnimeFileFilter {
    fn filter(&self, file: &SourceFile) -> bool {
        let Some(extension) = file.path.extension().and_then(|value| value.to_str())
        else {
            return false;
        };
        let extension = extension.to_lowercase();
        if !is_allowed_extension(&extension) {
            return false;
        }

        if ARCHIVE_EXTENSIONS.contains(&extension.as_str()) {
            return file
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| SUBTITLE_REGEX.is_match(name));
        }

        if is_in_named_directory(&file.path, MUST_FILTER_DIR_NAMES) {
            return false;
        }

        let regexes: &[Regex] = if is_in_named_directory(&file.path, SPECIAL_DIR_NAMES) {
            SPECIAL_REGEXES.as_slice()
        } else {
            NORMAL_REGEXES.as_slice()
        };
        let Some(stem) = file.path.file_stem().and_then(|value| value.to_str()) else {
            return false;
        };
        let normalized = stem.replace(['-', '_', '[', ']', '(', ')', '.'], " ");
        let normalized = CRC_REGEX.replace_all(&normalized, "");
        regexes.iter().all(|regex| !regex.is_match(&normalized))
    }
}

fn is_allowed_extension(extension: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&extension)
        || ARCHIVE_EXTENSIONS.contains(&extension)
        || SUBTITLE_EXTENSIONS.contains(&extension)
}

fn is_in_named_directory(path: &Path, names: &[&str]) -> bool {
    path.parent().is_some_and(|parent| {
        parent.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|name| names.contains(&name.to_lowercase().as_str()))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn accepts(path: &str) -> bool {
        AnimeFileFilter.filter(&SourceFile::new(PathBuf::from(path)))
    }

    #[test]
    fn supplier_supports_implicit_construction() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::source_file_filter("anime".to_string())]
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
    fn accepts_episode_video_and_subtitle_extensions_case_insensitively() {
        for path in [
            "Show/Show - 01.mkv",
            "Show/Show - 01.MP4",
            "Show/Show - 01.ass",
            "Show/Show - 01.VTT",
        ] {
            assert!(accepts(path), "expected {path} to be accepted");
        }
    }

    #[test]
    fn rejects_unsupported_and_extensionless_files() {
        for path in ["Show/readme.txt", "Show/episode", "Show/cover.jpg"] {
            assert!(!accepts(path), "expected {path} to be rejected");
        }
    }

    #[test]
    fn accepts_only_subtitle_archives() {
        assert!(accepts("Show/English Subtitles.zip"));
        assert!(accepts("Show/字幕.RAR"));
        assert!(!accepts("Show/Scans.zip"));
    }

    #[test]
    fn rejects_files_in_forced_directories_at_any_depth() {
        for path in [
            "Show/NCOP/Show.mkv",
            "Show/extras/Fonts/font.ass",
            "Show/Preview/nested/Show.mp4",
            "Show/SCAN/page.mkv",
        ] {
            assert!(!accepts(path), "expected {path} to be rejected");
        }
    }

    #[test]
    fn applies_special_and_normal_filename_rules() {
        for path in [
            "Show/Show NCOP.mkv",
            "Show/Show OP01.mkv",
            "Show/Show Preview.mkv",
            "Show/映像特典.mkv",
            "Show/Special/Other.mkv",
            "Show/Extra/Making.mkv",
        ] {
            assert!(!accepts(path), "expected {path} to be rejected");
        }
        assert!(accepts("Show/Special/Show 01.mkv"));
    }

    #[test]
    fn removes_crc_before_matching_without_hiding_episode_names() {
        assert!(accepts("Show/[A1B2C3D4] Show - 01.mkv"));
        assert!(!accepts("Show/[A1B2C3D4] Show NCED.mkv"));
    }
}

use source_downloader_sdk::SdComponent;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileExistsDetector,
    FileMover, SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct MediaTypeExistsDetectorSupplier;
pub const SUPPLIER: MediaTypeExistsDetectorSupplier = MediaTypeExistsDetectorSupplier;

impl ComponentSupplier for MediaTypeExistsDetectorSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_exists_detector("media-type".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(MediaTypeExistsDetector))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(FileExistsDetector)]
struct MediaTypeExistsDetector;

impl Display for MediaTypeExistsDetector {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "media-type")
    }
}

impl FileExistsDetector for MediaTypeExistsDetector {
    fn exists<'a>(
        &self,
        file_mover: &'a dyn FileMover,
        _: &'a SourceItem,
        file_contents: &'a [FileContent],
    ) -> HashMap<&'a PathBuf, Option<PathBuf>> {
        let mut directories: HashMap<&PathBuf, Vec<&FileContent>> = HashMap::new();
        for file in file_contents {
            directories.entry(&file.target_save_path).or_default().push(file);
        }

        let mut result = HashMap::with_capacity(file_contents.len());
        for (directory, targets) in directories {
            let existing = file_mover.list_files(directory).unwrap_or_default();
            let existing: Vec<_> = existing
                .iter()
                .map(|path| (top_level_media_type(path), path.file_stem(), path))
                .collect();
            for target in targets {
                let target_path = target.target_path();
                let target_type = top_level_media_type(target_path);
                let target_stem = target_path.file_stem();
                let matched = existing
                    .iter()
                    .find(|(media_type, stem, _)| {
                        *media_type == target_type && *stem == target_stem
                    })
                    .map(|(_, _, path)| (*path).clone());
                result.insert(target_path, matched);
            }
        }
        result
    }
}

fn top_level_media_type(path: &std::path::Path) -> String {
    infer::get_from_path(path)
        .ok()
        .flatten()
        .and_then(|kind| kind.mime_type().split_once('/').map(|(top_level, _)| top_level))
        .or_else(|| top_level_from_extension(path))
        .unwrap_or("application")
        .to_string()
}

fn top_level_from_extension(path: &std::path::Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "avif" | "heic" | "heif" | "tif"
        | "tiff" | "bmp" | "jxr" | "psd" | "ico" | "ora" | "djvu" => Some("image"),
        "mp4" | "m4v" | "mkv" | "webm" | "mov" | "avi" | "wmv" | "mpg" | "mpeg"
        | "flv" | "3gp" => Some("video"),
        "mid" | "midi" | "mp3" | "m4a" | "ogg" | "opus" | "flac" | "wav" | "amr"
        | "aac" | "aiff" | "dsf" | "ape" | "wma" => Some("audio"),
        "txt" | "css" | "csv" | "tsv" | "html" | "htm" | "md" | "nfo" | "srt" | "ass"
        | "ssa" | "vtt" => Some("text"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::component::{FileContentStatus, ProcessingError};
    use source_downloader_sdk::{http::Uri, time::OffsetDateTime};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    #[derive(Debug, SdComponent)]
    #[component(FileMover)]
    struct TestMover {
        files: HashMap<PathBuf, Vec<PathBuf>>,
    }
    impl Display for TestMover {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "test")
        }
    }
    impl FileMover for TestMover {
        fn list_files(&self, path: &Path) -> Result<Vec<PathBuf>, ProcessingError> {
            Ok(self.files.get(path).cloned().unwrap_or_default())
        }
    }

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
    fn content(directory: &Path, filename: &str) -> FileContent {
        FileContent {
            download_path: PathBuf::new(),
            file_download_path: PathBuf::new(),
            source_save_path: directory.to_path_buf(),
            pattern_variables: HashMap::new(),
            file_save_path_pattern: String::new(),
            filename_pattern: String::new(),
            tags: vec![],
            attrs: Map::new(),
            file_uri: None,
            target_save_path: directory.to_path_buf(),
            target_filename: filename.to_string(),
            exist_target_path: None,
            errors: vec![],
            status: FileContentStatus::Undetected,
            target_path: OnceLock::new(),
            data: None,
            processed_variables: None,
        }
    }

    #[test]
    fn supplier_contract() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::file_exists_detector("media-type".to_string())]
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
    fn matches_same_basename_and_top_level_media_type() {
        let dir = tempfile::tempdir().unwrap();
        let existing_video = dir.path().join("episode.mp4");
        let existing_image = dir.path().join("episode.png");
        std::fs::write(
            &existing_video,
            [0, 0, 0, 24, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm'],
        )
        .unwrap();
        std::fs::write(&existing_image, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])
            .unwrap();
        let targets =
            vec![content(dir.path(), "episode.mkv"), content(dir.path(), "other.mkv")];
        let mover = TestMover {
            files: HashMap::from([(
                dir.path().to_path_buf(),
                vec![existing_video.clone(), existing_image],
            )]),
        };
        let source_item = item();
        let result = MediaTypeExistsDetector.exists(&mover, &source_item, &targets);
        assert_eq!(Some(existing_video), result[targets[0].target_path()]);
        assert_eq!(None, result[targets[1].target_path()]);
    }

    #[test]
    fn keeps_directory_groups_independent() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let existing = first.path().join("episode.mp4");
        std::fs::write(
            &existing,
            [0, 0, 0, 24, b'f', b't', b'y', b'p', b'i', b's', b'o', b'm'],
        )
        .unwrap();
        let targets = vec![
            content(first.path(), "episode.mkv"),
            content(second.path(), "episode.mkv"),
        ];
        let mover = TestMover {
            files: HashMap::from([(first.path().to_path_buf(), vec![existing.clone()])]),
        };
        let source_item = item();
        let result = MediaTypeExistsDetector.exists(&mover, &source_item, &targets);
        assert_eq!(Some(existing), result[targets[0].target_path()]);
        assert_eq!(None, result[targets[1].target_path()]);
    }
}

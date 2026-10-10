use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileMover,
    ProcessingError, SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::Arc;

pub struct GeneralFileMoverSupplier;
pub const SUPPLIER: GeneralFileMoverSupplier = GeneralFileMoverSupplier;

impl ComponentSupplier for GeneralFileMoverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_mover("general".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(GeneralFileMover))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Moves files using the standard file move operation.".to_owned(),
            props_json_schema: None,
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(FileMover)]
pub struct GeneralFileMover;

impl Display for GeneralFileMover {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("general")
    }
}

#[source_downloader_sdk::async_trait::async_trait]
impl FileMover for GeneralFileMover {
    async fn move_file(
        &self,
        _: &SourceItem,
        file: &FileContent,
    ) -> Result<(), ProcessingError> {
        let source = file.file_download_path.clone();
        let target = file.target_path().clone();
        tokio::task::spawn_blocking(move || move_local_file(&source, &target))
            .await
            .map_err(|error| {
                ProcessingError::non_retryable(format!("File move task failed: {error}"))
            })?
            .map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Failed to move {} to {}: {error}",
                    file.file_download_path.display(),
                    file.target_path().display(),
                ))
            })
    }
}

fn move_local_file(source: &Path, target: &Path) -> io::Result<()> {
    match fs::rename(source, target) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {}
        Err(error) => return Err(error),
    }

    let target_directory = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    // Publish only a complete copy so retries cannot mistake a partial target for success.
    let temporary = tempfile::NamedTempFile::new_in(target_directory)?;
    fs::copy(source, temporary.path())?;
    temporary.persist(target).map_err(|error| error.error)?;
    fs::remove_file(source)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use source_downloader_sdk::SourceItem;
    use source_downloader_sdk::component::{FileContent, FileContentStatus};
    use std::collections::HashMap;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;
    use std::sync::OnceLock;

    fn cross_device_directories() -> Option<(tempfile::TempDir, tempfile::TempDir)> {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir_in("/dev/shm").ok()?;
        if fs::metadata(source.path()).unwrap().dev()
            == fs::metadata(target.path()).unwrap().dev()
        {
            return None;
        }
        Some((source, target))
    }

    fn file_content(source_dir: &Path, target_dir: &Path) -> FileContent {
        FileContent {
            download_path: source_dir.to_path_buf(),
            file_download_path: source_dir.join("file.zip"),
            source_save_path: target_dir.to_path_buf(),
            pattern_variables: HashMap::new(),
            file_save_path_pattern: String::new(),
            filename_pattern: String::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: target_dir.to_path_buf(),
            target_filename: "file.zip".to_owned(),
            exist_target_path: None,
            errors: Vec::new(),
            status: FileContentStatus::Normal,
            target_path: OnceLock::new(),
            data: None,
            processed_variables: None,
        }
    }

    #[tokio::test]
    async fn moves_file_across_filesystems() {
        let Some((source_dir, target_dir)) = cross_device_directories() else {
            eprintln!("Skipping: /dev/shm is unavailable or uses the same filesystem");
            return;
        };
        let file = file_content(source_dir.path(), target_dir.path());
        fs::write(&file.file_download_path, b"payload").unwrap();

        GeneralFileMover.move_file(&SourceItem::default(), &file).await.unwrap();

        assert_eq!(fs::read(file.target_path()).unwrap(), b"payload");
        assert!(!file.file_download_path.exists());
        assert_eq!(fs::read_dir(target_dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn replaces_file_across_filesystems() {
        let Some((source_dir, target_dir)) = cross_device_directories() else {
            eprintln!("Skipping: /dev/shm is unavailable or uses the same filesystem");
            return;
        };
        let mut file = file_content(source_dir.path(), target_dir.path());
        let old_target = target_dir.path().join("old.zip");
        fs::write(&old_target, b"old payload").unwrap();
        fs::write(&file.file_download_path, b"new payload").unwrap();
        file.exist_target_path = Some(old_target.clone());

        GeneralFileMover.replace(&SourceItem::default(), &[&file]).await.unwrap();

        assert_eq!(fs::read(file.target_path()).unwrap(), b"new payload");
        assert!(!file.file_download_path.exists());
        assert!(!old_target.exists());
        assert_eq!(fs::read_dir(target_dir.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn failed_cross_filesystem_replacement_restores_original_and_cleans_copy() {
        let Some((source_dir, target_dir)) = cross_device_directories() else {
            eprintln!("Skipping: /dev/shm is unavailable or uses the same filesystem");
            return;
        };
        let mut file = file_content(source_dir.path(), target_dir.path());
        fs::write(file.target_path(), b"old payload").unwrap();
        fs::create_dir(&file.file_download_path).unwrap();
        file.exist_target_path = Some(file.target_path().clone());

        GeneralFileMover.replace(&SourceItem::default(), &[&file]).await.unwrap_err();

        assert_eq!(fs::read(file.target_path()).unwrap(), b"old payload");
        assert!(file.file_download_path.is_dir());
        assert_eq!(fs::read_dir(target_dir.path()).unwrap().count(), 1);
    }
}

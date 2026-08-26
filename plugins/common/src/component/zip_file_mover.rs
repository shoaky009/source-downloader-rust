use parking_lot::Mutex;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileMover,
    ProcessingError, SdComponent, SdComponentMetadata, SourceFile,
    deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fmt::{Display, Formatter};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

static ZIP_WRITE_LOCK: Mutex<()> = Mutex::new(());

pub struct ZipFileMoverSupplier;
pub const SUPPLIER: ZipFileMoverSupplier = ZipFileMoverSupplier;
#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ZipFileMoverConfig {
    #[serde(default)]
    entry_path_depth: usize,
    #[serde(default)]
    compression_level: u8,
}

impl ComponentSupplier for ZipFileMoverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_mover("zip".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: ZipFileMoverConfig = deserialize_component_config(props)?;
        if config.compression_level > 9 {
            return Err(ComponentError::new(format!(
                "Invalid configuration at 'compression-level': expected 0..=9, got {}",
                config.compression_level
            )));
        }
        Ok(Arc::new(ZipFileMover {
            entry_path_depth: config.entry_path_depth,
            compression_level: config.compression_level,
        }))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Archives files at a configurable target-path level.".to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type": "object",
                "properties": {
                    "entry-path-depth": {
                        "type": "integer",
                        "minimum": 0,
                        "default": 0,
                        "description": "Trailing target directory levels stored inside the ZIP."
                    },
                    "compression-level": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 9,
                        "default": 0,
                        "description": "0 stores entries without recompression; 1-9 use Deflate."
                    }
                }
            })),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(FileMover)]
pub struct ZipFileMover {
    entry_path_depth: usize,
    compression_level: u8,
}

impl Display for ZipFileMover {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("zip")
    }
}

#[source_downloader_sdk::async_trait::async_trait]
impl FileMover for ZipFileMover {
    async fn move_file(
        &self,
        _: &SourceItem,
        file: &FileContent,
    ) -> Result<(), ProcessingError> {
        self.archive(vec![PendingEntry::from_file(file, self.entry_path_depth)?]).await
    }

    async fn exists(&self, paths: &[&PathBuf]) -> Vec<bool> {
        let paths: Vec<PathBuf> = paths.iter().map(|path| (*path).clone()).collect();
        let path_count = paths.len();
        let entry_path_depth = self.entry_path_depth;
        match tokio::task::spawn_blocking(move || {
            existing_entries(paths, entry_path_depth)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, "Failed to inspect ZIP entries");
                vec![false; path_count]
            }
        }
    }

    async fn create_directories(&self, path: &Path) -> Result<(), ProcessingError> {
        let destination = archive_destination(path, self.entry_path_depth)?;
        let Some(parent) = destination.archive_path.parent() else {
            return Ok(());
        };
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            failure("Create ZIP parent directory", &destination.archive_path, error)
        })
    }

    async fn replace(
        &self,
        _: &SourceItem,
        files: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        if files.is_empty() {
            return Ok(());
        }
        Err(ProcessingError::non_retryable(
            "Replacing existing ZIP entries is not supported",
        ))
    }

    async fn list_files(&self, path: &Path) -> Result<Vec<PathBuf>, ProcessingError> {
        let directory = path.to_path_buf();
        let entry_path_depth = self.entry_path_depth;
        tokio::task::spawn_blocking(move || {
            list_archive_entries(&directory, entry_path_depth)
        })
        .await
        .map_err(|error| {
            ProcessingError::non_retryable(format!(
                "List ZIP entries task failed: {error}"
            ))
        })?
    }

    async fn path_metadata(&self, path: &Path) -> Result<SourceFile, ProcessingError> {
        let path = path.to_path_buf();
        let entry_path_depth = self.entry_path_depth;
        tokio::task::spawn_blocking(move || {
            archive_entry_metadata(&path, entry_path_depth)
        })
        .await
        .map_err(|error| {
            ProcessingError::non_retryable(format!(
                "Read ZIP entry metadata task failed: {error}"
            ))
        })?
    }

    async fn is_supported_batch_move(&self) -> bool {
        true
    }

    async fn batch_move(
        &self,
        _: &SourceItem,
        files: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        let entries = files
            .iter()
            .map(|file| PendingEntry::from_file(file, self.entry_path_depth))
            .collect::<Result<Vec<_>, _>>()?;
        self.archive(entries).await
    }
}

impl ZipFileMover {
    async fn archive(&self, entries: Vec<PendingEntry>) -> Result<(), ProcessingError> {
        let compression_level = self.compression_level;
        tokio::task::spawn_blocking(move || append_entries(entries, compression_level))
            .await
            .map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Archive files task failed: {error}"
                ))
            })?
    }
}

struct PendingEntry {
    archive_path: PathBuf,
    entry_name: PathBuf,
    source_path: PathBuf,
}

impl PendingEntry {
    fn from_file(
        file: &FileContent,
        entry_path_depth: usize,
    ) -> Result<Self, ProcessingError> {
        let filename = PathBuf::from(&file.target_filename);
        if filename.file_name() != Some(filename.as_os_str()) {
            return Err(ProcessingError::non_retryable(format!(
                "ZIP target filename must be one path component: {}",
                file.target_filename
            )));
        }
        let destination = archive_destination(&file.target_save_path, entry_path_depth)?;
        Ok(Self {
            archive_path: destination.archive_path,
            entry_name: destination.entry_prefix.join(filename),
            source_path: file.file_download_path.clone(),
        })
    }
}

struct ArchiveDestination {
    archive_path: PathBuf,
    entry_prefix: PathBuf,
}

fn archive_destination(
    target_directory: &Path,
    entry_path_depth: usize,
) -> Result<ArchiveDestination, ProcessingError> {
    let mut archive_directory = target_directory;
    let mut entry_components = Vec::with_capacity(entry_path_depth);
    for _ in 0..entry_path_depth {
        let component = archive_directory.file_name().ok_or_else(|| {
            invalid_entry_path_depth(target_directory, entry_path_depth)
        })?;
        entry_components.push(component.to_os_string());
        archive_directory = archive_directory
            .parent()
            .filter(|parent| parent.file_name().is_some())
            .ok_or_else(|| {
                invalid_entry_path_depth(target_directory, entry_path_depth)
            })?;
    }

    let group_name = archive_directory
        .file_name()
        .ok_or_else(|| invalid_entry_path_depth(target_directory, entry_path_depth))?;
    let mut archive_name = group_name.to_os_string();
    archive_name.push(".zip");
    let archive_path =
        archive_directory.parent().unwrap_or_else(|| Path::new("")).join(archive_name);
    let entry_prefix = entry_components.into_iter().rev().collect();
    Ok(ArchiveDestination { archive_path, entry_prefix })
}

fn invalid_entry_path_depth(
    target_directory: &Path,
    entry_path_depth: usize,
) -> ProcessingError {
    ProcessingError::non_retryable(format!(
        "entry-path-depth {entry_path_depth} exceeds target save path '{}'",
        target_directory.display()
    ))
}

fn logical_entry(
    path: &Path,
    entry_path_depth: usize,
) -> Result<(PathBuf, PathBuf), ProcessingError> {
    let directory = path.parent().ok_or_else(|| {
        ProcessingError::non_retryable(format!(
            "ZIP logical path has no archive group: {}",
            path.display()
        ))
    })?;
    let entry_name = path.file_name().ok_or_else(|| {
        ProcessingError::non_retryable(format!(
            "ZIP logical path has no entry name: {}",
            path.display()
        ))
    })?;
    let destination = archive_destination(directory, entry_path_depth)?;
    Ok((destination.archive_path, destination.entry_prefix.join(entry_name)))
}

fn append_entries(
    entries: Vec<PendingEntry>,
    compression_level: u8,
) -> Result<(), ProcessingError> {
    if entries.is_empty() {
        return Ok(());
    }

    let source_paths: HashSet<_> =
        entries.iter().map(|entry| entry.source_path.clone()).collect();
    let mut grouped = BTreeMap::<PathBuf, Vec<PendingEntry>>::new();
    for entry in entries {
        grouped.entry(entry.archive_path.clone()).or_default().push(entry);
    }

    let _guard = ZIP_WRITE_LOCK.lock();
    for (path, entries) in grouped {
        append_archive(&path, &entries, compression_level)?;
    }
    for source_path in source_paths {
        std::fs::remove_file(&source_path).map_err(|error| {
            failure("Remove archived source file", &source_path, error)
        })?;
    }
    Ok(())
}

fn append_archive(
    path: &Path,
    entries: &[PendingEntry],
    compression_level: u8,
) -> Result<(), ProcessingError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| failure("Create ZIP parent directory", path, error))?;
    }

    let existing = archive_entry_names(path)?;
    if entries.iter().all(|entry| existing.contains(&entry.entry_name)) {
        return Ok(());
    }

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| failure("Open ZIP archive", path, error))?;
    let mut writer = if file
        .metadata()
        .map_err(|error| failure("Read ZIP metadata", path, error))?
        .len()
        == 0
    {
        ZipWriter::new(file)
    } else {
        ZipWriter::new_append(file)
            .map_err(|error| failure("Open ZIP archive for append", path, error))?
    };

    for entry in entries {
        if existing.contains(&entry.entry_name) {
            continue;
        }
        let options = if compression_level == 0 {
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
        } else {
            SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(i64::from(compression_level)))
        };
        writer
            .start_file_from_path(&entry.entry_name, options)
            .map_err(|error| failure("Create ZIP entry", path, error))?;
        let mut source = File::open(&entry.source_path).map_err(|error| {
            failure("Open source file for ZIP entry", &entry.source_path, error)
        })?;
        io::copy(&mut source, &mut writer)
            .map_err(|error| failure("Write ZIP entry", path, error))?;
    }

    writer
        .finish()
        .map_err(|error| failure("Finish ZIP archive", path, error))?
        .sync_all()
        .map_err(|error| failure("Sync ZIP archive", path, error))
}

fn archive_entry_names(path: &Path) -> Result<HashSet<PathBuf>, ProcessingError> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let file =
        File::open(path).map_err(|error| failure("Open ZIP archive", path, error))?;
    if file.metadata().map_err(|error| failure("Read ZIP metadata", path, error))?.len()
        == 0
    {
        return Ok(HashSet::new());
    }
    let archive = ZipArchive::new(file)
        .map_err(|error| failure("Read ZIP archive", path, error))?;
    Ok(archive.file_names().map(PathBuf::from).collect())
}

fn existing_entries(paths: Vec<PathBuf>, entry_path_depth: usize) -> Vec<bool> {
    let mut result = vec![false; paths.len()];
    let mut grouped = BTreeMap::<PathBuf, Vec<(usize, PathBuf)>>::new();
    for (index, path) in paths.iter().enumerate() {
        match logical_entry(path, entry_path_depth) {
            Ok((archive_path, entry_name)) => {
                grouped.entry(archive_path).or_default().push((index, entry_name));
            }
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "Invalid ZIP logical path")
            }
        }
    }

    let _guard = ZIP_WRITE_LOCK.lock();
    for (path, entries) in grouped {
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let Ok(archive) = ZipArchive::new(file) else {
            tracing::warn!(path = %path.display(), "Invalid ZIP archive");
            continue;
        };
        for (index, entry_name) in entries {
            result[index] = archive.index_for_path(entry_name).is_some();
        }
    }
    result
}

fn list_archive_entries(
    directory: &Path,
    entry_path_depth: usize,
) -> Result<Vec<PathBuf>, ProcessingError> {
    let destination = archive_destination(directory, entry_path_depth)?;
    let _guard = ZIP_WRITE_LOCK.lock();
    if !destination.archive_path.exists() {
        return Ok(Vec::new());
    }
    let archive =
        ZipArchive::new(File::open(&destination.archive_path).map_err(|error| {
            failure("Open ZIP archive", &destination.archive_path, error)
        })?)
        .map_err(|error| failure("Read ZIP archive", &destination.archive_path, error))?;
    Ok(archive
        .file_names()
        .filter_map(|name| {
            let entry_path = Path::new(name);
            let relative = if destination.entry_prefix.as_os_str().is_empty() {
                entry_path
            } else {
                entry_path.strip_prefix(&destination.entry_prefix).ok()?
            };
            (!relative.as_os_str().is_empty()).then(|| directory.join(relative))
        })
        .collect())
}

fn archive_entry_metadata(
    path: &Path,
    entry_path_depth: usize,
) -> Result<SourceFile, ProcessingError> {
    let (archive_path, entry_name) = logical_entry(path, entry_path_depth)?;
    let _guard = ZIP_WRITE_LOCK.lock();
    let mut archive = ZipArchive::new(
        File::open(&archive_path)
            .map_err(|error| failure("Open ZIP archive", &archive_path, error))?,
    )
    .map_err(|error| failure("Read ZIP archive", &archive_path, error))?;
    let size = archive
        .by_path(&entry_name)
        .map_err(|error| failure("Read ZIP entry", &archive_path, error))?
        .size();
    let mut file = SourceFile::new(path.to_path_buf());
    file.attrs.insert("size".to_owned(), Value::from(size));
    Ok(file)
}

fn failure(operation: &str, path: &Path, error: impl Display) -> ProcessingError {
    ProcessingError::non_retryable(format!("{operation} '{}': {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::component::{
        EMPTY_COMPONENT_CREATE_CONTEXT, FileContentStatus,
    };
    use std::io::Read;

    fn file(source_path: PathBuf, target_save_path: PathBuf, name: &str) -> FileContent {
        FileContent {
            file_download_path: source_path,
            target_save_path,
            target_filename: name.to_owned(),
            status: FileContentStatus::Normal,
            ..Default::default()
        }
    }

    fn contents(path: &Path) -> BTreeMap<String, String> {
        let mut archive = ZipArchive::new(File::open(path).unwrap()).unwrap();
        let mut contents = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let name = entry.name().to_owned();
            let mut content = String::new();
            entry.read_to_string(&mut content).unwrap();
            contents.insert(name, content);
        }
        contents
    }

    fn mover(entry_path_depth: usize, compression_level: u8) -> ZipFileMover {
        ZipFileMover { entry_path_depth, compression_level }
    }

    #[test]
    fn entry_path_depth_selects_archive_boundary() {
        let direct = archive_destination(Path::new("pixiv/artist/42"), 0).unwrap();
        let parent = archive_destination(Path::new("pixiv/artist/42"), 1).unwrap();

        assert_eq!(
            (
                direct.archive_path,
                direct.entry_prefix,
                parent.archive_path,
                parent.entry_prefix,
            ),
            (
                PathBuf::from("pixiv/artist/42.zip"),
                PathBuf::new(),
                PathBuf::from("pixiv/artist.zip"),
                PathBuf::from("42"),
            )
        );
    }

    #[tokio::test]
    async fn depth_zero_groups_by_target_directory_and_appends() {
        let temp = tempfile::tempdir().unwrap();
        let first_source = temp.path().join("first.jpg");
        let second_source = temp.path().join("second.jpg");
        std::fs::write(&first_source, "first").unwrap();
        std::fs::write(&second_source, "second").unwrap();
        let group = temp.path().join("pixiv/artist/42");
        let first = file(first_source.clone(), group.clone(), "1.jpg");
        let second = file(second_source.clone(), group.clone(), "2.jpg");
        let mover = mover(0, 0);

        mover.batch_move(&SourceItem::default(), &[&first]).await.unwrap();
        mover.batch_move(&SourceItem::default(), &[&second]).await.unwrap();

        let archive = archive_destination(&group, 0).unwrap().archive_path;
        assert_eq!(
            (contents(&archive), [first_source.exists(), second_source.exists()],),
            (
                BTreeMap::from([
                    ("1.jpg".to_owned(), "first".to_owned()),
                    ("2.jpg".to_owned(), "second".to_owned()),
                ]),
                [false, false],
            )
        );
    }

    #[tokio::test]
    async fn depth_one_groups_sibling_directories_into_parent_archive() {
        let temp = tempfile::tempdir().unwrap();
        let first_source = temp.path().join("first.jpg");
        let second_source = temp.path().join("second.jpg");
        std::fs::write(&first_source, "first").unwrap();
        std::fs::write(&second_source, "second").unwrap();
        let first_group = temp.path().join("pixiv/artist/42");
        let second_group = temp.path().join("pixiv/artist/43");
        let first = file(first_source, first_group.clone(), "1.jpg");
        let second = file(second_source, second_group, "1.jpg");
        let mover = mover(1, 0);

        mover.batch_move(&SourceItem::default(), &[&first, &second]).await.unwrap();

        let archive = archive_destination(&first_group, 1).unwrap().archive_path;
        assert_eq!(
            contents(&archive),
            BTreeMap::from([
                ("42/1.jpg".to_owned(), "first".to_owned()),
                ("43/1.jpg".to_owned(), "second".to_owned()),
            ])
        );
    }

    #[tokio::test]
    async fn compression_level_zero_stores_and_nonzero_deflates() {
        let temp = tempfile::tempdir().unwrap();
        let stored_source = temp.path().join("stored.txt");
        let deflated_source = temp.path().join("deflated.txt");
        let content = "compressible".repeat(1_000);
        std::fs::write(&stored_source, &content).unwrap();
        std::fs::write(&deflated_source, &content).unwrap();
        let stored_group = temp.path().join("stored/42");
        let deflated_group = temp.path().join("deflated/42");
        let stored = file(stored_source, stored_group.clone(), "content.txt");
        let deflated = file(deflated_source, deflated_group.clone(), "content.txt");

        mover(0, 0).batch_move(&SourceItem::default(), &[&stored]).await.unwrap();
        mover(0, 6).batch_move(&SourceItem::default(), &[&deflated]).await.unwrap();

        let mut stored_archive = ZipArchive::new(
            File::open(archive_destination(&stored_group, 0).unwrap().archive_path)
                .unwrap(),
        )
        .unwrap();
        let mut deflated_archive = ZipArchive::new(
            File::open(archive_destination(&deflated_group, 0).unwrap().archive_path)
                .unwrap(),
        )
        .unwrap();
        let stored = stored_archive.by_name("content.txt").unwrap();
        let deflated = deflated_archive.by_name("content.txt").unwrap();

        assert_eq!(
            (
                stored.compression(),
                deflated.compression(),
                deflated.compressed_size() < deflated.size(),
            ),
            (CompressionMethod::Stored, CompressionMethod::Deflated, true)
        );
    }

    #[tokio::test]
    async fn exists_checks_entries_at_configured_depth() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("download.jpg");
        std::fs::write(&source, "image").unwrap();
        let group = temp.path().join("pixiv/artist/42");
        let file = file(source, group.clone(), "1.jpg");
        let missing = group.join("2.jpg");
        let mover = mover(1, 0);
        mover.batch_move(&SourceItem::default(), &[&file]).await.unwrap();

        assert_eq!(
            mover.exists(&[file.target_path(), &missing]).await,
            vec![true, false]
        );
    }

    #[test]
    fn supplier_accepts_depth_and_compression_configuration() {
        let props = Map::from_iter([
            ("entry-path-depth".to_owned(), Value::from(1)),
            ("compression-level".to_owned(), Value::from(6)),
        ]);

        let component = SUPPLIER.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props).unwrap();

        assert!(component.as_file_mover().is_ok());
    }

    #[test]
    fn supplier_rejects_compression_level_above_nine() {
        let props = Map::from_iter([("compression-level".to_owned(), Value::from(10))]);

        let error = SUPPLIER.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props).unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid configuration at 'compression-level': expected 0..=9, got 10"
        );
    }
}

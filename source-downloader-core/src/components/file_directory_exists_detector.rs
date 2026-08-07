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

pub struct ItemDirectoryExistsDetectorSupplier;
pub const SUPPLIER: ItemDirectoryExistsDetectorSupplier =
    ItemDirectoryExistsDetectorSupplier;

impl ComponentSupplier for ItemDirectoryExistsDetectorSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_exists_detector("item-dir".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(FileDirectoryExistsDetector))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(FileExistsDetector)]
pub struct FileDirectoryExistsDetector;

impl Display for FileDirectoryExistsDetector {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("item-dir")
    }
}

impl FileExistsDetector for FileDirectoryExistsDetector {
    fn exists<'a>(
        &self,
        file_mover: &'a dyn FileMover,
        _: &'a SourceItem,
        file_contents: &'a [FileContent],
    ) -> HashMap<&'a PathBuf, Option<PathBuf>> {
        let mut directories = Vec::new();
        for file in file_contents {
            if let Some(directory) = file_save_root_directory(file)
                && !directories.iter().any(|known| known == &directory)
            {
                directories.push(directory);
            }
        }
        let directory_refs: Vec<&PathBuf> = directories.iter().collect();
        let exists = file_mover.exists(&directory_refs);
        let exists_by_directory: HashMap<PathBuf, bool> =
            directories.into_iter().zip(exists).collect();

        file_contents
            .iter()
            .map(|file| {
                let target = file.target_path();
                let directory = file_save_root_directory(file);
                let existing = directory
                    .as_ref()
                    .and_then(|directory| exists_by_directory.get(directory))
                    .copied()
                    .unwrap_or(false);
                (target, existing.then(|| file.target_path().clone()))
            })
            .collect()
    }
}

fn file_save_root_directory(file: &FileContent) -> Option<PathBuf> {
    if file.source_save_path == file.target_save_path {
        return None;
    }
    let relative = file.target_save_path.strip_prefix(&file.source_save_path).ok()?;
    let first = relative.components().next()?;
    let root = file.source_save_path.join(first);
    (root != file.source_save_path).then_some(root)
}
#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::component::ComponentSupplier;

    #[test]
    fn exists_returns_the_file_target_path_when_save_directory_exists() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("source");
        let target_root = source_root.join("season");
        std::fs::create_dir_all(&target_root).unwrap();

        let file = FileContent {
            source_save_path: source_root,
            target_save_path: target_root,
            target_filename: String::from("episode.txt"),
            ..Default::default()
        };

        let files = [file];
        let mover = crate::components::system_file_mover::SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &Map::new(),
            )
            .unwrap()
            .as_file_mover()
            .unwrap();
        let source_item = SourceItem::default();
        let result =
            FileDirectoryExistsDetector.exists(mover.as_ref(), &source_item, &files);
        let existing_target = result.values().next().cloned().flatten();

        assert_eq!(existing_target.as_ref(), Some(files[0].target_path()));
    }
}

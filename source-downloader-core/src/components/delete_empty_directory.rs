use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, ItemContent,
    ProcessContext, ProcessListener, ProcessingError, SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use walkdir::WalkDir;

pub struct DeleteEmptyDirectorySupplier;
pub const SUPPLIER: DeleteEmptyDirectorySupplier = DeleteEmptyDirectorySupplier;

impl ComponentSupplier for DeleteEmptyDirectorySupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::listener("delete-empty-directory".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(DeleteEmptyDirectory))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(ProcessListener)]
pub struct DeleteEmptyDirectory;

impl Display for DeleteEmptyDirectory {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("delete-empty-directory")
    }
}

impl ProcessListener for DeleteEmptyDirectory {
    fn on_item_success(
        &self,
        _: &dyn ProcessContext,
        item_content: &ItemContent,
    ) -> Result<(), ProcessingError> {
        if item_content.file_contents.len() == 1 {
            let file = &item_content.file_contents[0];
            let Some(parent) = file.file_download_path.parent() else {
                return Ok(());
            };
            if !parent.exists() {
                return Ok(());
            }
            let is_empty = fs::read_dir(parent)?.next().is_none();
            if is_empty {
                fs::remove_dir(parent)?;
            }
            return Ok(());
        }

        let Some(file) = item_content.file_contents.first() else {
            return Ok(());
        };
        let Some(directory) = file_download_root_directory(file) else {
            return Ok(());
        };
        let all_directories =
            WalkDir::new(&directory).into_iter().try_fold(true, |_, entry| {
                let entry = entry
                    .map_err(|error| ProcessingError::non_retryable(error.to_string()))?;
                Ok::<_, ProcessingError>(entry.file_type().is_dir())
            })?;
        if all_directories {
            fs::remove_dir_all(directory)?;
        }
        Ok(())
    }

    fn on_item_error(
        &self,
        _: &dyn ProcessContext,
        _: &SourceItem,
        _: &ProcessingError,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    fn on_process_completed(
        &self,
        _: &dyn ProcessContext,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }
}

fn file_download_root_directory(file: &FileContent) -> Option<PathBuf> {
    let mut path = file.file_download_path.parent()?.to_path_buf();
    if path == file.download_path {
        return None;
    }
    while let Some(parent) = path.parent() {
        if parent == file.download_path {
            break;
        }
        path = parent.to_path_buf();
    }
    (path != file.download_path).then_some(path)
}

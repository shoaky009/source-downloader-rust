use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, ItemContent,
    ProcessContext, ProcessListener, ProcessingError, SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

pub struct TouchItemDirectorySupplier;
pub const SUPPLIER: TouchItemDirectorySupplier = TouchItemDirectorySupplier;

impl ComponentSupplier for TouchItemDirectorySupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::listener("touch-item-directory".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(TouchItemDirectory))
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
pub struct TouchItemDirectory;

impl Display for TouchItemDirectory {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("touch-item-directory")
    }
}

impl ProcessListener for TouchItemDirectory {
    fn on_item_success(
        &self,
        _: &dyn ProcessContext,
        item_content: &ItemContent,
    ) -> Result<(), ProcessingError> {
        let mut directories = Vec::new();
        for file in item_content.file_contents {
            if let Some(directory) = file_save_root_directory(file)
                && directory.exists()
                && !directories.contains(&directory)
            {
                directories.push(directory);
            }
        }
        for directory in directories {
            tracing::debug!(
                item = %item_content.source_item.title,
                directory = %directory.display(),
                "Touching item directory"
            );
            touch_directory(&directory)?;
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

fn file_save_root_directory(file: &FileContent) -> Option<PathBuf> {
    if file.source_save_path == file.target_save_path {
        return None;
    }
    let relative = file.target_save_path.strip_prefix(&file.source_save_path).ok()?;
    let first = relative.components().next()?;
    let root = file.source_save_path.join(first);
    (root != file.source_save_path).then_some(root)
}

fn touch_directory(path: &Path) -> Result<(), ProcessingError> {
    #[cfg(windows)]
    let status = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$path = [Environment]::GetEnvironmentVariable('SOURCE_DOWNLOADER_TOUCH_PATH'); (Get-Item -LiteralPath $path).LastWriteTime = Get-Date",
        ])
        .env("SOURCE_DOWNLOADER_TOUCH_PATH", path)
        .status()?;

    #[cfg(not(windows))]
    let status = Command::new("touch").arg("-m").arg(path).status()?;

    if status.success() {
        Ok(())
    } else {
        Err(ProcessingError::non_retryable(format!(
            "Failed to update directory timestamp: {}",
            path.display()
        )))
    }
}

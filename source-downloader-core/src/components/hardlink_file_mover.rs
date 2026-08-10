use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContent, FileMover,
    ProcessingError, SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::fs;
use std::sync::Arc;

pub struct HardlinkFileMoverSupplier;
pub const SUPPLIER: HardlinkFileMoverSupplier = HardlinkFileMoverSupplier;

impl ComponentSupplier for HardlinkFileMoverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_mover("hardlink".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(HardlinkFileMover))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Moves files by creating hard links.".to_owned(),
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
pub struct HardlinkFileMover;

impl Display for HardlinkFileMover {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("hardlink")
    }
}

impl FileMover for HardlinkFileMover {
    fn move_file(
        &self,
        _: &SourceItem,
        file: &FileContent,
    ) -> Result<(), ProcessingError> {
        fs::hard_link(&file.file_download_path, file.target_path()).map_err(Into::into)
    }

    fn replace(
        &self,
        _: &SourceItem,
        files: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        for file in files {
            let target = file.target_path();
            let is_symlink = fs::symlink_metadata(target)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false);
            if !is_symlink {
                tracing::warn!(
                    target = %target.display(),
                    "target file is not symbolic link; replacement skipped"
                );
                continue;
            }
            fs::remove_file(target)?;
            fs::hard_link(&file.file_download_path, target)?;
        }
        Ok(())
    }
}

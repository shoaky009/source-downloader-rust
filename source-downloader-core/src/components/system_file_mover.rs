use serde_json::{Map, Value};
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileMover, SdComponent,
    SdComponentMetadata,
};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct SystemFileMoverSupplier {}
pub const SUPPLIER: SystemFileMoverSupplier = SystemFileMoverSupplier {};
const INSTANCE: SystemFileMover = SystemFileMover {};

impl ComponentSupplier for SystemFileMoverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_mover("system-file".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(INSTANCE))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        todo!()
    }
}

#[derive(SdComponent, Debug)]
#[component(FileMover)]
struct SystemFileMover {}

impl Display for SystemFileMover {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "system-file")
    }
}

impl FileMover for SystemFileMover {}

#[cfg(test)]
mod tests {
    use super::SystemFileMover;
    use source_downloader_sdk::SourceItem;
    use source_downloader_sdk::component::{FileContent, FileContentStatus, FileMover};
    use source_downloader_sdk::http::Uri;
    use source_downloader_sdk::time::OffsetDateTime;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::OnceLock;

    #[test]
    fn moves_file_to_target_path() {
        let root = std::env::temp_dir()
            .join(format!("source-downloader-mover-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let download_dir = root.join("download");
        let target_dir = root.join("target");
        fs::create_dir_all(&download_dir).unwrap();
        let download_file = download_dir.join("file.txt");
        fs::write(&download_file, b"content").unwrap();

        let content = FileContent {
            download_path: download_dir,
            file_download_path: download_file.clone(),
            source_save_path: target_dir.clone(),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: target_dir.clone(),
            target_filename: "renamed.txt".to_owned(),
            exist_target_path: None,
            errors: Vec::new(),
            status: FileContentStatus::Normal,
            target_path: OnceLock::new(),
            data: None,
        };
        let item = SourceItem {
            title: "item".to_owned(),
            link: Uri::from_static("https://example.com"),
            datetime: OffsetDateTime::now_utc(),
            content_type: "text/plain".to_owned(),
            download_uri: Uri::from_static("https://example.com/file"),
            attrs: Default::default(),
            tags: Vec::new(),
            identity: None,
        };
        let mover = SystemFileMover {};

        mover.create_directories(&target_dir).unwrap();
        mover.move_file(&item, &content).unwrap();

        assert!(!download_file.exists());
        assert_eq!(fs::read(target_dir.join("renamed.txt")).unwrap(), b"content");
        fs::remove_dir_all(root).unwrap();
    }
}

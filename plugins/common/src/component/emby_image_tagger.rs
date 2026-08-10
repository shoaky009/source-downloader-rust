use source_downloader_sdk::SdComponent;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileTagger, SdComponent,
    SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct EmbyImageTaggerSupplier;

pub const SUPPLIER: EmbyImageTaggerSupplier = EmbyImageTaggerSupplier;

impl ComponentSupplier for EmbyImageTaggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_tagger("emby-image".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(EmbyImageTagger))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Adds Emby image tags to files.".into(),
            props_json_schema: None,
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, SdComponent)]
#[component(FileTagger)]
struct EmbyImageTagger;

impl Display for EmbyImageTagger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "emby-image")
    }
}

#[async_trait]
impl FileTagger for EmbyImageTagger {
    async fn tag(&self, source_file: &SourceFile) -> Option<String> {
        let filename = source_file.path.file_name()?.to_str()?;
        let filename_lower = filename.to_lowercase();
        if filename_lower.contains("thumb") {
            return Some("thumb".to_string());
        }
        if filename_lower.contains("poster") {
            return Some("poster".to_string());
        }
        let extension = source_file.path.extension()?.to_str()?;
        if !["jpg", "jpeg", "png", "webp", "bmp"].contains(&extension) {
            return None;
        }
        let size = imagesize::size(&source_file.path).ok()?;
        Some(if size.width >= size.height { "thumb" } else { "poster" }.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    async fn tag(path: PathBuf) -> Option<String> {
        EmbyImageTagger.tag(&SourceFile::new(path)).await
    }

    #[test]
    fn supplier_supports_implicit_construction() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::file_tagger("emby-image".to_string())]
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

    #[tokio::test]
    async fn filename_tags_take_priority_without_reading_the_file() {
        assert_eq!(
            Some("thumb".to_string()),
            tag(PathBuf::from("missing-poster-thumb.bin")).await
        );
        assert_eq!(
            Some("poster".to_string()),
            tag(PathBuf::from("missing-POSTER.bin")).await
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_missing_and_damaged_images() {
        let dir = tempfile::tempdir().unwrap();
        let damaged = dir.path().join("damaged.png");
        std::fs::write(&damaged, b"not an image").unwrap();
        assert_eq!(None, tag(dir.path().join("missing.jpg")).await);
        assert_eq!(None, tag(dir.path().join("image.gif")).await);
        assert_eq!(None, tag(damaged).await);
    }

    #[tokio::test]
    async fn classifies_landscape_portrait_and_square_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        let landscape = dir.path().join("landscape.png");
        let portrait = dir.path().join("portrait.png");
        let square = dir.path().join("square.png");
        write_png_header(&landscape, 20, 10);
        write_png_header(&portrait, 10, 20);
        write_png_header(&square, 10, 10);
        assert_eq!(Some("thumb".to_string()), tag(landscape).await);
        assert_eq!(Some("poster".to_string()), tag(portrait).await);
        assert_eq!(Some("thumb".to_string()), tag(square).await);
    }

    fn write_png_header(path: &std::path::Path, width: u32, height: u32) {
        let mut bytes = vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H',
            b'D', b'R',
        ];
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
        std::fs::write(path, bytes).unwrap();
    }
}

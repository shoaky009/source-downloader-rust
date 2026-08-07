use async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, ProcessingError,
    SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::sync::Arc;

pub struct UrlFileResolverSupplier;
pub const SUPPLIER: UrlFileResolverSupplier = UrlFileResolverSupplier;

impl ComponentSupplier for UrlFileResolverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_resolver("url".to_owned())]
    }

    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(UrlFileResolver))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(ItemFileResolver)]
pub struct UrlFileResolver;

impl Display for UrlFileResolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("url")
    }
}

#[async_trait]
impl ItemFileResolver for UrlFileResolver {
    async fn resolve_files(
        &self,
        source_item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let raw_path = source_item.download_uri.path();
        let filename = url::Url::parse(&source_item.download_uri.to_string())
            .ok()
            .and_then(|url| {
                url.path_segments()
                    .and_then(|mut segments| segments.next_back().map(str::to_owned))
            })
            .or_else(|| raw_path.rsplit('/').next().map(str::to_owned))
            .filter(|filename| !filename.trim().is_empty())
            .unwrap_or_else(|| source_item.hashing());
        Ok(vec![SourceFile::new(PathBuf::from(filename))])
    }
}

use source_downloader_sdk::component::SourceItemFilter;
use source_downloader_sdk::storage::ProcessingStorage;
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;

#[derive(SdComponent)]
#[component(SourceItemFilter)]
pub struct SourceItemIdentityFilter {
    pub processor_name: String,
    pub storage: Arc<dyn ProcessingStorage>,
}

impl Debug for SourceItemIdentityFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceItemIdentityFilter")
            .field("processor_name", &self.processor_name)
            .field("storage", &"<skipped>")
            .finish()
    }
}

#[async_trait::async_trait]
impl SourceItemFilter for SourceItemIdentityFilter {
    async fn filter(&self, item: &SourceItem) -> bool {
        let exists = self
            .storage
            .processing_content_exists(&self.processor_name, &item.hashing())
            .await
            .unwrap_or(false);
        if exists {
            tracing::debug!("Item already processed:{}", item);
        }
        !exists
    }
}

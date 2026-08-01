use async_trait::async_trait;
use source_downloader_sdk::component::{
    ItemContent, ItemContentFilter, SourceItemFilter,
};
use source_downloader_sdk::storage::{ProcessingContentQuery, ProcessingStorage};
use source_downloader_sdk::{SdComponent, SourceItem};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

#[derive(SdComponent)]
#[component(SourceItemFilter, ItemContentFilter)]
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

impl Display for SourceItemIdentityFilter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("item_hashing_or_identity")
    }
}

#[async_trait]
impl SourceItemFilter for SourceItemIdentityFilter {
    async fn filter(&self, item: &SourceItem) -> bool {
        let exists = self
            .storage
            .processing_content_exists(&self.processor_name, &item.hashing())
            .await
            .unwrap_or(false);
        if exists {
            tracing::debug!(
                processor = %self.processor_name,
                item = %item,
                "Source item was already submitted and will be skipped"
            );
        }
        !exists
    }
}

#[async_trait]
impl ItemContentFilter for SourceItemIdentityFilter {
    async fn filter(&self, content: &ItemContent) -> bool {
        if let Some(identity) = content.source_item.identity.as_deref()
            && !identity.trim().is_empty()
        {
            let query = ProcessingContentQuery {
                processor_name: Some(vec![self.processor_name.clone()]),
                item_identity: Some(vec![identity.to_owned()]),
                ..Default::default()
            };
            let exists = self
                .storage
                .query_processing_content(&query)
                .await
                .map(|contents| !contents.is_empty())
                .unwrap_or(false);
            return !exists;
        }
        SourceItemFilter::filter(self, content.source_item).await
    }
}

use source_downloader_sdk::storage::ProcessingStorage;
use std::sync::Arc;

pub mod error_handle;
pub mod service;

pub struct ApplicationContext {
    pub core: Arc<source_downloader_core::application::CoreApplication>,
    pub storage: Arc<dyn ProcessingStorage>,
}

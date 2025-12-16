use sdk::storage::ProcessingStorage;
use std::sync::Arc;

pub mod error_handle;
pub mod service;

pub struct ApplicationContext {
    pub core: Arc<core::application::CoreApplication>,
    pub storage: Arc<dyn ProcessingStorage>,
}

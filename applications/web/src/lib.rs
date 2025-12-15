use sdk::storage::ProcessingStorage;
use std::sync::Arc;

pub mod app;
pub mod component;
pub mod error_handle;
pub mod processor;
pub mod processing;
pub mod path;

pub struct ApplicationContext {
    pub core: Arc<core::application::CoreApplication>,
    pub storage: Arc<dyn ProcessingStorage>,
}

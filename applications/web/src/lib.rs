use core::CoreApplication;
use sdk::ProcessingStorage;
use std::sync::Arc;

pub mod app;
pub mod component;
pub mod error_handle;
pub mod processor;
pub mod processing;
pub mod path;

pub struct ApplicationContext {
    pub core: Arc<CoreApplication>,
    pub storage: Arc<dyn ProcessingStorage>,
}

use sdk::ProcessingStorage;
use std::sync::Arc;
use core::CoreApplication;

pub mod dao;
pub mod error;
pub mod model;
pub mod service;

pub struct ApplicationContext {
    pub core: Arc<CoreApplication>,
    pub storage: Arc<dyn ProcessingStorage>,
}

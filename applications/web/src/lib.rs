use core::CoreApplication;
use sdk::ProcessingStorage;
use std::sync::Arc;

pub mod dao;
pub mod error;
pub mod model;
pub mod service;

pub mod app;

pub struct ApplicationContext {
    pub core: Arc<CoreApplication>,
    pub storage: Arc<dyn ProcessingStorage>,
}

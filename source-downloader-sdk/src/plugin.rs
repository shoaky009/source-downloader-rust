#![allow(dead_code)]

use crate::component::ComponentSupplier;
use crate::instance::InstanceFactory;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::Arc;

pub trait Plugin: Send + Sync {
    fn init(&self, plugin_context: Arc<dyn PluginContext>);

    fn destroy(&self, plugin_context: Arc<dyn PluginContext>);

    fn get_instance_factories(&self) -> Vec<Arc<dyn InstanceFactory>>;

    fn get_component_suppliers(&self) -> Vec<Arc<dyn ComponentSupplier>>;

    fn description(&self) -> PluginDescription;
}

pub trait PluginContext: Send + Sync {
    fn get_persistent_data_path(&self) -> &Path;
}

pub struct PluginDescription {
    pub name: String,
    pub version: String,
}

impl Display for PluginDescription {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.name, self.version)
    }
}

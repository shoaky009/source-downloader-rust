#![allow(dead_code)]

use crate::component::{ComponentError, ComponentSupplier};
use crate::instance::InstanceFactory;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::Arc;

pub trait Plugin: Send + Sync {
    fn init(&self, plugin_context: Arc<dyn PluginContext>);

    fn destroy(&self, plugin_context: Arc<dyn PluginContext>);

    fn description(&self) -> PluginDescription;
}

pub trait PluginContext: Send + Sync {
    fn get_persistent_data_path(&self) -> &Path;

    fn register_supplier(&self, suppliers: Vec<Arc<dyn ComponentSupplier>>);

    fn register_instance_factory(
        &self,
        factories: Vec<Arc<dyn InstanceFactory>>,
    ) -> Result<bool, ComponentError>;
}

pub struct PluginDescription {
    pub name: String,
    pub version: String,
}

impl Display for PluginDescription {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{}:{}", self.name, self.version)
    }
}

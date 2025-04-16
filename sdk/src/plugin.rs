#![allow(dead_code)]

use crate::component::ComponentSupplier;
use crate::instance::InstanceFactory;
use std::fmt::{Display, Formatter};
use std::path::Path;
use std::sync::{Arc, Mutex};

pub trait Plugin {
    fn init(&self, plugin_context: Arc<Mutex<dyn PluginContext>>);

    fn destroy(&self, plugin_context: Arc<dyn PluginContext>);

    fn description(&self) -> PluginDescription;
}

pub trait PluginContext {
    fn get_persistent_data_path(&self) -> &Path;

    fn register_supplier(&mut self, suppliers: Vec<Arc<dyn ComponentSupplier>>);

    fn register_instance_factory(&mut self, factories: Vec<Box<dyn InstanceFactory>>);
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

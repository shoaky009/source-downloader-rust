#![allow(dead_code)]
use crate::component::{ComponentSupplier, SdComponent};
use crate::instance::{InstanceFactory, InstanceManager};
use std::collections::HashMap;
use std::path::Path;

pub trait Plugin {
    fn init(&self, plugin_context: &impl PluginContext);

    fn destroy(&self, plugin_context: &impl PluginContext);

    fn description(&self) -> PluginDescription;
}

pub trait PluginContext {

    fn get_persistent_data_path(&self) -> &Path;

    fn register_supplier(&mut self, suppliers: Vec<Box<dyn ComponentSupplier<dyn SdComponent>>>);

    fn register_instance_factory(&mut self, factories: Vec<Box<dyn InstanceFactory>>);

    fn load_instance<T: 'static>(
        &self,
        name: &str,
        klass: &str,
        props: Option<HashMap<String, String>>,
    ) -> T;

    fn get_instance_manager(&self) -> dyn InstanceManager;
}

pub struct PluginDescription {
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
}

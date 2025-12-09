mod components;

use crate::components::test_source;
use sdk::instance::InstanceFactory;
use sdk::plugin::{Plugin, PluginContext, PluginDescription};
use std::sync::Arc;

pub struct CommonPlugin;
pub const PLUGIN: CommonPlugin = CommonPlugin {};

impl Plugin for CommonPlugin {
    fn init(&self, _: Arc<dyn PluginContext>) {}

    fn destroy(&self, _: Arc<dyn PluginContext>) {}

    fn get_instance_factories(&self) -> Vec<Arc<dyn InstanceFactory>> {
        vec![]
    }

    fn get_component_suppliers(&self) -> Vec<Arc<dyn sdk::component::ComponentSupplier>> {
        vec![Arc::new(test_source::SUPPLIER)]
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            name: "common".to_string(),
            version: "0.1.0".to_string(),
        }
    }
}
// #[unsafe(no_mangle)]
// pub extern "Rust" fn create_plugin() -> Box<dyn Plugin> {
//     Box::new(CommonPlugin {})
// }

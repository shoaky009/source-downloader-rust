mod components;

use crate::components::test_source::TestSourceSupplier;
use sdk::plugin::{Plugin, PluginContext, PluginDescription};
use std::sync::{Arc, Mutex};

struct CommonPlugin;

impl Plugin for CommonPlugin {
    fn init(&self, plugin_context: Arc<Mutex<dyn PluginContext>>) {
        println!("Initializing common plugin");
        if let Ok(mut context) = plugin_context.lock() {
            context.register_supplier(vec![Arc::new(TestSourceSupplier::new())]);
        }
    }

    fn destroy(&self, _: Arc<dyn PluginContext>) {
        println!("Destroying common plugin");
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            name: "common".to_string(),
            version: "0.1.0".to_string(),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "Rust" fn create_plugin() -> Box<dyn Plugin> {
    Box::new(CommonPlugin {})
}

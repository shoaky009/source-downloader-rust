mod components;

use crate::components::test_source::TestSourceSupplier;
use sdk::plugin::{Plugin, PluginContext, PluginDescription};
use std::sync::Arc;
use tracing::info;

struct CommonPlugin;

impl Plugin for CommonPlugin {
    fn init(&self, plugin_context: Arc<dyn PluginContext>) {
        info!("Initializing common plugin");
        plugin_context.register_supplier(vec![Arc::new(TestSourceSupplier::new())]);
    }

    fn destroy(&self, _: Arc<dyn PluginContext>) {
        info!("Destroying common plugin");
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

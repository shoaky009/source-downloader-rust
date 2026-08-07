mod client;
mod integration;
mod media_tagger;
mod session;
mod source;

use source_downloader_sdk::component::ComponentSupplier;
use source_downloader_sdk::instance::InstanceFactory;
use source_downloader_sdk::plugin::{Plugin, PluginContext, PluginDescription};
use std::sync::Arc;

pub struct TelegramPlugin;
pub const PLUGIN: TelegramPlugin = TelegramPlugin;

impl Plugin for TelegramPlugin {
    fn init(&self, _: Arc<dyn PluginContext>) {}

    fn destroy(&self, _: Arc<dyn PluginContext>) {}

    fn get_instance_factories(&self) -> Vec<Arc<dyn InstanceFactory>> {
        vec![Arc::new(client::INSTANCE_FACTORY)]
    }

    fn get_component_suppliers(&self) -> Vec<Arc<dyn ComponentSupplier>> {
        vec![
            Arc::new(source::SOURCE_SUPPLIER),
            Arc::new(integration::INTEGRATION_SUPPLIER),
            Arc::new(media_tagger::MEDIA_TAGGER_SUPPLIER),
        ]
    }

    fn description(&self) -> PluginDescription {
        PluginDescription {
            name: "telegram".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
}

#[unsafe(no_mangle)]
pub extern "Rust" fn create_plugin() -> Box<dyn Plugin> {
    Box::new(TelegramPlugin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_exposes_factory_and_component_suppliers() {
        assert_eq!(PLUGIN.get_instance_factories().len(), 1);
        assert_eq!(PLUGIN.get_component_suppliers().len(), 3);
        assert_eq!(PLUGIN.description().name, "telegram");
    }
}

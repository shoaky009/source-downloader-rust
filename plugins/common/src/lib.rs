mod component;
mod http;
mod instance;
#[cfg(test)]
mod test_support;
pub mod util;

use crate::component::{
    ai_variable_provider, anime_file_filter, anime_replacement_decider, anime_tagger,
    anime_title_variable_provider, doujin_title_trimmer, emby_image_tagger,
    episode_variable_provider, language_variable_provider, media_type_exists_detector,
    mikan_source, resolution_variable_provider, season_variable_provider,
    simple_file_tagger,
};
use source_downloader_sdk::component::ComponentSupplier;
use source_downloader_sdk::instance::InstanceFactory;
use source_downloader_sdk::plugin::{Plugin, PluginContext, PluginDescription};
use std::sync::Arc;

pub struct CommonPlugin;
pub const PLUGIN: CommonPlugin = CommonPlugin {};

impl Plugin for CommonPlugin {
    fn init(&self, _: Arc<dyn PluginContext>) {}

    fn destroy(&self, _: Arc<dyn PluginContext>) {}

    fn get_instance_factories(&self) -> Vec<Arc<dyn InstanceFactory>> {
        vec![]
    }

    fn get_component_suppliers(&self) -> Vec<Arc<dyn ComponentSupplier>> {
        vec![
            Arc::new(mikan_source::SUPPLIER),
            Arc::new(anime_file_filter::SUPPLIER),
            Arc::new(anime_replacement_decider::SUPPLIER),
            Arc::new(anime_tagger::SUPPLIER),
            Arc::new(anime_title_variable_provider::SUPPLIER),
            Arc::new(doujin_title_trimmer::SUPPLIER),
            Arc::new(emby_image_tagger::SUPPLIER),
            Arc::new(episode_variable_provider::SUPPLIER),
            Arc::new(language_variable_provider::SUPPLIER),
            Arc::new(media_type_exists_detector::SUPPLIER),
            Arc::new(resolution_variable_provider::SUPPLIER),
            Arc::new(season_variable_provider::SUPPLIER),
            Arc::new(simple_file_tagger::SUPPLIER),
            Arc::new(ai_variable_provider::SUPPLIER),
        ]
    }

    fn description(&self) -> PluginDescription {
        PluginDescription { name: "common".to_string(), version: "0.1.0".to_string() }
    }
}
// #[unsafe(no_mangle)]
// pub extern "Rust" fn create_plugin() -> Box<dyn Plugin> {
//     Box::new(CommonPlugin {})
// }

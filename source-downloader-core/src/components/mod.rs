use crate::component_manager::ComponentManager;
use source_downloader_sdk::component::ComponentSupplier;
use std::sync::Arc;

pub mod composites;
pub mod cron_trigger;
pub mod delete_empty_directory;
pub mod expression_file_content_filter;
pub mod expression_item_content_filter;
pub mod expression_item_filter;
pub mod file_directory_exists_detector;
pub mod file_replacement_decider_always;
pub mod file_replacement_decider_size;
pub mod fixed_schedule_trigger;
pub mod fixed_source;
pub mod force_trimmer;
pub mod full_width_replacer;
pub mod general_file_mover;
pub mod hardlink_file_mover;
pub mod holding_task_trigger;
pub mod http_downloader;
pub mod keyword_integration;
pub mod mapped_file_tagger;
pub mod never_replace_decider;
pub mod none_downloader;
pub mod regex_trimmer;
pub mod regex_variable_provider;
pub mod regex_variable_replacer;
pub mod run_command;
pub mod send_http_request;
pub mod sequence_variable_provider;
pub mod simple_file_exists_detector;
pub mod source_item_identity_filter;
pub mod system_file_mover;
pub mod system_file_resolver;
pub mod system_file_source;
pub mod touch_item_directory;
pub mod uri_source;
pub mod url_downloader;
pub mod url_file_resolver;
pub mod webhook_trigger;
pub mod windows_path_replacer;

#[allow(dead_code)]
pub fn get_build_in_component_supplier(
    component_manager: &Arc<ComponentManager>,
) -> Vec<Arc<dyn ComponentSupplier>> {
    vec![
        Arc::new(fixed_schedule_trigger::SUPPLIER),
        Arc::new(expression_item_filter::SUPPLIER),
        Arc::new(expression_item_content_filter::SUPPLIER),
        Arc::new(expression_file_content_filter::SUPPLIER),
        Arc::new(never_replace_decider::SUPPLIER),
        Arc::new(file_replacement_decider_always::SUPPLIER),
        Arc::new(file_replacement_decider_size::SUPPLIER),
        Arc::new(system_file_source::SUPPLIER),
        Arc::new(system_file_resolver::SUPPLIER),
        Arc::new(http_downloader::SUPPLIER),
        Arc::new(system_file_mover::SUPPLIER),
        Arc::new(mapped_file_tagger::SUPPLIER),
        Arc::new(general_file_mover::SUPPLIER),
        Arc::new(hardlink_file_mover::SUPPLIER),
        Arc::new(file_directory_exists_detector::SUPPLIER),
        Arc::new(none_downloader::SUPPLIER),
        Arc::new(sequence_variable_provider::SUPPLIER),
        Arc::new(regex_variable_provider::SUPPLIER),
        Arc::new(simple_file_exists_detector::SUPPLIER),
        Arc::new(force_trimmer::SUPPLIER),
        Arc::new(regex_trimmer::SUPPLIER),
        Arc::new(full_width_replacer::SUPPLIER),
        Arc::new(regex_variable_replacer::SUPPLIER),
        Arc::new(windows_path_replacer::SUPPLIER),
        Arc::new(delete_empty_directory::SUPPLIER),
        Arc::new(cron_trigger::SUPPLIER),
        Arc::new(fixed_source::SUPPLIER),
        Arc::new(keyword_integration::SUPPLIER),
        Arc::new(run_command::SUPPLIER),
        Arc::new(send_http_request::SUPPLIER),
        Arc::new(touch_item_directory::SUPPLIER),
        Arc::new(uri_source::SUPPLIER),
        Arc::new(url_downloader::SUPPLIER),
        Arc::new(url_file_resolver::SUPPLIER),
        Arc::new(webhook_trigger::SUPPLIER),
        Arc::new(composites::CompositeDownloaderSupplier::new(component_manager)),
        Arc::new(composites::CompositeItemFileResolverSupplier::new(component_manager)),
    ]
}

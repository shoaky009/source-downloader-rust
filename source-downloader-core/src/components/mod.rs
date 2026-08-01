use source_downloader_sdk::component::ComponentSupplier;
use std::sync::Arc;

pub mod expression_file_content_filter;
pub mod expression_item_content_filter;
pub mod expression_item_filter;
pub mod file_replacement_deciders;
pub mod mapped_file_tagger;
pub mod file_directory_exists_detector;
pub mod fixed_schedule_trigger;
pub mod holding_task_trigger;
pub mod http_downloader;
pub mod never_replace_decider;
pub mod simple_file_exists_detector;
pub mod source_item_identity_filter;
pub mod system_file_mover;
pub mod system_file_resolver;
pub mod system_file_source;
pub mod trimmers;
pub mod variable_replacers;

#[allow(dead_code)]
pub fn get_build_in_component_supplier() -> Vec<Arc<dyn ComponentSupplier>> {
    vec![
        Arc::new(fixed_schedule_trigger::SUPPLIER),
        Arc::new(expression_item_filter::SUPPLIER),
        Arc::new(expression_item_content_filter::SUPPLIER),
        Arc::new(expression_file_content_filter::SUPPLIER),
        Arc::new(never_replace_decider::SUPPLIER),
        Arc::new(file_replacement_deciders::ALWAYS_SUPPLIER),
        Arc::new(file_replacement_deciders::SIZE_SUPPLIER),
        Arc::new(system_file_source::SUPPLIER),
        Arc::new(system_file_resolver::SUPPLIER),
        Arc::new(http_downloader::SUPPLIER),
        Arc::new(system_file_mover::SUPPLIER),
        Arc::new(mapped_file_tagger::SUPPLIER),
        Arc::new(file_directory_exists_detector::SUPPLIER),
        Arc::new(simple_file_exists_detector::SUPPLIER),
        Arc::new(trimmers::FORCE_SUPPLIER),
        Arc::new(trimmers::REGEX_SUPPLIER),
        Arc::new(variable_replacers::FULL_WIDTH_SUPPLIER),
        Arc::new(variable_replacers::REGEX_SUPPLIER),
        Arc::new(variable_replacers::WINDOWS_PATH_SUPPLIER),
    ]
}

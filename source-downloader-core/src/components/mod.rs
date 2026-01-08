use source_downloader_sdk::component::ComponentSupplier;
use std::sync::Arc;

pub mod expression_item_filter;
pub mod fixed_schedule_trigger;
pub mod system_file_source;
pub mod system_file_resolver;
pub mod http_downloader;
pub mod system_file_mover;
pub mod source_item_identity_filter;
pub mod expression_file_content_filter;
pub mod expression_item_content_filter;

#[allow(dead_code)]
pub fn get_build_in_component_supplier() -> Vec<Arc<dyn ComponentSupplier>> {
    vec![
        Arc::new(fixed_schedule_trigger::SUPPLIER),
        Arc::new(expression_item_filter::SUPPLIER),
        Arc::new(expression_file_content_filter::SUPPLIER),
        Arc::new(system_file_source::SUPPLIER),
        Arc::new(system_file_resolver::SUPPLIER),
        Arc::new(http_downloader::SUPPLIER),
        Arc::new(system_file_mover::SUPPLIER),
    ]
}

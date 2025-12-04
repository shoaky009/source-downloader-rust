use crate::components::expression_item_filter::ExpressionItemFilterSupplier;
use crate::components::fixed_schedule_trigger::FixedScheduleTriggerSupplier;
use sdk::component::ComponentSupplier;
use std::sync::Arc;

pub mod expression_item_filter;
pub mod fixed_schedule_trigger;
pub mod system_file_source;

#[allow(dead_code)]
pub fn get_build_in_component_supplier() -> Vec<Arc<dyn ComponentSupplier>> {
    vec![
        Arc::new(FixedScheduleTriggerSupplier {}),
        Arc::new(ExpressionItemFilterSupplier {}),
        Arc::new(system_file_source::SystemFileSourceSupplier {}),
    ]
}

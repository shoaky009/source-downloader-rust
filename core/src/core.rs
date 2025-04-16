use crate::ComponentManager;
use crate::components::system_file_source::SystemFileSourceSupplier;
use std::sync::Arc;

pub struct CoreApplication {
    pub component_manager: ComponentManager,
}

impl CoreApplication {

    pub fn start(&mut self) {
        self.register_component()
    }

    fn register_component(&mut self) {
        self.component_manager
            .register(Arc::new(SystemFileSourceSupplier {}))
            .unwrap();
    }
}

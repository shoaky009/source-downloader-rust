use sdk::component::{ComponentError, ComponentSupplier, ComponentType};
use std::collections::HashMap;

pub struct ComponentManager {
    component_suppliers: HashMap<ComponentType, Box<dyn ComponentSupplier>>,
}

impl ComponentManager {
    pub fn new() -> Self {
        ComponentManager {
            component_suppliers: HashMap::new(),
        }
    }

    pub fn register(
        &mut self,
        supplier: Box<dyn ComponentSupplier>,
    ) -> Result<bool, ComponentError> {
        let component_types = supplier.supply_types();
        for component_type in component_types {
            if self.component_suppliers.contains_key(&component_type) {
                return Err(ComponentError::from(format!(
                    "Component type {:?} already registered",
                    component_type
                )));
            }
            // self.component_suppliers
            //     .insert(component_type, supplier.clone());
        }
        Ok(true)
    }
}

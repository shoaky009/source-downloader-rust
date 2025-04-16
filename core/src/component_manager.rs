#![allow(dead_code)]

use crate::ObjectWrapperContainer;
use sdk::component::{ComponentError, ComponentSupplier, ComponentType};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct ComponentManager {
    component_suppliers: HashMap<ComponentType, Arc<dyn ComponentSupplier>>,
    object_container: Box<ObjectWrapperContainer>,
}

impl Display for ComponentManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        use std::collections::HashMap;

        let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
        for (component_type, _) in &self.component_suppliers {
            let root_type = component_type.root_type.name();
            let type_name = &component_type.name;
            grouped
                .entry(root_type)
                .or_insert_with(Vec::new)
                .push(type_name);
        }

        writeln!(
            f,
            "ComponentManager registered {} component suppliers:",
            self.component_suppliers.len()
        )?;
        for (key, values) in &grouped {
            writeln!(f, "{}: [{}]", key, values.join(", "))?;
        }
        Ok(())
    }
}

impl ComponentManager {
    pub fn new(object_container: Box<ObjectWrapperContainer>) -> Self {
        ComponentManager {
            component_suppliers: HashMap::new(),
            object_container,
        }
    }

    pub fn register(
        &mut self,
        supplier: Arc<dyn ComponentSupplier>,
    ) -> Result<bool, ComponentError> {
        let component_types = supplier.supply_types();
        for component_type in component_types {
            if self.component_suppliers.contains_key(&component_type) {
                return Err(ComponentError::from(format!(
                    "Component type {:?} already registered",
                    component_type
                )));
            }
            self.component_suppliers
                .insert(component_type, supplier.clone());
        }
        Ok(true)
    }

    pub fn register_suppliers(
        &mut self,
        suppliers: Vec<Arc<dyn ComponentSupplier>>,
    ) -> Result<bool, ComponentError> {
        for supplier in suppliers {
            self.register(supplier)?;
        }
        Ok(true)
    }
}

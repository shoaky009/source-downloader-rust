#![allow(dead_code)]

use crate::config::{ConfigOperator, Properties};
use sdk::component::{ComponentError, ComponentSupplier, ComponentType, SdComponent};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, RwLock};

pub struct ComponentManager {
    config_operator: Arc<dyn ConfigOperator>,
    component_suppliers: HashMap<ComponentType, Arc<dyn ComponentSupplier>>,
    wrappers: RwLock<HashMap<String, ComponentWrapper>>,
}

impl Display for ComponentManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
        for (component_type, _) in &self.component_suppliers {
            grouped
                .entry(&component_type.root_type.name())
                .or_insert_with(Vec::new)
                .push(&component_type.name);
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
    pub fn new(config_operator: Arc<dyn ConfigOperator>) -> Self {
        Self {
            config_operator,
            component_suppliers: HashMap::new(),
            wrappers: RwLock::new(HashMap::new()),
        }
    }

    fn get_instance_name(type_: &ComponentType, name: &str) -> String {
        format!("{}:{}:{}", type_.root_type.name(), type_.name, name)
    }

    pub fn register_supplier(
        &mut self,
        supplier: Arc<dyn ComponentSupplier>,
    ) -> Result<bool, ComponentError> {
        let component_types = supplier.supply_types();
        for component_type in component_types {
            if self.component_suppliers.contains_key(&component_type) {
                return Err(ComponentError::new(format!(
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
            self.register_supplier(supplier)?;
        }
        Ok(true)
    }

    pub fn get_component(
        &self,
        type_: &ComponentType,
        name: &str,
    ) -> Result<ComponentWrapper, ComponentError> {
        let instance_name = Self::get_instance_name(type_, name);

        {
            let guard = self.wrappers.read().unwrap();
            if let Some(wrapper) = guard.get(&instance_name) {
                return Ok(wrapper.clone());
            }
        }

        let supplier = self.component_suppliers.get(type_).ok_or_else(|| {
            ComponentError::new(format!("Supplier not found for type: {}", type_))
        })?;

        let types = supplier.supply_types();
        let (pk_type, props) =
            self.get_component_props(&types, name, supplier.is_support_no_props());

        let (component, creation_error) = match supplier.apply(props.inner) {
            Ok(c) => (Some(c), None),
            Err(e) => {
                eprintln!("Failed to create component {}: {}", instance_name, e);
                (None, Some(e))
            }
        };

        let mut guard = self.wrappers.write().unwrap();

        // Double-Check Locking
        if let Some(existing) = guard.get(&instance_name) {
            return Ok(existing.clone());
        }

        let error_message = creation_error.map(|e| e.message);
        let mut target_wrapper: Option<ComponentWrapper> = None;

        for x in &types {
            let wrapper = ComponentWrapper {
                component_type: x.clone(),
                name: name.to_string(),
                component: component.clone(),
                primary: x == &pk_type,
                creation_error: error_message.to_owned(),
            };

            let key = Self::get_instance_name(&wrapper.component_type, &wrapper.name);

            // 对应 C# TryAdd: 如果 key 已存在则报错 (或者你可以选择忽略)
            if guard.contains_key(&key) {
                return Err(ComponentError::new(format!(
                    "组件实例 '{}' 已经存在 (Race condition hit)",
                    key
                )));
            }

            guard.insert(key, wrapper.clone());

            if x == type_ {
                target_wrapper = Some(wrapper);
            }
        }

        target_wrapper
            .ok_or_else(|| ComponentError::new(format!("未找到类型为 '{}' 的组件", type_)))
    }

    fn get_component_props(
        &self,
        types: &[ComponentType],
        name: &str,
        _allow_no_args: bool,
    ) -> (ComponentType, Properties) {
        let empty_props = Properties::new();
        for component_type in types {
            if let Some(props) = self
                .config_operator
                .get_component_config(component_type, name)
            {
                return (component_type.clone(), Properties::from_map(props.props));
            }
        }
        (types[0].clone(), empty_props)
    }

    pub fn destroy(&self, type_: &ComponentType, name: &str) {
        let instance_name = Self::get_instance_name(type_, name);
        let mut guard = self.wrappers.write().unwrap();
        if guard.remove(&instance_name).is_none() {
            return;
        }

        if let Some(supplier) = self.component_suppliers.get(type_) {
            for other_type in supplier.supply_types() {
                if &other_type != type_ {
                    let key = Self::get_instance_name(&other_type, name);
                    guard.remove(&key);
                }
            }
        }
    }

    pub fn get_all_suppliers(&self) -> Result<Vec<Arc<dyn ComponentSupplier>>, ComponentError> {
        let mut suppliers = Vec::new();
        for (_, supplier) in &self.component_suppliers {
            suppliers.push(supplier.clone());
        }
        Ok(suppliers)
    }

    pub fn destroy_all(&self) {
        let mut guard = self.wrappers.write().unwrap();
        guard.clear();
    }
}

#[derive(Debug, Clone)]
pub struct ComponentWrapper {
    pub component_type: ComponentType,
    pub name: String,
    // 使用 Arc 因为一个实例可能对应多个 Wrapper (多个 Interface)
    pub component: Option<Arc<dyn SdComponent>>,
    pub primary: bool,
    pub creation_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use crate::ComponentManager;
    use crate::components::system_file_source::SystemFileSourceSupplier;
    use crate::config::YamlConfigOperator;
    use sdk::component::ComponentType;
    use std::sync::Arc;

    #[test]
    fn test() {
        let mut manager = ComponentManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.unwrap());

        let component_type = &ComponentType::source("system-file".to_string());
        let component_wrapper = manager.get_component(component_type, "test").unwrap();
        let source = component_wrapper
            .component
            .as_ref()
            .unwrap()
            .as_source()
            .unwrap();
        assert_eq!(component_wrapper.name, "test");
        let items = source.fetch();
        assert!(items.len() > 0);
        println!("{:?}", items)
    }
}

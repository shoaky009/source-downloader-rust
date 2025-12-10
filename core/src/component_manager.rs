#![allow(dead_code)]

use crate::config::{ConfigOperator, Properties};
use parking_lot::RwLock;
use sdk::component::{
    ComponentError, ComponentId, ComponentSupplier, ComponentType, SdComponent, Trigger,
};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use tracing::info;

pub struct ComponentManager {
    config_operator: Arc<dyn ConfigOperator>,
    component_suppliers: RwLock<HashMap<ComponentType, Arc<dyn ComponentSupplier>>>,
    component_wrappers: RwLock<HashMap<String, Arc<ComponentWrapper>>>,
}

impl Display for ComponentManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut grouped: HashMap<&str, Vec<&str>> = HashMap::new();
        let guard = self.component_suppliers.read();
        for component_type in guard.keys() {
            grouped
                .entry(component_type.root_type.name())
                .or_default()
                .push(&component_type.name);
        }

        writeln!(
            f,
            "ComponentManager registered {} component suppliers:",
            self.component_suppliers.read().len()
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
            component_suppliers: RwLock::new(HashMap::new()),
            component_wrappers: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_supplier(
        &self,
        supplier: Arc<dyn ComponentSupplier>,
    ) -> Result<bool, ComponentError> {
        let component_types = supplier.supply_types();
        for component_type in component_types {
            if self
                .component_suppliers
                .read()
                .contains_key(&component_type)
            {
                return Err(ComponentError::new(format!(
                    "Component type {:?} already registered",
                    component_type
                )));
            }
            self.component_suppliers
                .write()
                .insert(component_type, supplier.clone());
        }
        Ok(true)
    }

    pub fn register_suppliers(
        &self,
        suppliers: Vec<Arc<dyn ComponentSupplier>>,
    ) -> Result<bool, ComponentError> {
        for supplier in suppliers {
            self.register_supplier(supplier)?;
        }
        Ok(true)
    }

    pub fn get_component(&self, id: &ComponentId) -> Result<Arc<ComponentWrapper>, ComponentError> {
        let instance_name = id.display();

        {
            let guard = self.component_wrappers.read();
            if let Some(wrapper) = guard.get(&instance_name) {
                return Ok(wrapper.clone());
            }
        }

        let guard = self.component_suppliers.read();
        let component_type = &id.component_type;
        let name = &id.name;
        let supplier = guard.get(component_type).ok_or_else(|| {
            ComponentError::new(format!("Supplier not found for type: {}", component_type))
        })?;

        let types = supplier.supply_types();
        let (pk_type, props) =
            self.get_component_props(&types, name, supplier.is_support_no_props())?;

        let (component, creation_error) = match supplier.apply(&props.inner) {
            Ok(c) => (Some(c), None),
            Err(e) => {
                eprintln!("Failed to create component {}: {}", instance_name, e);
                (None, Some(e))
            }
        };

        let mut guard = self.component_wrappers.write();
        if let Some(existing) = guard.get(&instance_name) {
            return Ok(existing.clone());
        }

        let error_message = creation_error.map(|e| e.message);
        let mut target_wrapper: Option<Arc<ComponentWrapper>> = None;

        for x in &types {
            let wrapper = Arc::new(ComponentWrapper {
                id: ComponentId::new(x.clone(), name),
                component: component.clone(),
                primary: x == &pk_type,
                creation_error: error_message.to_owned(),
                processor_ref: RwLock::new(HashSet::new()),
            });

            let key = wrapper.id.display();
            if guard.contains_key(&key) {
                return Err(ComponentError::new(format!(
                    "组件实例 '{}' 已经存在 (Race condition hit)",
                    key
                )));
            }
            info!("Component[created] {}", instance_name);
            guard.insert(key, wrapper.clone());

            if x == component_type {
                target_wrapper = Some(wrapper);
            }
        }

        target_wrapper
            .ok_or_else(|| ComponentError::new(format!("未找到类型为 '{}' 的组件", component_type)))
    }

    fn get_component_props(
        &self,
        types: &[ComponentType],
        name: &str,
        allow_no_args: bool,
    ) -> Result<(ComponentType, Properties), ComponentError> {
        if types.is_empty() {
            return Err(ComponentError::new(
                "没有任何可用的 ComponentType (types list is empty)".to_string(),
            ));
        }

        for component_type in types {
            if let Some(config) = self
                .config_operator
                .get_component_config(component_type, name)
                .filter(|c| c.name == name)
            {
                return Ok((component_type.clone(), Properties::from_map(config.props)));
            }
        }

        if allow_no_args {
            return Ok((types[0].clone(), Properties::new()));
        }

        Err(ComponentError::new(format!(
            "Component config not found types {:?} name:{}",
            types
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(","),
            name,
        )))
    }

    pub fn destroy(&self, id: &ComponentId) {
        let instance_name = id.display();
        let mut guard = self.component_wrappers.write();
        if guard.remove(&instance_name).is_none() {
            return;
        }

        let type_ = &id.component_type;
        if let Some(supplier) = self.component_suppliers.read().get(type_) {
            for other_type in supplier.supply_types() {
                if &other_type != type_ {
                    let key = ComponentId::new(other_type, &id.name).display();
                    guard.remove(&key);
                }
            }
        }
    }

    pub fn get_all_suppliers(&self) -> Result<Vec<Arc<dyn ComponentSupplier>>, ComponentError> {
        let mut suppliers = Vec::new();
        for supplier in self.component_suppliers.read().values() {
            suppliers.push(supplier.clone());
        }
        Ok(suppliers)
    }

    pub fn destroy_all(&self) {
        let mut guard = self.component_wrappers.write();
        guard.clear();
    }

    pub fn get_all_component(&self) -> Vec<Arc<ComponentWrapper>> {
        self.component_wrappers.read().values().cloned().collect()
    }

    pub fn for_each_trigger<F>(&self, mut f: F)
    where
        F: FnMut(&ComponentWrapper, Arc<dyn Trigger>),
    {
        let wrappers = self.component_wrappers.read();
        for wrapper in wrappers.values() {
            let c = match wrapper.component.as_ref() {
                Some(c) => c,
                None => continue,
            };
            let trigger = match c.clone().as_trigger() {
                Ok(t) => t,
                Err(_) => continue,
            };
            f(wrapper, trigger);
        }
    }

    pub fn get_all_trigger(&self) -> Vec<Arc<dyn Trigger>> {
        self.component_wrappers
            .read()
            .values()
            .filter_map(|x| x.component.as_ref().map(|c| c.clone().as_trigger()))
            .flatten()
            .collect()
    }
}

#[derive(Debug)]
pub struct ComponentWrapper {
    pub id: ComponentId,
    pub component: Option<Arc<dyn SdComponent>>,
    pub primary: bool,
    pub creation_error: Option<String>,
    processor_ref: RwLock<HashSet<String>>,
}

impl ComponentWrapper {
    pub fn get_component(&self) -> Result<Arc<dyn SdComponent>, ComponentError> {
        if self.component.is_some() {
            return Ok(self.component.as_ref().unwrap().clone());
        }
        Err(ComponentError::new(
            self.creation_error
                .clone()
                .unwrap_or_else(|| format!("Component {} not created", self.id.display())),
        ))
    }

    pub fn get_and_mark_ref(&self, processor_name: &str) -> Option<Arc<dyn SdComponent>> {
        self.processor_ref
            .write()
            .insert(processor_name.to_string());
        self.component.clone()
    }

    pub fn remove_ref(&self, processor_name: &str) {
        self.processor_ref.write().remove(processor_name);
    }
}

#[cfg(test)]
mod tests {
    use crate::ComponentManager;
    use crate::components::system_file_source::SystemFileSourceSupplier;
    use crate::config::{ConfigOperator, YamlConfigOperator};
    use sdk::Map;
    use sdk::component::{ComponentRootType, ComponentSupplier};
    use std::sync::{Arc, OnceLock};

    static CONFIG_OP: OnceLock<Arc<dyn ConfigOperator>> = OnceLock::new();
    fn get_config_op() -> &'static Arc<dyn ConfigOperator> {
        CONFIG_OP.get_or_init(|| Arc::new(YamlConfigOperator::new("./tests/resources/config.yaml")))
    }
    // 预期一切正常
    #[test]
    fn normal_case() {
        let manager = ComponentManager::new(get_config_op().clone());
        // register supplier case
        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.unwrap());

        // get component and downcast case
        let id = &ComponentRootType::Source.parse_component_id("system-file:test");
        let component_wrapper = manager.get_component(id).unwrap();
        let component_arc = component_wrapper.component.as_ref().unwrap();
        let source = component_arc.clone().as_source().unwrap();
        assert_eq!(component_wrapper.id.name, "test");
        let items = source.fetch(&Map::new());
        assert!(items.len() > 0);
        println!("{:?}", items);

        // multiple time get a component case, the component should be the same instance
        let component_wp2 = manager.get_component(id).unwrap();
        assert!(Arc::ptr_eq(
            &component_arc,
            &component_wp2.component.as_ref().unwrap()
        ));

        // to destroy a component case, the component should be recreated so that the instance is different
        manager.destroy(id);
        let component_wp3 = manager.get_component(id).unwrap();
        assert!(!Arc::ptr_eq(
            &component_arc,
            &component_wp3.component.as_ref().unwrap()
        ));
    }

    #[test]
    fn duplicate_registration_case() {
        let manager = ComponentManager::new(get_config_op().clone());

        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.unwrap());

        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.is_err());
    }

    #[test]
    fn get_all_suppliers_case() {
        let manager = ComponentManager::new(get_config_op().clone());
        let arc: Arc<dyn ComponentSupplier> = Arc::new(SystemFileSourceSupplier {});
        manager.register_supplier(arc.clone()).unwrap();
        let suppliers = manager.get_all_suppliers().unwrap();
        assert_eq!(suppliers.len(), 1);
        assert!(Arc::ptr_eq(suppliers.first().unwrap(), &arc));
    }

    #[test]
    fn get_component_error_case() {
        let manager = ComponentManager::new(get_config_op().clone());
        let id = &ComponentRootType::Source.parse_component_id("system-file:test2");
        let result = manager.get_component(id);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.message.starts_with("Supplier not found for type:"));

        manager
            .register_supplier(Arc::new(SystemFileSourceSupplier {}))
            .unwrap();

        let result2 = manager.get_component(id);
        assert!(result2.is_err());
        let error2 = result2.unwrap_err();
        assert!(
            error2
                .message
                .starts_with("Component config not found types")
        );
    }
}

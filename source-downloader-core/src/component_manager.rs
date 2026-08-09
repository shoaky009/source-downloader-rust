#![allow(dead_code)]

use crate::config::{ConfigOperator, Properties};
use parking_lot::RwLock;
use source_downloader_sdk::component::{
    ComponentCompatibilityRule, ComponentCreateContext, ComponentError, ComponentId,
    ComponentRootType, ComponentSupplier, ComponentType, EMPTY_COMPONENT_CREATE_CONTEXT,
    SdComponent, Trigger,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use tracing::{debug, info};

pub struct ComponentManager {
    config_operator: Arc<dyn ConfigOperator>,
    create_context: Arc<dyn ComponentCreateContext>,
    component_suppliers: RwLock<HashMap<ComponentType, Arc<dyn ComponentSupplier>>>,
    compatibility_rules:
        RwLock<HashMap<ComponentType, Arc<[ComponentCompatibilityRule]>>>,
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
        Self::with_create_context(
            config_operator,
            Arc::new(EMPTY_COMPONENT_CREATE_CONTEXT),
        )
    }

    pub fn with_create_context(
        config_operator: Arc<dyn ConfigOperator>,
        create_context: Arc<dyn ComponentCreateContext>,
    ) -> Self {
        Self {
            config_operator,
            create_context,
            component_suppliers: RwLock::new(HashMap::new()),
            compatibility_rules: RwLock::new(HashMap::new()),
            component_wrappers: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_supplier(
        &self,
        supplier: Arc<dyn ComponentSupplier>,
    ) -> Result<bool, ComponentError> {
        let component_types = supplier.supply_types();
        let mut rules_by_owner = HashMap::<_, Vec<_>>::new();
        for rule in supplier.compatibility_rules() {
            if !component_types.contains(&rule.owner) {
                return Err(ComponentError::new(format!(
                    "Compatibility rule '{}' declares unsupported owner type '{}'",
                    rule.code, rule.owner
                )));
            }
            rules_by_owner.entry(rule.owner.clone()).or_default().push(rule);
        }

        let mut suppliers = self.component_suppliers.write();
        for component_type in &component_types {
            if suppliers.contains_key(component_type) {
                return Err(ComponentError::new(format!(
                    "Component type {:?} already registered",
                    component_type
                )));
            }
        }
        for component_type in component_types {
            suppliers.insert(component_type, supplier.clone());
        }
        drop(suppliers);

        let mut compatibility_rules = self.compatibility_rules.write();
        for (owner, rules) in rules_by_owner {
            compatibility_rules.insert(owner, Arc::from(rules));
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

    pub fn get_component(
        &self,
        id: &ComponentId,
    ) -> Result<Arc<ComponentWrapper>, ComponentError> {
        let instance_name = id.display();

        {
            let guard = self.component_wrappers.read();
            if let Some(wrapper) = guard.get(&instance_name) {
                return Ok(wrapper.clone());
            }
        }

        let component_type = &id.component_type;
        let name = &id.name;
        let supplier =
            self.component_suppliers.read().get(component_type).cloned().ok_or_else(
                || {
                    ComponentError::new(format!(
                        "Supplier not found for type: {}",
                        component_type
                    ))
                },
            )?;

        let types = supplier.supply_types();
        let (pk_type, props) =
            self.get_component_props(&types, name, supplier.is_support_no_props())?;
        let (component, creation_error) =
            match supplier.apply(self.create_context.as_ref(), &props.inner) {
                Ok(c) => {
                    info!("Component[created] {instance_name}");
                    (Some(c), None)
                }
                Err(error) => {
                    let error = ComponentError::new(format!(
                        "Component '{}' creation failed (type={}, name={}): {}",
                        instance_name, component_type, name, error
                    ));
                    tracing::error!("Component[create-failed] {instance_name}: {error}");
                    (None, Some(error))
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
                processor_refs: RwLock::new(HashSet::new()),
            });

            let key = wrapper.id.display();
            if guard.contains_key(&key) {
                return Err(ComponentError::new(format!(
                    "组件实例 '{}' 已经存在 (Race condition hit)",
                    key
                )));
            }
            debug!("Component[share] {}", key);
            guard.insert(key, wrapper.clone());

            if x == component_type {
                target_wrapper = Some(wrapper);
            }
        }

        target_wrapper.ok_or_else(|| {
            ComponentError::new(format!("未找到类型为 '{}' 的组件", component_type))
        })
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
            "Component config not found, types {:?} name:{}",
            types.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","),
            name,
        )))
    }

    pub fn destroy(&self, id: &ComponentId) -> Option<Arc<ComponentWrapper>> {
        let instance_name = id.display();
        let mut guard = self.component_wrappers.write();
        let removed = guard.remove(&instance_name);
        if removed.is_none() {
            return removed;
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
        removed
    }

    pub fn get_all_suppliers(&self) -> Vec<Arc<dyn ComponentSupplier>> {
        let mut suppliers = Vec::new();
        for supplier in self.component_suppliers.read().values() {
            if !suppliers.iter().any(|known| Arc::ptr_eq(known, supplier)) {
                suppliers.push(supplier.clone());
            }
        }
        suppliers
    }

    pub fn get_supplier(
        &self,
        component_type: &ComponentType,
    ) -> Option<Arc<dyn ComponentSupplier>> {
        self.component_suppliers.read().get(component_type).cloned()
    }

    pub fn get_compatibility_rules(
        &self,
        component_type: &ComponentType,
    ) -> Option<Arc<[ComponentCompatibilityRule]>> {
        self.compatibility_rules.read().get(component_type).cloned()
    }

    pub fn get_all_compatibility_rules(&self) -> Vec<ComponentCompatibilityRule> {
        self.compatibility_rules
            .read()
            .values()
            .flat_map(|rules| rules.iter().cloned())
            .collect()
    }

    pub fn destroy_all(&self) {
        let mut guard = self.component_wrappers.write();
        guard.clear();
    }

    pub fn get_all_component(&self) -> Vec<Arc<ComponentWrapper>> {
        self.component_wrappers.read().values().cloned().collect()
    }

    pub fn remove_processor_refs(&self, processor_name: &str) {
        for wrapper in self.component_wrappers.read().values() {
            wrapper.remove_ref(processor_name);
        }
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
    processor_refs: RwLock<HashSet<String>>,
}

impl ComponentWrapper {
    pub fn require_component(&self) -> Result<Arc<dyn SdComponent>, ComponentError> {
        if let Some(component) = &self.component {
            return Ok(component.clone());
        }
        Err(ComponentError::new(
            self.creation_error.clone().unwrap_or_else(|| {
                format!("Component {} not created", self.id.display())
            }),
        ))
    }

    pub fn require_and_mark_ref(
        &self,
        processor_name: &str,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let component = self.require_component()?;
        self.processor_refs.write().insert(processor_name.to_owned());
        Ok(component)
    }

    pub fn remove_ref(&self, processor_name: &str) {
        self.processor_refs.write().remove(processor_name);
    }

    pub fn get_refs(&self) -> HashSet<String> {
        self.processor_refs.read().clone()
    }
}

impl Drop for ComponentWrapper {
    fn drop(&mut self) {
        debug!("Component[drop] {}", self.id.display());
    }
}

pub struct ComponentQuery {
    pub root_type: Option<ComponentRootType>,
    pub type_name: Option<String>,
    pub name: Option<String>,
}

pub struct ComponentInfo {
    pub component_root_type: ComponentRootType,
    pub type_name: String,
    pub name: String,
    pub props: Map<String, Value>,
    pub state_detail: Option<Map<String, Value>>,
    pub primary: bool,
    pub running: bool,
    pub refs: HashSet<String>,
    pub modifiable: bool,
    pub error_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use crate::component_manager::ComponentManager;
    use crate::components::system_file_source::SystemFileSourceSupplier;
    use crate::config::{ConfigOperator, YamlConfigOperator};
    use source_downloader_sdk::component::{ComponentRootType, ComponentSupplier};
    use std::sync::{Arc, LazyLock};

    static CONFIG_OP: LazyLock<Arc<dyn ConfigOperator>> = LazyLock::new(|| {
        Arc::new(YamlConfigOperator::new("./tests/resources/config.yaml"))
    });
    // 预期一切正常
    #[tokio::test]
    async fn normal_case() {
        let manager = ComponentManager::new(CONFIG_OP.clone());
        // register supplier case
        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.unwrap());

        // get component and downcast case
        let id = &ComponentRootType::Source.parse_component_id("system-file:test");
        let component_wrapper = manager.get_component(id).unwrap();
        let component_arc = component_wrapper.component.as_ref().unwrap();
        let _ = component_arc.clone().as_source().unwrap();
        assert_eq!(component_wrapper.id.name, "test");

        // multiple time get a component case, the component should be the same instance
        let component_wp2 = manager.get_component(id).unwrap();
        assert!(Arc::ptr_eq(component_arc, component_wp2.component.as_ref().unwrap()));

        // to destroy a component case, the component should be recreated so that the instance is different
        manager.destroy(id);
        let component_wp3 = manager.get_component(id).unwrap();
        assert!(!Arc::ptr_eq(component_arc, component_wp3.component.as_ref().unwrap()));
    }

    #[test]
    fn duplicate_registration_case() {
        let manager = ComponentManager::new(CONFIG_OP.clone());

        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.unwrap());

        let result = manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        assert!(result.is_err());
    }

    #[test]
    fn get_all_suppliers_case() {
        let manager = ComponentManager::new(CONFIG_OP.clone());
        let arc: Arc<dyn ComponentSupplier> = Arc::new(SystemFileSourceSupplier {});
        manager.register_supplier(arc.clone()).unwrap();
        let suppliers = manager.get_all_suppliers();
        assert_eq!(suppliers.len(), 1);
        assert!(Arc::ptr_eq(suppliers.first().unwrap(), &arc));
    }

    #[test]
    fn get_component_error_case() {
        let manager = ComponentManager::new(CONFIG_OP.clone());
        let id = &ComponentRootType::Source.parse_component_id("system-file:test2");
        let result = manager.get_component(id);
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.message.starts_with("Supplier not found for type:"));

        manager.register_supplier(Arc::new(SystemFileSourceSupplier {})).unwrap();

        let result2 = manager.get_component(id);
        assert!(result2.is_err());
        let error2 = result2.unwrap_err();
        assert!(error2.message.starts_with("Component config not found"));
    }
}

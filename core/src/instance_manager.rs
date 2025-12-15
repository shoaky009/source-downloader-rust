use crate::config::{ConfigOperator, Properties};
use parking_lot::RwLock;
use sdk::component::ComponentError;
use sdk::instance::InstanceFactory;
use std::any::{Any, TypeId, type_name};
use std::collections::HashMap;
use std::sync::Arc;

pub struct InstanceManager {
    config_operator: Arc<dyn ConfigOperator>,
    factories: RwLock<HashMap<TypeId, Arc<dyn InstanceFactory>>>,
    instances: RwLock<HashMap<String, Arc<dyn Any + Send + Sync>>>,
}

impl InstanceManager {
    pub fn new(config_operator: Arc<dyn ConfigOperator>) -> Self {
        Self {
            config_operator,
            factories: RwLock::new(HashMap::new()),
            instances: RwLock::new(HashMap::new()),
        }
    }

    pub fn load_instance<T: Send + Sync + 'static>(
        &self,
        name: &str,
        props: Option<Properties>,
    ) -> Result<Arc<T>, String> {
        let mut instances = self.instances.write();
        if let Some(instance_any) = instances.get(name) {
            return instance_any
                .clone()
                .downcast::<T>()
                .map_err(|_| format!("Instance '{}' exists but type mismatch", name));
        }
        let request_type_id = TypeId::of::<T>();
        let factory_guard = self.factories.read();
        let factory = factory_guard.get(&request_type_id).ok_or_else(|| {
            format!(
                "No factory found for typeId {:?} name:{:?}",
                request_type_id,
                type_name::<T>()
            )
        })?;

        let final_props = match props {
            Some(p) => p,
            None => self
                .config_operator
                .get_instance_props(name.to_string())
                .map_err(|e| e.message)?,
        };

        let new_instance = factory
            .create_instance(&final_props.inner)
            .map_err(|e| e.message)?;
        let instance_type_id = (*new_instance).type_id();
        if request_type_id != instance_type_id {
            let factory_type_name = factory.factory_name();
            let request_type_name = type_name::<T>();
            return Err(format!(
                "Factory implementation error: factory `{}` declared output {} {:?}, but actually created instance of type {:?}.",
                factory_type_name, request_type_name, request_type_id, instance_type_id,
            ));
        }

        let instance_any = instances.entry(name.to_string()).or_insert(new_instance);
        instance_any
            .clone()
            .downcast::<T>()
            .map_err(|_| format!("Instance '{}' exists but type mismatch", name))
    }

    pub fn destroy_instance(&self, name: &str) {
        self.instances.write().remove(name);
    }

    pub fn destroy_all_instances(&self) {
        self.instances.write().clear();
    }

    pub fn get_instances<T: Send + Sync + 'static>(&self) -> Vec<Arc<T>> {
        let target_type_id = TypeId::of::<T>();
        self.instances
            .read()
            .values()
            .filter_map(|any_arc| {
                if (**any_arc).type_id() == target_type_id {
                    Some(any_arc.clone().downcast::<T>().unwrap())
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn register_instance_factory(
        &self,
        factory: Arc<dyn InstanceFactory>,
    ) -> Result<bool, ComponentError> {
        let type_id = factory.instance_type_id();
        if self.factories.read().contains_key(&type_id) {
            return Err(ComponentError::new(format!(
                "Instance factory {:?} already registered",
                type_id
            )));
        }
        self.factories.write().insert(type_id, factory.clone());
        Ok(true)
    }
}

#[cfg(test)]
mod test {
    use crate::YamlConfigOperator;
    use crate::config::Properties;
    use crate::instance_manager::InstanceManager;
    use sdk::component::ComponentError;
    use sdk::instance::InstanceFactory;
    use sdk::serde_json::{Map, Value, from_str};
    use std::any::{Any, TypeId};
    use std::sync::Arc;

    #[test]
    fn normal_case() {
        let manager = InstanceManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let instance_name = "client1";
        assert!(
            manager
                .load_instance::<String>(instance_name, None)
                .is_err()
        );
        let _ = manager.register_instance_factory(Arc::new(ClientFactory {}));

        let vars: Map<String, Value> = from_str(r#"{"name": "hello"}"#).unwrap();
        let hello_value1 = manager
            .load_instance::<Client>(instance_name, Some(Properties::from_map(vars.clone())));
        assert!(hello_value1.is_ok());
        assert_eq!("hello", hello_value1.as_ref().unwrap().name);

        let hello_value2 = manager
            .load_instance::<Client>(instance_name, Some(Properties::from_map(vars.clone())));
        assert!(Arc::ptr_eq(
            &hello_value2.unwrap(),
            hello_value1.as_ref().unwrap()
        ));

        // get by type
        let instances = manager.get_instances::<Client>();
        assert_eq!(1, instances.len());

        // destroy instance
        manager.destroy_instance(instance_name);
        assert_eq!(0, manager.instances.read().len());

        // destroy all instances
        let _ = manager
            .load_instance::<Client>(instance_name, Some(Properties::from_map(vars.clone())));
        manager.destroy_all_instances();
        assert_eq!(0, manager.instances.read().len());
    }

    #[test]
    fn factory_error_case() {
        let manager = InstanceManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let _ = manager.register_instance_factory(Arc::new(ErrorImplFactory {}));
        let result = manager.load_instance::<String>("client1", None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .starts_with("Factory implementation error")
        );
    }

    struct ClientFactory {}
    impl InstanceFactory for ClientFactory {
        fn create_instance(
            &self,
            props: &Map<String, Value>,
        ) -> Result<Arc<dyn Any + Send + Sync>, ComponentError> {
            let name = props.get("name").unwrap().as_str().unwrap().to_string();
            Ok(Arc::new(Client { name }))
        }

        fn instance_type_id(&self) -> TypeId {
            TypeId::of::<Client>()
        }
    }

    struct Client {
        name: String,
    }
    impl Drop for Client {
        fn drop(&mut self) {
            println!("Client dropped");
        }
    }

    struct ErrorImplFactory {}
    impl InstanceFactory for ErrorImplFactory {
        fn create_instance(
            &self,
            _: &Map<String, Value>,
        ) -> Result<Arc<dyn Any + Send + Sync>, ComponentError> {
            Ok(Arc::new(1))
        }

        fn instance_type_id(&self) -> TypeId {
            TypeId::of::<String>()
        }
    }
}

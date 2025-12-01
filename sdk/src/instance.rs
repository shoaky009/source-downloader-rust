#![allow(dead_code)]

use crate::component::ComponentError;
use crate::{Map, Value};
use std::any::{type_name, Any, TypeId};
use std::sync::Arc;

pub trait InstanceFactory {
    /// Create an instance of type T with the given properties.
    /// Returns an error if instance creation fails.
    fn create_instance(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn Any + Send + Sync>, ComponentError>;
    /// Get the type name of the instance type use[`std::any::type_name`].
    fn instance_type_id(&self) -> TypeId;
    /// Get the factory name for logging purpose.
    fn factory_name(&self) -> String {
        type_name::<Self>().to_string()
    }
}

pub trait InstanceManager {
    /// Register a new instance factory.
    /// Returns an error if a factory for the same type is already registered.
    fn register_factory(&mut self, factory: Arc<dyn InstanceFactory>)
    -> Result<(), ComponentError>;
    /// Create an instance by name with the given properties.
    /// Returns an error if the factory for the instance type is not found.
    /// Also returns an error if instance creation fails.
    fn create_instance(
        &self,
        name: &str,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn Sync + Send>, ComponentError>;
    /// Destroy an instance by name.
    fn destroy_instance(&self, name: &str);
    /// Get all instances of a specific type.
    fn get_instances(&self, type_id: TypeId) -> Vec<Arc<dyn Sync + Send>>;
}

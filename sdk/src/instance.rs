#![allow(dead_code)]

use crate::component::ComponentError;
use crate::{Map, Value};
use std::any::TypeId;
use std::sync::Arc;

pub trait InstanceFactory<T: Sync + Send> {
    /// Create an instance of type T with the given properties.
    /// Returns an error if instance creation fails.
    fn create_instance(&self, props: &Map<String, Value>) -> Result<Arc<T>, ComponentError>;
    /// Get the TypeId of the instance type T.
    fn type_id(&self) -> TypeId;
}

pub trait InstanceManager {
    /// Register a new instance factory.
    /// Returns an error if a factory for the same type is already registered.
    fn register_factory(
        &mut self,
        factory: Arc<dyn InstanceFactory<dyn Sync + Send>>,
    ) -> Result<(), ComponentError>;
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

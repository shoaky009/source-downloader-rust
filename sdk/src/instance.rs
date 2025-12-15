#![allow(dead_code)]

use crate::component::ComponentError;
use crate::serde_json::{Map, Value};
use std::any::{type_name, Any, TypeId};
use std::sync::Arc;

pub trait InstanceFactory : Send + Sync {
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

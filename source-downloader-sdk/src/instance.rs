#![allow(dead_code)]

use crate::component::ComponentError;
use crate::serde_json::{Map, Value};
use serde::Serialize;
use std::any::{Any, TypeId, type_name};
use std::sync::Arc;
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstanceFactoryMetadata {
    pub description: String,
    pub props_json_schema: Option<Value>,
    pub props_ui_schema: Option<Value>,
}

pub trait InstanceFactory: Send + Sync {
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

    /// Describes the factory and its configuration for management clients.
    fn get_metadata(&self) -> Option<Box<InstanceFactoryMetadata>> {
        None
    }
}

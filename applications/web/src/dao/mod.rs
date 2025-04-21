use crate::error::error_handle::AppError;

pub mod yaml_file;

pub trait ComponentDao : Send + Sync {
    fn list_component_suppliers(&self) -> Result<Vec<String>, AppError>;
}

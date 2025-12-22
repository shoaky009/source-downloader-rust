pub mod cel;

use sdk::serde_json::{Map, Value};
use std::any::Any;

pub trait CompiledExpressionFactory: Send + Sync {
    fn create<T>(&self, expression: &str) -> Result<Box<dyn CompiledExpression<T>>, String>
    where
        T: ExprValue;
}

pub trait CompiledExpression<T>: Send + Sync
where
    T: ExprValue,
{
    fn execute(&self, vars: &Map<String, Value>) -> Result<T, String>;
}

pub trait ExprValue: Send + Sync + Sized + 'static {
    fn from_value(value: &dyn Any) -> Result<Self, String>;
}

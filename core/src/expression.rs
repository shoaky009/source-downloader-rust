mod cel;

use sdk::{Map, Value};
use serde::Serialize;
use std::any::Any;

pub trait CompiledExpressionFactory {
    fn create<T>(&self, expression: &str) -> Result<Box<dyn CompiledExpression<T>>, String>
    where
        T: ExprValue;
}

pub trait CompiledExpression<T>
where
    T: ExprValue,
{
    fn execute(&self, vars: &Map<String, Value>) -> Result<T, String>;
}

pub trait ExprValue: Sized + 'static {
    fn from_value(value: &dyn Any) -> Result<Self, String>;
}

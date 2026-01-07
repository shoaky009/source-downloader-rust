use crate::expression::{CompiledExpression, CompiledExpressionFactory, ExprValue};
use cel::extractors::This;
use cel::{Context, Program, Value};
use sdk::serde_json::Map;
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::Arc;

pub struct CelCompiledExpressionFactory {}
pub const FACTORY: CelCompiledExpressionFactory = CelCompiledExpressionFactory {};

impl CompiledExpressionFactory for CelCompiledExpressionFactory {
    fn create<T>(&self, expression: &str) -> Result<Box<dyn CompiledExpression<T>>, String>
    where
        T: ExprValue + 'static,
    {
        let program = Program::compile(expression).map_err(|e| e.to_string())?;
        Ok(Box::new(CelCompiledExpression::new(program)))
    }
}

pub struct CelCompiledExpression<T> {
    program: Program,
    _marker: PhantomData<T>,
}

impl<T> CompiledExpression<T> for CelCompiledExpression<T>
where
    T: ExprValue,
{
    fn execute(&self, vars: &Map<String, serde_json::Value>) -> Result<T, String> {
        let mut context = Context::default();
        context.add_function("containsAny", contains_any);
        for (k, v) in vars.iter() {
            // 预期不应该错误
            let _ = context
                .add_variable(k.as_str(), Self::json_to_cel(v))
                .unwrap();
        }
        let value = self.program.execute(&context).map_err(|e| e.to_string())?;
        T::from_value(&value)
    }
}

impl<T> CelCompiledExpression<T> {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            _marker: PhantomData,
        }
    }

    fn json_to_cel(value: &serde_json::Value) -> Value {
        match value {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Bool(*b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map(Value::Int)
                .or_else(|| n.as_u64().map(Value::UInt))
                .or_else(|| n.as_f64().map(Value::Float))
                .unwrap_or(Value::Null),
            serde_json::Value::String(s) => Value::String(Arc::new(s.to_owned())),
            serde_json::Value::Array(arr) => {
                Value::List(Arc::new(arr.iter().map(Self::json_to_cel).collect()))
            }
            serde_json::Value::Object(obj) => {
                let map: HashMap<String, Value> = obj
                    .iter()
                    .map(|(k, v)| (k.clone(), Self::json_to_cel(v)))
                    .collect();
                Value::Map(map.into())
            }
        }
    }
}

impl ExprValue for i64 {
    fn from_value(value: &dyn Any) -> Result<Self, String> {
        match value.downcast_ref::<Value>() {
            Some(v) => match v {
                Value::Int(i) => Ok(*i),
                Value::UInt(u) => Ok(*u as i64),
                Value::Float(f) => Ok(*f as i64),
                other => Err(format!(
                    "Cannot convert CEL value: expected i64, got {}",
                    other.type_of()
                )),
            },
            None => Err("Value type mismatch".into()),
        }
    }
}

impl ExprValue for f64 {
    fn from_value(value: &dyn Any) -> Result<Self, String> {
        match value.downcast_ref::<Value>() {
            Some(v) => match v {
                Value::Int(i) => Ok(*i as f64),
                Value::UInt(u) => Ok(*u as f64),
                Value::Float(f) => Ok(*f),
                other => Err(format!(
                    "Cannot convert CEL value: expected f64, got {}",
                    other.type_of()
                )),
            },
            None => Err("Value type mismatch".into()),
        }
    }
}

impl ExprValue for bool {
    fn from_value(value: &dyn Any) -> Result<Self, String> {
        match value.downcast_ref::<Value>() {
            Some(v) => match v {
                Value::Bool(b) => Ok(*b),
                _ => Err(format!(
                    "Cannot convert CEL value: expected bool, got {}",
                    v.type_of()
                )),
            },
            None => Err("Value type mismatch".into()),
        }
    }
}

impl ExprValue for String {
    fn from_value(value: &dyn Any) -> Result<Self, String> {
        match value.downcast_ref::<Value>() {
            Some(v) => match v {
                Value::String(s) => Ok(s.to_string()),
                Value::Int(i) => Ok(i.to_string()),
                Value::UInt(u) => Ok(u.to_string()),
                Value::Float(f) => Ok(f.to_string()),
                Value::Bool(b) => Ok(b.to_string()),
                _ => Err(format!(
                    "Cannot convert CEL value: expected String, got {}",
                    v.type_of()
                )),
            },
            None => Err("Value type mismatch".into()),
        }
    }
}

fn contains_any_not_ignore_case(This(source): This<Arc<Vec<Value>>>, target: Arc<Vec<Value>>) -> bool {
    contains_any(This(source), target, false)
}

fn contains_any(
    This(source): This<Arc<Vec<Value>>>,
    target: Arc<Vec<Value>>,
    ignore_case: bool,
) -> bool {
    if ignore_case {
        let target_set: HashSet<String> = target
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.to_lowercase()),
                _ => None,
            })
            .collect();

        source.iter().any(|v| match v {
            Value::String(s) => target_set.contains(&s.to_lowercase()),
            _ => false,
        })
    } else {
        let target_set: HashSet<&str> = target
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();

        source.iter().any(|v| match v {
            Value::String(s) => target_set.contains(s.as_str()),
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::expression::CompiledExpressionFactory;
    use crate::expression::cel::FACTORY;
    use sdk::serde_json::Map;

    #[test]
    fn test_cel_expression() {
        let expression = FACTORY.create::<i64>("a+c.c1");
        assert!(expression.is_ok());
        let data = r#"{"a": 1, "b": 1, "c": {"c1": 3}}"#;
        let vars: Map<String, serde_json::Value> = serde_json::from_str(data).unwrap();
        let result = expression.unwrap().execute(&vars);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 4);
    }
}

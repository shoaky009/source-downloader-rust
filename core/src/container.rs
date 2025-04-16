use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

/// 对象包装器错误
#[derive(Debug)]
pub enum ObjectWrapperError {
    /// 对象不存在
    NotFound(String),
    /// 类型转换错误
    TypeCast {
        name: String,
        expected: &'static str,
        actual: String,
    },
}

impl fmt::Display for ObjectWrapperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectWrapperError::NotFound(name) => write!(f, "Object '{}' not found", name),
            ObjectWrapperError::TypeCast {
                name,
                expected,
                actual,
            } => write!(
                f,
                "Object '{}' cannot be cast to {}, actual type: {}",
                name, expected, actual
            ),
        }
    }
}

impl std::error::Error for ObjectWrapperError {}

/// SimpleObjectWrapperContainer 是 ObjectWrapperContainer 的简单实现
/// 使用 RwLock 和 HashMap 来存储对象包装器
pub struct ObjectWrapperContainer {
    objects: RwLock<HashMap<String, Arc<dyn ObjectWrapper>>>,
}

impl ObjectWrapperContainer {
    pub fn new() -> Self {
        ObjectWrapperContainer {
            objects: RwLock::new(HashMap::new()),
        }
    }
}

impl ObjectWrapperContainer {
    pub fn contains(&self, name: &str) -> bool {
        self.objects.read().unwrap().contains_key(name)
    }

    pub fn put(&self, name: &str, value: Box<dyn ObjectWrapper>) {
        self.objects
            .write()
            .unwrap()
            .insert(name.to_owned(), Arc::from(value));
    }

    pub fn get<T: 'static>(
        &self,
        name: &str,
    ) -> Result<Arc<dyn ObjectWrapper>, ObjectWrapperError> {
        let objects = self.objects.read().unwrap();
        let object = objects
            .get(name)
            .cloned()
            .ok_or_else(|| ObjectWrapperError::NotFound(name.to_owned()))?;

        // 检查对象是否可以转换为请求的类型
        let type_name = std::any::type_name::<T>();
        if object.get().type_id() == TypeId::of::<T>() {
            Ok(object)
        } else {
            Err(ObjectWrapperError::TypeCast {
                name: name.to_owned(),
                expected: type_name,
                actual: format!("{:?}", object.get().type_id()),
            })
        }
    }

    pub fn get_objects_of_type<T: 'static>(&self) -> HashMap<String, Arc<dyn ObjectWrapper>> {
        let objects = self.objects.read().unwrap();
        let type_id = TypeId::of::<T>();

        objects
            .iter()
            .filter(|(_, object)| object.get().type_id() == type_id)
            .map(|(name, object)| (name.clone(), object.clone()))
            .collect()
    }

    pub fn remove(&self, name: &str) {
        self.objects.write().unwrap().remove(name);
    }

    pub fn get_all_object_names(&self) -> Vec<String> {
        self.objects.read().unwrap().keys().cloned().collect()
    }
}

pub trait ObjectWrapper: Send + Sync {
    fn as_any(&self) -> &dyn Any;

    fn as_any_mut(&mut self) -> &mut dyn Any;

    fn get(&self) -> &dyn Any;
}

pub struct GenericObjectWrapper<T: Any + Send + Sync> {
    inner: T,
}

impl<T: Any + Send + Sync> GenericObjectWrapper<T> {
    pub fn new(obj: T) -> Self {
        Self { inner: obj }
    }

    pub fn get_typed(&self) -> &T {
        &self.inner
    }
}

impl<T: Any + Send + Sync> ObjectWrapper for GenericObjectWrapper<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn get(&self) -> &dyn Any {
        &self.inner
    }
}

/// Component wrapper specifically for components
pub struct ComponentWrapper<T: Send + Sync + 'static> {
    inner: T,
}

impl<T: Send + Sync + 'static> ComponentWrapper<T> {
    pub fn new(component: T) -> Self {
        Self { inner: component }
    }

    pub fn get_component(&self) -> &T {
        &self.inner
    }
}

impl<T: Send + Sync + 'static> ObjectWrapper for ComponentWrapper<T> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn get(&self) -> &dyn Any {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_object_wrapper_container() {
        let container = ObjectWrapperContainer::new();

        // 创建一个简单的字符串包装器
        let string_wrapper = Box::new(GenericObjectWrapper::new("Hello, world!".to_owned()));
        container.put("greeting", string_wrapper);

        // 检查是否包含对象
        assert!(container.contains("greeting"));
        assert!(!container.contains("nonexistent"));

        // 尝试获取对象
        let result: Result<Arc<dyn ObjectWrapper>, _> = container.get::<String>("greeting");
        assert!(result.is_ok());

        // 尝试转换并检查值
        let wrapper = result.unwrap();
        let value = wrapper.get();
        if let Some(string_value) = value.downcast_ref::<String>() {
            assert_eq!(string_value, "Hello, world!");
        } else {
            panic!("Expected String, got something else");
        }

        // 尝试获取错误类型
        let result: Result<Arc<dyn ObjectWrapper>, _> = container.get::<i32>("greeting");
        assert!(result.is_err());

        // 获取所有对象名称
        let names = container.get_all_object_names();
        assert_eq!(names.len(), 1);
        assert!(names.contains(&"greeting".to_owned()));

        // 删除对象
        container.remove("greeting");
        assert!(!container.contains("greeting"));
    }
}

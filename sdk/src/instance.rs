#![allow(dead_code)]
use std::any::Any;

pub trait InstanceFactory {
    fn create_instance(&self, instance_id: &str) -> Box<dyn Any>;
}

pub trait InstanceManager {}
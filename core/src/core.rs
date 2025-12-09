use crate::components::get_build_in_component_supplier;
use crate::instance_manager::InstanceManager;
use crate::plugin::PluginManager;
use crate::processor_manager::ProcessorManager;
use crate::{ComponentManager, ConfigOperator};
use sdk::plugin::PluginContext;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

pub struct CoreApplication {
    pub config_operator: Arc<dyn ConfigOperator>,
    pub component_manager: Arc<ComponentManager>,
    pub instance_manager: Arc<InstanceManager>,
    pub processor_manager: Arc<ProcessorManager>,
    pub plugin_manager: PluginManager,
    pub data_location: Box<Path>,
    pub plugin_location: Option<Box<Path>>,
}

impl CoreApplication {
    pub fn start(&self) {
        self.init_plugin();
        self.register_instance_factory();
        self.register_component_supplier();
        info!("{}", self.component_manager);
        self.create_processors();
        self.start_triggers();
    }

    fn init_plugin(&self) {
        let path = match &self.plugin_location {
            Some(p) => p,
            None => {
                info!("未配置插件路径不加载插件");
                return;
            }
        };
        info!("从目录加载插件: {}", path.display());
        self.plugin_manager
            .load_dylib_plugins(path.to_str().unwrap());
    }

    fn register_instance_factory(&self) {
        self.plugin_manager.with_plugins(|plugins| {
            for plugin in plugins {
                plugin.get_instance_factories().iter().for_each(|x| {
                    // 有重复的直接crash
                    self.instance_manager
                        .register_instance_factory(x.clone())
                        .unwrap();
                });
            }
        })
    }

    fn register_component_supplier(&self) {
        self.component_manager
            .register_suppliers(get_build_in_component_supplier())
            .unwrap();

        self.plugin_manager.with_plugins(|plugins| {
            for plugin in plugins {
                plugin.get_component_suppliers().iter().for_each(|x| {
                    // 有重复的直接crash
                    self.component_manager.register_supplier(x.clone()).unwrap();
                })
            }
        })
    }

    fn create_processors(&self) {
        let configs = self.config_operator.get_all_processor_config();
        info!("Total {} processors to be created", configs.len());
        for cfg in configs {
            self.processor_manager.create_processor(&cfg)
        }
    }

    fn start_triggers(&self) {
        self.component_manager.for_each_trigger(|wrapper, trigger| {
            info!(
                "Starting trigger {}:{}",
                wrapper.component_type.name, wrapper.name
            );
            trigger.start();
        });
    }
}

pub struct CorePluginContext {}

impl CorePluginContext {
    pub fn new() -> Self {
        CorePluginContext {}
    }
}

impl PluginContext for CorePluginContext {
    fn get_persistent_data_path(&self) -> &Path {
        todo!()
    }
}

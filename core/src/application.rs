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
                    // 因为插件目前没有卸载重载等周期
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
                wrapper.id.component_type.name, wrapper.id.name
            );
            trigger.start();
        });
    }

    pub fn reload(&self) {
        self.destroy_all_processor();
        self.destroy_all_component();
        self.destroy_all_instance();
        self.create_processors();
        self.start_triggers();
    }

    fn destroy_all_processor(&self) {
        for name in self.processor_manager.get_all_processor_names() {
            self.processor_manager.destroy_processor(&name)
        }
        info!("All processors destroyed");
    }

    fn destroy_all_component(&self) {
        for wrapper in self.component_manager.get_all_component() {
            self.component_manager.destroy(&wrapper.id)
        }
        info!("All components destroyed");
    }

    fn destroy_all_instance(&self) {
        self.instance_manager.destroy_all_instances();
        info!("All instances destroyed");
    }
}

pub struct CorePluginContext {
    pub data_location: Box<Path>,
}

impl PluginContext for CorePluginContext {
    fn get_persistent_data_path(&self) -> &Path {
        self.data_location.as_ref()
    }
}

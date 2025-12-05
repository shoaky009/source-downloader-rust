use crate::ComponentManager;
use crate::components::system_file_source::SystemFileSourceSupplier;
use crate::instance_manager::InstanceManager;
use crate::plugin::PluginManager;
use crate::processor_manager::ProcessorManager;
use sdk::component::{ComponentError, ComponentSupplier};
use sdk::instance::InstanceFactory;
use sdk::plugin::PluginContext;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

pub struct CoreApplication {
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
        self.create_processor();
    }

    fn init_plugin(&self) {
        if self.plugin_location.is_none() {
            info!("未配置插件路径不加载插件");
            return;
        }
        let path = self.plugin_location.as_ref().unwrap().to_str().unwrap();
        info!("从目录加载插件: {}", path);
        self.plugin_manager.load_dylib_plugins(&path);
    }

    fn register_instance_factory(&self) {}

    fn register_component_supplier(&self) {
        self.component_manager
            .register_supplier(Arc::new(SystemFileSourceSupplier {}))
            .unwrap();
    }

    fn create_processor(&self) {}
}

pub struct CorePluginContext {
    component_manager: Arc<ComponentManager>,
    instance_manager: Arc<InstanceManager>,
}

impl CorePluginContext {
    pub fn new(
        component_manager: Arc<ComponentManager>,
        instance_manager: Arc<InstanceManager>,
    ) -> Self {
        CorePluginContext {
            component_manager,
            instance_manager,
        }
    }
}

impl PluginContext for CorePluginContext {
    fn get_persistent_data_path(&self) -> &Path {
        todo!()
    }

    fn register_supplier(&self, suppliers: Vec<Arc<dyn ComponentSupplier>>) {
        self.component_manager
            .register_suppliers(suppliers)
            .unwrap();
    }

    fn register_instance_factory(
        &self,
        factories: Vec<Arc<dyn InstanceFactory>>,
    ) -> Result<bool, ComponentError> {
        for fac in factories {
            self.instance_manager.register_instance_factory(fac)?;
        }
        Ok(true)
    }
}

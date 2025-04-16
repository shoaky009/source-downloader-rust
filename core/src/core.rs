use crate::ComponentManager;
use crate::components::system_file_source::SystemFileSourceSupplier;
use libloading::{Library, Symbol};
use sdk::component::ComponentSupplier;
use sdk::instance::InstanceFactory;
use sdk::plugin::{Plugin, PluginContext};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

pub struct CoreApplication {
    pub component_manager: Rc<RefCell<ComponentManager>>,
    pub plugin_manager: PluginManager,
}

impl CoreApplication {
    pub fn start(&mut self) {
        self.init_plugin();
        self.register_instance_factory();
        self.register_component_supplier();
        self.create_processor();
    }

    fn init_plugin(&mut self) {
        self.plugin_manager.load_dylib_plugins();
    }

    fn register_instance_factory(&mut self) {}

    fn register_component_supplier(&mut self) {
        self.component_manager
            .borrow_mut()
            .register(Arc::new(SystemFileSourceSupplier {}))
            .unwrap();
    }

    fn create_processor(&mut self) {}
}

pub struct CorePluginContext {
    component_manager: Rc<RefCell<ComponentManager>>,
}

impl CorePluginContext {
    pub fn new(manager: Rc<RefCell<ComponentManager>>) -> Self {
        CorePluginContext {
            component_manager: manager,
        }
    }
}

impl PluginContext for CorePluginContext {
    fn get_persistent_data_path(&self) -> &Path {
        todo!()
    }

    fn register_supplier(&mut self, suppliers: Vec<Arc<dyn ComponentSupplier>>) {
        self.component_manager
            .borrow_mut()
            .register_suppliers(suppliers)
            .unwrap();
    }

    fn register_instance_factory(&mut self, factories: Vec<Box<dyn InstanceFactory>>) {
        // 暂时不做任何操作，防止未使用变量警告
        let _ = factories;
    }
}

pub struct PluginManager {
    context: Arc<Mutex<dyn PluginContext>>,
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginManager {
    pub fn new(ctx: Arc<Mutex<dyn PluginContext>>) -> Self {
        PluginManager {
            context: ctx,
            plugins: Vec::new(),
        }
    }

    pub fn load_dylib_plugins(&mut self) {
        // TODO 根据配置加载暂时写死
        let plugin_path = "./target/debug/libcommon.dylib";
        unsafe {
            let lib = Library::new(plugin_path).expect("Failed to load plugin");
            let create_plugin: Symbol<unsafe extern "Rust" fn() -> Box<dyn Plugin>> =
                lib.get(b"create_plugin").expect("Failed to find symbol");
            let plugin = create_plugin();
            plugin.init(self.context.clone());
            log::info!("Loaded plugin: {}", plugin.description());
            self.plugins.push(plugin);
        }
    }
}

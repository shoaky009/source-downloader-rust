use crate::components::system_file_source::SystemFileSourceSupplier;
use crate::ComponentManager;
use libloading::Library;
use sdk::component::ComponentSupplier;
use sdk::instance::InstanceFactory;
use sdk::plugin::{Plugin, PluginContext};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::{env, fs};

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
        match env::var("SOURCE_DOWNLOADER_PLUGIN_LOCATION") {
            Ok(path) => {
                log::info!("从目录加载插件: {}", path);
                self.plugin_manager.load_dylib_plugins(&path);
            }
            Err(_) => {
                log::info!("未设置 SOURCE_DOWNLOADER_PLUGIN_LOCATION 环境变量");
            }
        }
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
    // Keep libraries alive as long as the plugins are in use
    _libraries: Vec<Library>,
}

impl PluginManager {
    pub fn new(ctx: Arc<Mutex<dyn PluginContext>>) -> Self {
        PluginManager {
            context: ctx,
            plugins: Vec::new(),
            _libraries: Vec::new(),
        }
    }

    pub fn load_dylib_plugins(&mut self, plugin_dir: &str) {
        let lib_ext = if cfg!(target_os = "windows") {
            "dll"
        } else if cfg!(target_os = "macos") {
            "dylib"
        } else {
            "so"
        };

        match fs::read_dir(plugin_dir) {
            Ok(entries) => {
                for entry in entries.filter_map(Result::ok) {
                    let path = entry.path();
                    if path.extension().and_then(|ext| ext.to_str()) != Some(lib_ext) {
                        continue;
                    }
                    self.try_load_plugin(&path);
                }
            }
            Err(e) => log::error!("无法读取插件目录 {}: {}", plugin_dir, e),
        }
    }

    fn try_load_plugin(&mut self, path: &Path) {
        let lib = match unsafe { Library::new(path) } {
            Ok(lib) => lib,
            Err(e) => {
                log::error!("加载插件失败 {:?}: {}", path, e);
                return;
            }
        };

        unsafe {
            let create_plugin_result =
                lib.get::<unsafe extern "Rust" fn() -> Box<dyn Plugin>>(b"create_plugin");
            match create_plugin_result {
                Ok(create_plugin) => {
                    let plugin = create_plugin();
                    plugin.init(self.context.clone());
                    log::info!("成功加载插件: {}", plugin.description());
                    self.plugins.push(plugin);
                    self._libraries.push(lib);
                }
                Err(e) => {
                    log::error!("在插件中查找符号失败 {:?}: {}", path, e);
                }
            }
        }
    }
}

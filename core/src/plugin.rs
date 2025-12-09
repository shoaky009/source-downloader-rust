use libloading::Library;
use parking_lot::RwLock;
use sdk::plugin::{Plugin, PluginContext};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tracing::{error, info};

pub struct PluginManager {
    context: Arc<dyn PluginContext>,
    plugins: RwLock<Vec<Box<dyn Plugin>>>,
    _libraries: RwLock<Vec<Library>>,
}

impl PluginManager {
    pub fn new(ctx: Arc<dyn PluginContext>) -> Self {
        PluginManager {
            context: ctx,
            plugins: RwLock::new(Vec::new()),
            _libraries: RwLock::new(Vec::new()),
        }
    }

    pub fn with_plugins<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&[Box<dyn Plugin>]) -> R,
    {
        let guard = self.plugins.read();
        f(&guard)
    }

    pub fn register_plugin(&self, plugin: Box<dyn Plugin>) {
        plugin.init(self.context.clone());
        info!("成功注册内置插件: {}", plugin.description());
        self.plugins.write().push(plugin);
    }

    pub fn load_dylib_plugins(&self, plugin_dir: &str) {
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
            Err(e) => error!("无法读取插件目录 {}: {}", plugin_dir, e),
        }
    }

    fn try_load_plugin(&self, path: &Path) {
        let lib = match unsafe { Library::new(path) } {
            Ok(lib) => lib,
            Err(e) => {
                error!("加载插件失败 {:?}: {}", path, e);
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
                    info!("成功加载插件: {}", plugin.description());
                    self.plugins.write().push(plugin);
                    self._libraries.write().push(lib);
                }
                Err(e) => {
                    error!("在插件中查找符号失败 {:?}: {}", path, e);
                }
            }
        }
    }
}

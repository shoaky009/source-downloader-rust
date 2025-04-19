use core::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn main() {
    env_logger::Builder::new()
        .filter(None, log::LevelFilter::Info)
        .init();

    let component_manager = Rc::new(RefCell::new(ComponentManager::new()));

    let plugin_ctx = CorePluginContext::new(component_manager.clone());
    let plugin_ctx = Arc::new(Mutex::new(plugin_ctx));

    let plugin_manager = PluginManager::new(plugin_ctx);

    let mut app = CoreApplication {
        component_manager,
        plugin_manager,
    };
    app.start();

    // 打印组件管理器状态
    let manager = app.component_manager;
    log::info!("{}", manager.borrow())
}

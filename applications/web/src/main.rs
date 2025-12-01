use core::*;
use std::sync::{Arc, Mutex};
use parking_lot::RwLock;
use tokio::net::TcpListener;
use web::service::router;

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .filter(None, log::LevelFilter::Info)
        .init();

    let component_manager = Arc::new(RwLock::new(ComponentManager::new(
        // TODO from config
        Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        ))
    )));

    let plugin_ctx = CorePluginContext::new(component_manager.clone());
    let plugin_ctx = Arc::new(Mutex::new(plugin_ctx));

    let plugin_manager = PluginManager::new(plugin_ctx);

    let mut app = CoreApplication {
        component_manager,
        plugin_manager,
    };
    app.start();

    let app = Arc::new(RwLock::new(app));

    // 打印组件管理器状态
    let manager = app.read().component_manager.clone();
    log::info!("{}", manager.read());
    run_web_server(app).await;
}

async fn run_web_server(core_application: Arc<RwLock<CoreApplication>>) {
    // 使用router模块中的register_routers函数获取配置好的路由
    let app = router::register_routers(core_application);

    // 监听所有网络接口的3000端口
    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    log::info!("Web服务器已启动，监听端口 0.0.0.0:3000");

    // 使用axum serve函数启动服务器
    axum::serve(listener, app).await.unwrap();
}
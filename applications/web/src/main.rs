use clap::{Args, Parser};
use core::*;
use sdk::ProcessingStorage;
use std::path::Path;
use std::sync::Arc;
use storage_memory::MemoryProcessingStorage;
use tokio::net::TcpListener;
use tracing::log;
use web::{ApplicationContext, app};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().try_init().unwrap();

    let config = init_config();
    let storage = create_storage(&config.db);
    let core = create_core_application(&storage, &config.source_downloader);

    core.plugin_manager
        .register_plugin(Box::new(common::PLUGIN));
    core.start();

    let app = Arc::new(core);
    let ctx = Arc::new(ApplicationContext {
        core: app.clone(),
        storage,
    });
    run_web_server(ctx, &config).await;
}

fn init_config() -> ApplicationConfig {
    let args = CliArgs::parse();
    ApplicationConfig {
        server: args.server,
        source_downloader: SourceDownloaderConfig {
            data_location: args.data_location,
            plugin_location: args.plugin_location,
        },
        db: args.db,
    }
}

fn create_storage(_config: &Db) -> Arc<dyn ProcessingStorage> {
    // TODO change to sqlite
    Arc::new(MemoryProcessingStorage::new())
}

fn create_core_application(
    processing_storage: &Arc<dyn ProcessingStorage>,
    config: &SourceDownloaderConfig,
) -> CoreApplication {
    let config_path = config.data_location.join("config.yaml");
    let config_operator = Arc::new(YamlConfigOperator::new_path(config_path.as_path()));
    config_operator.init().unwrap();
    let component_manager = Arc::new(ComponentManager::new(config_operator.clone()));
    let instance_manager = Arc::new(InstanceManager::new(config_operator.clone()));
    let plugin_ctx = Arc::new(CorePluginContext {
        data_location: config.data_location.clone(),
    });

    let plugin_manager = PluginManager::new(plugin_ctx);
    let processor_manager = Arc::new(ProcessorManager::new(
        component_manager.clone(),
        processing_storage.clone(),
    ));
    CoreApplication {
        config_operator,
        component_manager,
        instance_manager,
        processor_manager,
        plugin_manager,
        data_location: config.data_location.clone(),
        plugin_location: config.plugin_location.clone(),
    }
}

async fn run_web_server(core_application: Arc<ApplicationContext>, config: &ApplicationConfig) {
    // 使用router模块中的register_routers函数获取配置好的路由
    let app = app::router::register_routers(core_application);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await.unwrap();
    log::info!("Web服务器已启动，监听 {}", &addr);

    // 使用axum serve函数启动服务器
    axum::serve(listener, app).await.unwrap();
}

struct ApplicationConfig {
    server: Server,
    source_downloader: SourceDownloaderConfig,
    db: Db,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8080,
        }
    }
}

#[allow(dead_code, unused)]
struct SourceDownloaderConfig {
    data_location: Box<Path>,
    plugin_location: Option<Box<Path>>,
}

#[derive(Parser, Debug)]
#[command(name = "source-downloader")]
struct CliArgs {
    /// 数据存放路径
    #[arg(
        long,
        short = 'd',
        env = "SOURCE_DOWNLOADER_DATA_LOCATION",
        default_value = "./"
    )]
    data_location: Box<Path>,
    /// 插件加载路径
    #[arg(long, short = 'p', env = "SOURCE_DOWNLOADER_PLUGIN_LOCATION")]
    plugin_location: Option<Box<Path>>,
    /// 配置文件路径, 默认在data_location下的config.yaml
    #[arg(long, short = 'f')]
    config_file: Option<Box<Path>>,
    #[command(flatten)]
    server: Server,
    #[command(flatten)]
    db: Db,
}

#[derive(Args, Debug)]
struct Db {
    /// 数据库用户
    #[arg(long = "db.username", default_value = "sd")]
    username: String,
    /// 数据库密码
    #[arg(long = "db.password", default_value = "sd")]
    password: String,
    /// 数据库URL, 默认sqlite:{data_location}/source-downloader.db
    #[arg(long = "db.url")]
    url: Option<String>,
}

#[derive(Args, Debug)]
struct Server {
    #[arg(long = "server.host", default_value = "0.0.0.0")]
    host: String,
    #[arg(long = "server.port", default_value_t = 8080)]
    port: u16,
}

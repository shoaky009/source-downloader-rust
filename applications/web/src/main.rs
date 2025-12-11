use axum::http::Uri;
use axum::{http::StatusCode, middleware, response::IntoResponse, Router};
use clap::{Args, Parser};
use core::*;
use problem_details::ProblemDetails;
use sdk::ProcessingStorage;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use storage_sqlite::SeaProcessingStorage;
use tokio::net::TcpListener;
use tower_http::services::ServeDir;
use tracing::{info, log};
use tracing_subscriber::fmt::time::OffsetTime;
use web::{app, component, error_handle, path, processing, processor, ApplicationContext};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_timer(OffsetTime::local_rfc_3339().unwrap())
        .with_level(true)
        .with_ansi(true)
        .with_thread_names(true)
        .init();

    let config = init_config();
    let storage = create_storage(&config.db).await;
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

async fn create_storage(config: &Db) -> Arc<dyn ProcessingStorage> {
    let url = config
        .url
        .clone()
        .unwrap_or_else(|| "sqlite::memory:".to_string());
    let url = &url;
    info!("Using database url={}", url);
    let storage = SeaProcessingStorage::new(url).await.unwrap();
    Arc::new(storage)
}

fn create_core_application(
    processing_storage: &Arc<dyn ProcessingStorage>,
    config: &SourceDownloaderConfig,
) -> CoreApplication {
    info!("Using data location={}", config.data_location.display());
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
    let app_router = app::router::register_routers(core_application.clone());
    let component_routers = component::router::register_routers(core_application.clone());
    let processor_routers = processor::router::register_routers(core_application.clone());
    let processing_routers = processing::router::register_routers(core_application.clone());
    let path_routers = path::router::register_routers(core_application.clone());
    let api_routers = app_router
        .merge(component_routers)
        .merge(processor_routers)
        .merge(processing_routers)
        .merge(path_routers)
        .layer(middleware::from_fn(error_handle::error_handler));

    let root_router = match &config.server.static_dir {
        None => Router::new().nest("/api", api_routers),
        Some(dir) => {
            let dir_path = PathBuf::from(dir);
            Router::new()
                .nest("/api", api_routers)
                .fallback_service(
                    ServeDir::new(&dir_path)
                        .precompressed_gzip()
                        .precompressed_br(),
                )
                .fallback(move |uri: Uri| {
                    let dir = dir_path.clone();
                    async move { handle_spa_fallback(uri, dir).await }
                })
        }
    };

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await.unwrap();
    log::info!("Web服务器已启动，监听 {}", &addr);
    axum::serve(listener, root_router).await.unwrap();
}

async fn handle_spa_fallback(uri: Uri, dir: PathBuf) -> impl IntoResponse {
    if uri.path().starts_with("/api") {
        let problem = ProblemDetails::from_status_code(StatusCode::NOT_FOUND);
        return (StatusCode::NOT_FOUND, axum::Json(problem)).into_response();
    }
    let index_path = dir.join("index.html");
    match tokio::fs::read(&index_path).await {
        Ok(content) => axum::response::Html(content).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
    }
}

#[derive(Debug)]
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
            static_dir: None,
        }
    }
}

#[allow(dead_code, unused)]
#[derive(Debug)]
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
    #[arg(long, short = 'f', env = "SOURCE_DOWNLOADER_CONFIG_FILE")]
    config_file: Option<Box<Path>>,
    #[command(flatten)]
    server: Server,
    #[command(flatten)]
    db: Db,
}

#[derive(Args, Debug)]
struct Db {
    /// 数据库用户
    #[arg(
        long = "db.username",
        env = "SOURCE_DOWNLOADER_DB_USERNAME",
        default_value = "sd"
    )]
    username: String,
    /// 数据库密码
    #[arg(
        long = "db.password",
        env = "SOURCE_DOWNLOADER_DB_PASSWORD",
        default_value = "sd"
    )]
    password: String,
    /// 数据库URL, 默认sqlite:{data_location}/source-downloader.db
    #[arg(long = "db.url", env = "SOURCE_DOWNLOADER_DB_URL")]
    url: Option<String>,
}

#[derive(Args, Debug)]
struct Server {
    #[arg(
        long = "server.host",
        env = "SOURCE_DOWNLOADER_SERVER_HOST",
        default_value = "0.0.0.0"
    )]
    host: String,
    #[arg(
        long = "server.port",
        env = "SOURCE_DOWNLOADER_SERVER_PORT",
        default_value_t = 8080
    )]
    port: u16,
    #[arg(
        long = "server.static_dir",
        env = "SOURCE_DOWNLOADER_SERVER_STATIC_DIR"
    )]
    static_dir: Option<String>,
}

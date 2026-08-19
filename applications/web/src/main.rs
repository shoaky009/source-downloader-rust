mod webhook_adapter;

use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::{Router, middleware, response::IntoResponse};
use clap::{Args, Parser};
use problem_details::ProblemDetails;
use source_downloader_core::application::{CoreApplication, CorePluginContext};
use source_downloader_core::component_manager::ComponentManager;
use source_downloader_core::config::YamlConfigOperator;
use source_downloader_core::instance_manager::InstanceManager;
use source_downloader_core::plugin::PluginManager;
use source_downloader_core::processor_manager::ProcessorManager;
use source_downloader_sdk::storage::ProcessingStorage;
use std::fmt::Debug;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use storage_sqlite::SeaProcessingStorage;
use tokio::net::TcpListener;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tracing::{info, log};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::time::OffsetTime;
use web::{ApplicationContext, error_handle, service};
use webhook_adapter::AxumWebhookAdapter;

#[tokio::main]
async fn main() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_timer(OffsetTime::local_rfc_3339().unwrap())
        .with_level(true)
        .with_ansi(true)
        .with_thread_names(true)
        .with_thread_ids(true)
        .with_env_filter(filter)
        .init();

    let config = init_config();
    let storage =
        create_storage(&config.db, &config.source_downloader.data_location).await;
    let mut core = create_core_application(&storage, &config.source_downloader);
    let webhook_adapter = Arc::new(AxumWebhookAdapter::default());
    core.set_webhook_adapter(webhook_adapter.clone());

    core.plugin_manager.register_plugin(Box::new(common::PLUGIN));
    core.plugin_manager.register_plugin(Box::new(telegram::PLUGIN));
    core.start()
        .unwrap_or_else(|error| panic!("Failed to start core application: {error}"));

    let app = Arc::new(core);
    let ctx = Arc::new(ApplicationContext { core: app.clone(), storage });
    run_web_server(ctx, &config, webhook_adapter).await;
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

async fn create_storage(config: &Db, data_location: &Path) -> Arc<dyn ProcessingStorage> {
    let url = match &config.url {
        Some(url) => url.clone(),
        None => {
            tokio::fs::create_dir_all(data_location).await.unwrap();
            default_database_url(data_location)
        }
    };
    info!("Using database url={}", url);
    let storage = SeaProcessingStorage::new_with_wal(&url).await.unwrap();
    Arc::new(storage)
}

fn default_database_url(data_location: &Path) -> String {
    format!("sqlite:{}", data_location.join("source-downloader.db").display())
}

fn create_core_application(
    processing_storage: &Arc<dyn ProcessingStorage>,
    config: &SourceDownloaderConfig,
) -> CoreApplication {
    info!("Using data location={}", config.data_location.display());
    let config_path = config.data_location.join("config.yaml");
    let config_operator = Arc::new(YamlConfigOperator::new_path(config_path.as_path()));
    config_operator.init().unwrap();
    let instance_manager = Arc::new(InstanceManager::new(config_operator.clone()));
    let component_manager = Arc::new(ComponentManager::with_create_context(
        config_operator.clone(),
        instance_manager.clone(),
    ));
    let plugin_ctx =
        Arc::new(CorePluginContext { data_location: config.data_location.clone() });

    let plugin_manager = PluginManager::new(plugin_ctx);
    let run_manager = Arc::new(
        source_downloader_core::processor_run_manager::ProcessorRunManager::default(),
    );
    let processor_manager = Arc::new(ProcessorManager::new(
        component_manager.clone(),
        processing_storage.clone(),
        run_manager.clone(),
    ));
    CoreApplication {
        config_operator,
        component_manager,
        instance_manager,
        processor_manager,
        run_manager,
        plugin_manager,
        data_location: config.data_location.clone(),
        plugin_location: config.plugin_location.clone(),
        webhook_adapter: None,
    }
}

async fn run_web_server(
    core_application: Arc<ApplicationContext>,
    config: &ApplicationConfig,
    webhook_adapter: Arc<AxumWebhookAdapter>,
) {
    let app_router = service::app::register_routers(core_application.clone());
    let component_routers =
        service::component::register_routers(core_application.clone());
    let processor_routers =
        service::processor::register_routers(core_application.clone());
    let processing_routers =
        service::processing::register_routers(core_application.clone());
    let path_routers = service::path::register_routers(core_application.clone());
    let metadata_routers = service::metadata::register_routers(core_application.clone());
    let api_routers = app_router
        .merge(component_routers)
        .merge(processor_routers)
        .merge(processing_routers)
        .merge(path_routers)
        .merge(metadata_routers)
        .fallback(handle_api_fallback)
        .layer(middleware::from_fn(error_handle::error_handler));
    let webhook_router = webhook_adapter.router();
    let base_router = apply_cors(
        Router::new().merge(webhook_router).nest("/api", api_routers),
        &config.server.cors,
    )
    .unwrap_or_else(|error| panic!("Invalid CORS configuration: {error}"));

    let root_router = match &config.server.static_dir {
        None => base_router,
        Some(dir) => with_static_files(base_router, PathBuf::from(dir)),
    };

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = TcpListener::bind(&addr).await.unwrap();
    log::info!("Web服务器已启动，监听 {}", addr);
    axum::serve(listener, root_router)
        .with_graceful_shutdown(shutdown_signal(core_application.core.clone()))
        .await
        .unwrap();
}

fn apply_cors(router: Router, config: &Cors) -> Result<Router, String> {
    let Some(layer) = cors_layer(config)? else {
        return Ok(router);
    };
    Ok(router.layer(layer))
}

fn cors_layer(config: &Cors) -> Result<Option<CorsLayer>, String> {
    if config.allowed_origins.is_empty() {
        return Ok(None);
    }

    let wildcard = config.allowed_origins.iter().any(|origin| origin == "*");
    if wildcard && config.allowed_origins.len() != 1 {
        return Err("'*' must be the only allowed origin".to_owned());
    }
    if wildcard && config.allow_credentials {
        return Err("'*' cannot be combined with allow-credentials=true".to_owned());
    }

    let allow_origin = if wildcard {
        Any.into()
    } else {
        let origins = config
            .allowed_origins
            .iter()
            .map(|origin| {
                origin.parse::<HeaderValue>().map_err(|error| {
                    format!("invalid allowed origin '{origin}': {error}")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        AllowOrigin::list(origins)
    };

    Ok(Some(
        CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                HeaderName::from_static("authorization"),
                HeaderName::from_static("content-type"),
                HeaderName::from_static("accept"),
            ])
            .allow_credentials(config.allow_credentials)
            .max_age(std::time::Duration::from_secs(config.max_age)),
    ))
}

async fn shutdown_signal(core_application: Arc<CoreApplication>) {
    #[cfg(unix)]
    tokio::select! {
        () = ctrl_c_signal() => {}
        () = terminate_signal() => {}
    }
    #[cfg(not(unix))]
    ctrl_c_signal().await;

    info!("Shutdown signal received");
    core_application.shutdown();
}

async fn ctrl_c_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "Failed to listen for Ctrl+C");
        std::future::pending::<()>().await;
    }
}

#[cfg(unix)]
async fn terminate_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let Ok(mut signal) = signal(SignalKind::terminate()) else {
        tracing::error!("Failed to listen for SIGTERM");
        std::future::pending::<()>().await;
        return;
    };
    signal.recv().await;
}
fn with_static_files(base_router: Router, dir_path: PathBuf) -> Router {
    let index_path = dir_path.join("index.html");
    base_router.fallback_service(
        ServeDir::new(&dir_path)
            .precompressed_gzip()
            .precompressed_br()
            .fallback(ServeFile::new(index_path)),
    )
}

async fn handle_api_fallback() -> impl IntoResponse {
    let problem = ProblemDetails::from_status_code(StatusCode::NOT_FOUND);
    (StatusCode::NOT_FOUND, axum::Json(problem))
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
            cors: Cors::default(),
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
    #[arg(long = "server.static_dir", env = "SOURCE_DOWNLOADER_SERVER_STATIC_DIR")]
    static_dir: Option<String>,
    #[command(flatten)]
    cors: Cors,
}

#[derive(Args, Debug, Default)]
struct Cors {
    #[arg(
        long = "server.cors.allowed-origins",
        env = "SOURCE_DOWNLOADER_SERVER_CORS_ALLOWED_ORIGINS",
        value_delimiter = ','
    )]
    allowed_origins: Vec<String>,
    #[arg(
        long = "server.cors.allow-credentials",
        env = "SOURCE_DOWNLOADER_SERVER_CORS_ALLOW_CREDENTIALS",
        default_value_t = false,
        action = clap::ArgAction::Set
    )]
    allow_credentials: bool,
    #[arg(
        long = "server.cors.max-age",
        env = "SOURCE_DOWNLOADER_SERVER_CORS_MAX_AGE",
        default_value_t = 3600
    )]
    max_age: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn static_asset_is_served_from_configured_directory() {
        let static_dir = tempfile::tempdir().unwrap();
        tokio::fs::write(static_dir.path().join("index.html"), "<main>SPA</main>")
            .await
            .unwrap();
        let assets_dir = static_dir.path().join("assets");
        tokio::fs::create_dir(&assets_dir).await.unwrap();
        tokio::fs::write(assets_dir.join("app.js"), "console.log('loaded');")
            .await
            .unwrap();

        let response = with_static_files(Router::new(), static_dir.path().to_path_buf())
            .oneshot(
                Request::get("/assets/app.js").body(axum::body::Body::empty()).unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(axum::http::header::CONTENT_TYPE).unwrap(),
            "text/javascript"
        );
        assert_eq!(
            to_bytes(response.into_body(), usize::MAX).await.unwrap(),
            "console.log('loaded');"
        );
    }

    #[tokio::test]
    async fn configured_origin_receives_preflight_headers() {
        let cors = Cors {
            allowed_origins: vec!["https://source.example.com".to_owned()],
            allow_credentials: true,
            max_age: 7200,
        };
        let router = apply_cors(
            Router::new().route("/api/test", axum::routing::get(|| async {})),
            &cors,
        )
        .unwrap();

        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::OPTIONS)
                    .uri("/api/test")
                    .header("origin", "https://source.example.com")
                    .header("access-control-request-method", "GET")
                    .header("access-control-request-headers", "authorization")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.headers().get("access-control-allow-origin").unwrap(),
            "https://source.example.com"
        );
        assert_eq!(
            response.headers().get("access-control-allow-credentials").unwrap(),
            "true"
        );
        assert_eq!(response.headers().get("access-control-max-age").unwrap(), "7200");
    }

    #[tokio::test]
    async fn unconfigured_origin_does_not_receive_allow_origin_header() {
        let cors = Cors {
            allowed_origins: vec!["https://source.example.com".to_owned()],
            allow_credentials: false,
            max_age: 3600,
        };
        let router = apply_cors(
            Router::new().route("/api/test", axum::routing::get(|| async {})),
            &cors,
        )
        .unwrap();

        let response = router
            .oneshot(
                Request::get("/api/test")
                    .header("origin", "https://other.example.com")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(response.headers().get("access-control-allow-origin").is_none());
    }

    #[test]
    fn wildcard_origin_rejects_credentials() {
        let cors = Cors {
            allowed_origins: vec!["*".to_owned()],
            allow_credentials: true,
            max_age: 3600,
        };

        assert_eq!(
            cors_layer(&cors).unwrap_err(),
            "'*' cannot be combined with allow-credentials=true"
        );
    }

    #[test]
    fn missing_origins_disables_cors() {
        assert!(cors_layer(&Cors::default()).unwrap().is_none());
    }

    #[test]
    fn default_database_is_stored_in_data_location() {
        assert_eq!(
            default_database_url(Path::new("/var/lib/source-downloader")),
            "sqlite:/var/lib/source-downloader/source-downloader.db"
        );
    }
}

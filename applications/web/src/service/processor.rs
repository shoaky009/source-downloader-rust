use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use core::application::CoreApplication;
use core::config::ProcessorConfig;
use sdk::SourceItem;
use sdk::component::ProcessTask;
use sdk::serde_json::{Map, Value};
use sdk::time::UtcDateTime;
use serde::{Deserialize, Serialize};
use serde_qs::to_string;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::info;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/processor",
            Router::new()
                .route(
                    "/{name}",
                    get(get_processor)
                        .put(update_processor)
                        .delete(delete_processor),
                )
                .route("/", get(query_processors).post(create_processor))
                .route("/{name}/reload", post(reload_processor))
                .route("/{name}/dry-run", get(dry_run).post(dry_run))
                .route(
                    "/{name}/dry-run-stream",
                    get(dry_run_stream).post(dry_run_stream),
                )
                .route("/{name}/trigger", post(trigger_processor))
                .route("/{name}/rename", post(trigger_rename))
                .route("/{name}/items", post(post_items))
                .route("/{name}/state", get(get_state))
                .route("/{name}/pointer", put(update_pointer))
                .route("/{name}/contents", delete(delete_contents)),
        )
        .with_state(ctx.core.clone())
}

#[axum::debug_handler]
async fn get_processor(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("get_processor name={}", name);
    todo!()
}

#[axum::debug_handler]
async fn query_processors(
    State(_core): State<Arc<CoreApplication>>,
    Query(_): Query<QueryParams>,
) -> Json<Vec<ProcessorInfo>> {
    info!("query_processors");
    todo!()
}

#[axum::debug_handler]
async fn update_processor(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
    Json(body): Json<ProcessorConfig>,
) -> Result<(), AppError> {
    _core.config_operator.save_processor(body.clone())?;
    _core.processor_manager.destroy_processor(&_name);
    _core.processor_manager.create_processor(&body);
    Ok(())
}

#[axum::debug_handler]
async fn delete_processor(
    State(_core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<(), AppError> {
    _core.processor_manager.destroy_processor(&name);
    _core.config_operator.delete_processor(&name)?;
    Ok(())
}

#[axum::debug_handler]
async fn create_processor(
    State(_core): State<Arc<CoreApplication>>,
    Json(body): Json<ProcessorConfig>,
) -> Result<(), AppError> {
    if _core.processor_manager.processor_exists(&body.name) {
        return Err(AppError::BadRequest("Processor already exists".to_string()));
    }
    _core.config_operator.save_processor(body.clone())?;
    _core.processor_manager.create_processor(&body);
    Ok(())
}

#[axum::debug_handler]
async fn reload_processor(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
) -> Result<(), AppError> {
    let config = _core.config_operator.get_processor_config(&_name);
    if config.is_none() {
        return Err(AppError::NotFound("Processor config not found".to_string()));
    }
    let config = config.unwrap();
    if _core.processor_manager.processor_exists(&_name) {
        _core.processor_manager.destroy_processor(&_name)
    }
    _core.processor_manager.create_processor(&config);
    Ok(())
}

#[axum::debug_handler]
async fn trigger_processor(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
) -> Result<(), AppError> {
    let wp = _core
        .processor_manager
        .get_processor(&_name)
        .ok_or_else(|| AppError::NotFound("Processor not found".into()))?;
    let p = wp
        .processor
        .clone()
        .ok_or_else(|| AppError::BadRequest("Processor not running".into()))?;
    let _ = p.run().await;
    Ok(())
}

#[axum::debug_handler]
async fn dry_run(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
    Json(_): Json<Option<DryRunOptions>>,
) -> () {
    info!("dry_run name={}", _name);
    todo!()
}

#[axum::debug_handler]
async fn dry_run_stream(
    State(_code): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
    Json(_options): Json<Option<DryRunOptions>>,
) -> () {
    // gen application/x-ndjson
    info!("dry_run_stream name={}", _name);
    todo!()
}

#[axum::debug_handler]
async fn trigger_rename(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
) -> () {
    info!("trigger_rename name={}", _name);
    todo!()
}

#[axum::debug_handler]
async fn post_items(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
    Json(items): Json<Vec<SourceItem>>,
) -> () {
    info!(
        "post_items name={}, items={}",
        _name,
        to_string(&items).unwrap()
    );
    todo!()
}

#[axum::debug_handler]
async fn get_state(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("get_state name={}", name);
    todo!()
}

#[axum::debug_handler]
async fn update_pointer(
    State(_core): State<Arc<CoreApplication>>,
    Path(_name): Path<String>,
    Json(body): Json<PointerPayload>,
) -> () {
    info!(
        "update_pointer name={}, sourceId={} ,pt={}",
        _name,
        body.source_id,
        to_string(&body.pointer).unwrap()
    );
    todo!()
}

#[axum::debug_handler]
async fn delete_contents(
    State(_core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> () {
    info!("delete_contents name={}", name);
    todo!()
}

#[derive(Deserialize)]
struct PointerPayload {
    #[serde(rename = "sourceId")]
    pub source_id: String,
    pub pointer: Map<String, Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct QueryParams {
    name: Option<String>,
    size: Option<u32>,
    page: Option<u32>,
}

#[derive(Deserialize)]
pub struct DryRunOptions {
    pub pointer: Option<Map<String, Value>>,
    #[serde(rename = "filterProcessed")]
    pub filter_processed: Option<bool>,
}

#[derive(Serialize)]
struct ProcessorInfo {
    pub name: String,
    pub enabled: bool,
    pub category: Option<String>,
    pub tags: HashSet<String>,
    pub runtime: RuntimeSnapshot,
    #[serde(rename = "errorMessage")]
    pub error_message: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSnapshot {
    pub created_at: UtcDateTime,
    pub last_process_failed_message: Option<String>,
    pub last_start_process_time: Option<UtcDateTime>,
    pub last_end_process_time: Option<UtcDateTime>,
    pub processing: bool,
}

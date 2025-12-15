use crate::ApplicationContext;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use core::CoreApplication;
use core::ProcessorConfig;
use sdk::SourceItem;
use sdk::serde::{Deserialize, Serialize};
use sdk::serde_json::{Map, Value};
use sdk::time::UtcDateTime;
use serde_qs::to_string;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::info;

pub fn register_routers(core_application: Arc<ApplicationContext>) -> Router {
    let core: Arc<CoreApplication> = core_application.core.clone();
    Router::new()
        .nest(
            "/processor",
            Router::new()
                .route("/{name}", get(get_processor))
                .route("/", get(query_processors))
                .route("/{name}", put(update_processor))
                .route("/{name}", delete(delete_processor))
                .route("/", post(create_processor))
                .route("/{name}/reload", post(reload_processor))
                .route("/{name}/dry-run", get(dry_run))
                .route("/{name}/dry-run", post(dry_run))
                .route("/{name}/dry-run-stream", get(dry_run_stream))
                .route("/{name}/dry-run-stream", post(dry_run_stream))
                .route("/{name}/trigger", post(trigger_processor))
                .route("/{name}/rename", post(trigger_rename))
                .route("/{name}/items", post(post_items))
                .route("/{name}/state", get(get_state))
                .route("/{name}/pointer", put(update_pointer))
                .route("/{name}/contents", delete(delete_contents)),
        )
        .with_state(core)
}

#[axum::debug_handler]
async fn get_processor(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("get_processor name={}", name)
}

#[axum::debug_handler]
async fn query_processors(
    State(_): State<Arc<CoreApplication>>,
    Query(_): Query<QueryParams>,
) -> Json<Vec<ProcessorInfo>> {
    info!("query_processors");
    vec![].into()
}

#[axum::debug_handler]
async fn update_processor(
    State(_): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(body): Json<ProcessorConfig>,
) -> () {
    info!(
        "update_processor name={} body={}",
        name,
        to_string(&body).unwrap()
    )
}

#[axum::debug_handler]
async fn delete_processor(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("delete_processor name={}", name)
}

#[axum::debug_handler]
async fn create_processor(
    State(_): State<Arc<CoreApplication>>,
    Json(body): Json<ProcessorConfig>,
) -> () {
    info!("create_processor body={}", to_string(&body).unwrap())
}

#[axum::debug_handler]
async fn reload_processor(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("reload_processor name={}", name)
}

#[axum::debug_handler]
async fn trigger_processor(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("trigger_processor name={}", name)
}

#[axum::debug_handler]
async fn dry_run(
    State(_): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(_): Json<Option<DryRunOptions>>,
) -> () {
    info!("dry_run name={}", name)
}

#[axum::debug_handler]
async fn dry_run_stream(
    State(_): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(_): Json<Option<DryRunOptions>>,
) -> () {
    // gen application/x-ndjson
    info!("dry_run_stream name={}", name)
}

#[axum::debug_handler]
async fn trigger_rename(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("trigger_rename name={}", name)
}

#[axum::debug_handler]
async fn post_items(
    State(_): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(items): Json<Vec<SourceItem>>,
) -> () {
    info!(
        "post_items name={}, items={}",
        name,
        to_string(&items).unwrap()
    )
}

#[axum::debug_handler]
async fn get_state(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("get_state name={}", name)
}

#[axum::debug_handler]
async fn update_pointer(
    State(_): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(body): Json<PointerPayload>,
) -> () {
    info!(
        "update_pointer name={}, sourceId={} ,pt={}",
        name,
        body.source_id,
        to_string(&body.pointer).unwrap()
    )
}

#[axum::debug_handler]
async fn delete_contents(State(_): State<Arc<CoreApplication>>, Path(name): Path<String>) -> () {
    info!("delete_contents name={}", name)
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
pub struct ProcessorInfo {
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
pub struct RuntimeSnapshot {
    pub created_at: UtcDateTime,
    pub last_process_failed_message: Option<String>,
    pub last_start_process_time: Option<UtcDateTime>,
    pub last_end_process_time: Option<UtcDateTime>,
    pub processing: bool,
}

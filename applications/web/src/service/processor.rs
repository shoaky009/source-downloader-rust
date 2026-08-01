use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use source_downloader_core::application::CoreApplication;
use source_downloader_core::config::ProcessorConfig;
use source_downloader_core::processor_manager::ProcessorWrapper;
use source_downloader_core::source_processor::{
    DryRunOptions as CoreDryRunOptions, DryRunResult, ProcessorContentDeletion,
    ProcessorRuntimeSnapshot, SourceProcessor,
};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::ProcessTask;
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::storage::ProcessorSourceState;
use source_downloader_sdk::time::{OffsetDateTime, UtcDateTime};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::warn;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/processor",
            Router::new()
                .route(
                    "/{name}",
                    get(get_processor).put(update_processor).delete(delete_processor),
                )
                .route("/", get(query_processors).post(create_processor))
                .route("/{name}/reload", post(reload_processor))
                .route("/{name}/dry-run", get(dry_run).post(dry_run))
                .route("/{name}/dry-run-stream", get(dry_run_stream).post(dry_run_stream))
                .route("/{name}/trigger", get(trigger_processor))
                .route("/{name}/rename", get(trigger_rename))
                .route("/{name}/items", post(post_items))
                .route("/{name}/state", get(get_state))
                .route("/{name}/pointer", put(update_pointer))
                .route("/{name}/contents", delete(delete_contents)),
        )
        .with_state(ctx.core.clone())
}

fn require_processor(
    core: &CoreApplication,
    name: &str,
) -> Result<Arc<SourceProcessor>, AppError> {
    let wrapper = core
        .processor_manager
        .get_processor(name)
        .ok_or_else(|| AppError::NotFound("Processor not found".to_owned()))?;
    wrapper.processor.clone().ok_or_else(|| {
        AppError::BadRequest(
            wrapper
                .error_message
                .clone()
                .unwrap_or_else(|| "Processor not running".to_owned()),
        )
    })
}
#[axum::debug_handler]
async fn get_processor(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<ProcessorConfig>, AppError> {
    let config = core
        .config_operator
        .get_processor_config(&name)
        .ok_or_else(|| AppError::NotFound("Processor config not found".to_owned()))?;
    Ok(Json(config))
}

#[axum::debug_handler]
async fn query_processors(
    State(core): State<Arc<CoreApplication>>,
    Query(params): Query<QueryParams>,
) -> Json<Vec<ProcessorInfo>> {
    let processors = select_processor_configs(
        core.config_operator.get_all_processor_config(),
        &params,
    )
    .into_iter()
    .map(|config| {
        let wrapper = core.processor_manager.get_processor(&config.name);
        ProcessorInfo::from_config(&config, wrapper.as_deref())
    })
    .collect();
    Json(processors)
}

#[axum::debug_handler]
async fn update_processor(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(body): Json<ProcessorConfig>,
) -> Result<Json<ProcessorConfig>, AppError> {
    if core.config_operator.get_processor_config(&name).is_none() {
        return Err(AppError::NotFound("Processor config not found".to_owned()));
    }
    if body.name != name {
        return Err(AppError::BadRequest(format!(
            "Processor name mismatch: path={name}, body={}",
            body.name
        )));
    }
    core.config_operator.save_processor(body.clone())?;
    core.processor_manager.destroy_processor(&name);
    core.processor_manager.create_processor(&body);
    Ok(Json(body))
}

#[axum::debug_handler]
async fn delete_processor(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    core.processor_manager.destroy_processor(&name);
    core.config_operator.delete_processor(&name)?;
    Ok(StatusCode::NO_CONTENT)
}

#[axum::debug_handler]
async fn create_processor(
    State(core): State<Arc<CoreApplication>>,
    Json(body): Json<ProcessorConfig>,
) -> Result<StatusCode, AppError> {
    if core.processor_manager.processor_exists(&body.name) {
        return Err(AppError::BadRequest("Processor already exists".to_string()));
    }
    core.config_operator.save_processor(body.clone())?;
    core.processor_manager.create_processor(&body);
    Ok(StatusCode::CREATED)
}

#[axum::debug_handler]
async fn reload_processor(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    let config = core
        .config_operator
        .get_processor_config(&name)
        .ok_or_else(|| AppError::NotFound("Processor config not found".to_string()))?;
    if core.processor_manager.processor_exists(&name) {
        core.processor_manager.destroy_processor(&name);
    }
    core.processor_manager.create_processor(&config);
    Ok(StatusCode::NO_CONTENT)
}

#[axum::debug_handler]
async fn trigger_processor(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    let processor = require_processor(&core, &name)?;
    tokio::spawn(async move {
        if let Err(error) = processor.run().await {
            warn!("Processor[manual-trigger-error] {} {}", processor.name, error);
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[axum::debug_handler]
async fn dry_run(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    options: Option<Json<DryRunOptions>>,
) -> Result<Json<Vec<DryRunResult>>, AppError> {
    let processor = require_processor(&core, &name)?;
    let options = options.map(|Json(options)| options).unwrap_or_default();
    let results = processor.dry_run(options.into()).await?;
    Ok(Json(results))
}

#[axum::debug_handler]
async fn dry_run_stream(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    options: Option<Json<DryRunOptions>>,
) -> Result<Response<Body>, AppError> {
    let processor = require_processor(&core, &name)?;
    let options = options.map(|Json(options)| options).unwrap_or_default();
    let stream = processor.dry_run_stream(options.into()).map(|result| {
        let result =
            result.map_err(|error| std::io::Error::other(error.message().to_owned()))?;
        let mut line = source_downloader_sdk::serde_json::to_vec(&result)
            .map_err(std::io::Error::other)?;
        line.push(b'\n');
        Ok::<_, std::io::Error>(Bytes::from(line))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(Body::from_stream(stream))
        .map_err(|error| AppError::InternalError(error.to_string()))
}

#[axum::debug_handler]
async fn trigger_rename(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<StatusCode, AppError> {
    require_processor(&core, &name)?.run_rename().await?;
    Ok(StatusCode::ACCEPTED)
}

#[axum::debug_handler]
async fn post_items(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(items): Json<Vec<SourceItem>>,
) -> Result<StatusCode, AppError> {
    require_processor(&core, &name)?.run_items(items).await?;
    Ok(StatusCode::ACCEPTED)
}

#[axum::debug_handler]
async fn get_state(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<ProcessorState>, AppError> {
    let state = require_processor(&core, &name)?.source_state().await?;
    Ok(Json(state.into()))
}

#[axum::debug_handler]
async fn update_pointer(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(body): Json<PointerPayload>,
) -> Result<(), AppError> {
    let processor = require_processor(&core, &name)?;
    let updated = processor
        .update_source_pointer(&body.source_id, Value::Object(body.pointer))
        .await?;
    if updated.is_none() {
        return Err(AppError::NotFound(format!(
            "Processor {name} with source {} not found",
            body.source_id
        )));
    }
    Ok(())
}

#[axum::debug_handler]
async fn delete_contents(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<ContentDeletion>, AppError> {
    let deleted = require_processor(&core, &name)?.delete_contents().await?;
    Ok(Json(deleted.into()))
}

#[derive(Deserialize)]
struct PointerPayload {
    #[serde(rename = "sourceId")]
    pub source_id: String,
    pub pointer: Map<String, Value>,
}

#[derive(Default, Deserialize)]
struct QueryParams {
    name: Option<String>,
    size: Option<usize>,
    page: Option<usize>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunOptions {
    pub pointer: Option<Map<String, Value>>,
    pub filter_processed: Option<bool>,
}

impl From<DryRunOptions> for CoreDryRunOptions {
    fn from(options: DryRunOptions) -> Self {
        Self {
            pointer: options.pointer.map(Value::Object),
            filter_processed: options.filter_processed.unwrap_or(true),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessorState {
    source_id: String,
    pointer: Value,
    last_active_time: Option<OffsetDateTime>,
    retry_times: u32,
}

impl From<ProcessorSourceState> for ProcessorState {
    fn from(state: ProcessorSourceState) -> Self {
        Self {
            source_id: state.source_id,
            pointer: state.last_pointer,
            last_active_time: state.last_active_time,
            retry_times: state.retry_times,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ContentDeletion {
    processing_content: u64,
    target_path: u64,
}

impl From<ProcessorContentDeletion> for ContentDeletion {
    fn from(deleted: ProcessorContentDeletion) -> Self {
        Self {
            processing_content: deleted.processing_content,
            target_path: deleted.target_path,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessorInfo {
    pub name: String,
    pub enabled: bool,
    pub category: Option<String>,
    pub tags: HashSet<String>,
    pub runtime: Option<RuntimeSnapshot>,
    pub error_message: Option<String>,
}

impl ProcessorInfo {
    fn from_config(config: &ProcessorConfig, wrapper: Option<&ProcessorWrapper>) -> Self {
        Self {
            name: config.name.clone(),
            enabled: config.enabled,
            category: config.category.clone(),
            tags: config.tags.clone(),
            runtime: wrapper
                .and_then(|wrapper| wrapper.processor.as_ref())
                .map(|processor| processor.runtime_snapshot().into()),
            error_message: wrapper.and_then(|wrapper| wrapper.error_message.clone()),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSnapshot {
    pub created_at: UtcDateTime,
    pub last_process_failed_message: Option<String>,
    pub last_start_process_time: Option<UtcDateTime>,
    pub last_end_process_time: Option<UtcDateTime>,
    pub processing: bool,
}

impl From<ProcessorRuntimeSnapshot> for RuntimeSnapshot {
    fn from(snapshot: ProcessorRuntimeSnapshot) -> Self {
        Self {
            created_at: snapshot.created_at.into(),
            last_process_failed_message: snapshot.last_process_failed_message,
            last_start_process_time: snapshot.last_start_process_time.map(Into::into),
            last_end_process_time: snapshot.last_end_process_time.map(Into::into),
            processing: snapshot.processing,
        }
    }
}

fn select_processor_configs(
    configs: Vec<ProcessorConfig>,
    params: &QueryParams,
) -> Vec<ProcessorConfig> {
    let page = params.page.unwrap_or(0);
    let size = params.size.unwrap_or(50);
    configs
        .into_iter()
        .filter(|config| {
            params.name.as_ref().is_none_or(|name| config.name.contains(name))
        })
        .skip(page.saturating_mul(size))
        .take(size)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processor_config(name: &str) -> ProcessorConfig {
        ProcessorConfig {
            name: name.to_owned(),
            enabled: true,
            save_path: "downloads".to_owned(),
            triggers: Vec::new(),
            source: "source".to_owned(),
            item_file_resolver: "resolver".to_owned(),
            downloader: "downloader".to_owned(),
            file_mover: "mover".to_owned(),
            options: Default::default(),
            category: None,
            tags: HashSet::new(),
        }
    }

    #[test]
    fn processor_query_filters_before_pagination() {
        let configs = vec![
            processor_config("alpha"),
            processor_config("beta"),
            processor_config("alphabet"),
        ];
        let params =
            QueryParams { name: Some("alpha".to_owned()), size: Some(1), page: Some(1) };

        let selected = select_processor_configs(configs, &params);

        assert_eq!(
            selected.iter().map(|config| config.name.as_str()).collect::<Vec<_>>(),
            ["alphabet"]
        );
    }

    #[test]
    fn processor_info_exposes_failed_wrapper_state() {
        let config = processor_config("broken");
        let wrapper = ProcessorWrapper {
            name: config.name.clone(),
            processor: None,
            error_message: Some("component failed".to_owned()),
        };

        let value = source_downloader_sdk::serde_json::to_value(
            ProcessorInfo::from_config(&config, Some(&wrapper)),
        )
        .unwrap();

        assert_eq!(value["name"], "broken");
        assert_eq!(value["enabled"], true);
        assert!(value["runtime"].is_null());
        assert_eq!(value["errorMessage"], "component failed");
    }

    #[test]
    fn dry_run_options_filter_processed_by_default() {
        let default_options: CoreDryRunOptions = DryRunOptions::default().into();
        let explicit_false: CoreDryRunOptions =
            DryRunOptions { pointer: None, filter_processed: Some(false) }.into();

        assert!(default_options.filter_processed);
        assert!(!explicit_false.filter_processed);
    }

    #[test]
    fn processor_state_uses_camel_case_wire_fields() {
        let value = source_downloader_sdk::serde_json::to_value(ProcessorState::from(
            ProcessorSourceState {
                id: Some(7),
                processor_name: "processor".to_owned(),
                source_id: "source".to_owned(),
                last_pointer: source_downloader_sdk::serde_json::json!({"page": 3}),
                last_active_time: None,
                retry_times: 0,
            },
        ))
        .unwrap();

        assert_eq!(value["sourceId"], "source");
        assert_eq!(value["pointer"]["page"], 3);
        assert!(value["lastActiveTime"].is_null());
        assert_eq!(value["retryTimes"], 0);
    }
}

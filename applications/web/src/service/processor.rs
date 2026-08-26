use crate::ApplicationContext;
use crate::error_handle::AppError;
use crate::service::processing::ProcessingContentDetail;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{
    Response,
    sse::{Event, KeepAlive, Sse},
};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use source_downloader_core::application::CoreApplication;
use source_downloader_core::compatibility::ProcessorCompatibilityReport;
use source_downloader_core::config::ProcessorConfig;
use source_downloader_core::processor_manager::ProcessorWrapper;
use source_downloader_core::processor_run_manager::{
    ProcessorRunEvent, ProcessorRunSnapshot,
};
#[cfg(test)]
use source_downloader_core::source_processor::DryRunErrorKind;
use source_downloader_core::source_processor::{
    DryRunError, DryRunEvent, DryRunItemErrorAction, DryRunOptions as CoreDryRunOptions,
    DryRunSummary, ProcessorContentDeletion, ProcessorRuntimeSnapshot, SourceProcessor,
};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::ProcessTask;
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::storage::ProcessorSourceState;
use source_downloader_sdk::time::OffsetDateTime;
use std::collections::HashSet;
use std::sync::Arc;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/processor",
            Router::new()
                .route("/validate", post(validate_processor))
                .route(
                    "/{name}",
                    get(get_processor).put(update_processor).delete(delete_processor),
                )
                .route("/runs", get(list_runs))
                .route("/runs/events", get(run_events))
                .route("/runs/{id}", get(get_run).delete(cancel_run))
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
async fn list_runs(
    State(core): State<Arc<CoreApplication>>,
) -> Json<Vec<ProcessorRunSnapshot>> {
    Json(core.run_manager.list())
}
async fn get_run(
    State(core): State<Arc<CoreApplication>>,
    Path(id): Path<u64>,
) -> Result<Json<ProcessorRunSnapshot>, AppError> {
    core.run_manager
        .get(id)
        .map(Json)
        .ok_or_else(|| AppError::NotFound("Run not found".to_owned()))
}
async fn cancel_run(
    State(core): State<Arc<CoreApplication>>,
    Path(id): Path<u64>,
) -> Result<StatusCode, AppError> {
    if core.run_manager.cancel(id) {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(AppError::NotFound("Run not found or already terminal".to_owned()))
    }
}
async fn run_events(
    State(core): State<Arc<CoreApplication>>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, std::convert::Infallible>>> {
    let receiver = core.run_manager.subscribe();
    let initial = ProcessorRunEvent::Resync { runs: core.run_manager.list() };
    let stream = futures_util::stream::unfold(
        (receiver, core.run_manager.clone(), Some(initial)),
        |(mut receiver, manager, pending)| async move {
            let event = if let Some(event) = pending {
                event
            } else {
                match receiver.recv().await {
                    Ok(event) => event,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        ProcessorRunEvent::Resync { runs: manager.list() }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            };
            Some((
                Ok(Event::default().event("processor-run").json_data(event).unwrap()),
                (receiver, manager, None),
            ))
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
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

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum DryRunEventResponse {
    Item {
        content: ProcessingContentDetail,
    },
    ItemError {
        item_hash: String,
        item: SourceItem,
        error: DryRunError,
        action: DryRunItemErrorAction,
    },
    Complete {
        summary: DryRunSummary,
    },
    RunError {
        error: DryRunError,
    },
}

impl From<DryRunEvent> for DryRunEventResponse {
    fn from(event: DryRunEvent) -> Self {
        match event {
            DryRunEvent::Item { result } => Self::Item {
                content: ProcessingContentDetail::new(
                    result.processing_content,
                    result.file_contents,
                ),
            },
            DryRunEvent::ItemError { item_hash, item, error, action } => {
                Self::ItemError { item_hash, item, error, action }
            }
            DryRunEvent::Complete { summary } => Self::Complete { summary },
            DryRunEvent::RunError { error } => Self::RunError { error },
        }
    }
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
fn validate_processor_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(AppError::BadRequest("Processor name must not be blank".to_owned()));
    }
    Ok(())
}

fn validate_processor_config(
    core: &CoreApplication,
    config: &ProcessorConfig,
) -> Result<ProcessorCompatibilityReport, AppError> {
    validate_processor_name(&config.name)?;
    Ok(core.processor_manager.validate_compatibility(config))
}

fn prepare_processor(
    core: &CoreApplication,
    config: &ProcessorConfig,
) -> Result<source_downloader_core::processor_manager::PreparedProcessor, AppError> {
    validate_processor_name(&config.name)?;
    core.processor_manager.prepare_processor(config).map_err(AppError::from)
}

#[axum::debug_handler]
async fn validate_processor(
    State(core): State<Arc<CoreApplication>>,
    Json(body): Json<ProcessorConfig>,
) -> Result<Json<ProcessorCompatibilityReport>, AppError> {
    Ok(Json(validate_processor_config(&core, &body)?))
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
    let prepared = prepare_processor(&core, &body)?;
    core.config_operator.save_processor(body.clone())?;
    core.activate_processor(prepared).map_err(AppError::BadRequest)?;
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
    let prepared = prepare_processor(&core, &body)?;
    core.config_operator.save_processor(body.clone())?;
    core.activate_processor(prepared).map_err(AppError::BadRequest)?;
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
    let prepared = prepare_processor(&core, &config)?;
    core.activate_processor(prepared).map_err(AppError::BadRequest)?;
    Ok(StatusCode::NO_CONTENT)
}
#[axum::debug_handler]
async fn trigger_processor(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<ProcessorRunSnapshot>, AppError> {
    let processor = require_processor(&core, &name)?;
    Ok(Json(core.run_manager.submit_full(name, async move {
        processor.run().await.map_err(|e| e.to_string())
    })))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CollectedDryRunResponse {
    run: ProcessorRunSnapshot,
    events: Vec<DryRunEventResponse>,
}

#[axum::debug_handler]
async fn dry_run(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    options: Option<Json<DryRunOptions>>,
) -> Result<Json<CollectedDryRunResponse>, AppError> {
    let processor = require_processor(&core, &name)?;
    let options = options.map(|Json(o)| o).unwrap_or_default();
    let (run, rx) = core.run_manager.submit_dry_run_collected(name, async move {
        Ok(processor.dry_run(options.into()).await)
    });
    let events = rx
        .await
        .map_err(|_| AppError::InternalError("dry-run cancelled".to_owned()))?
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(Json(CollectedDryRunResponse { run, events }))
}

#[axum::debug_handler]
async fn dry_run_stream(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    options: Option<Json<DryRunOptions>>,
) -> Result<Response<Body>, AppError> {
    let processor = require_processor(&core, &name)?;
    let options = options.map(|Json(o)| o).unwrap_or_default();
    let (run, rx) = core.run_manager.submit_dry_run_streamed(name, async move {
        let (tx, out) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            let mut stream = Box::pin(processor.dry_run_stream(options.into()));
            while let Some(event) = stream.next().await {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });
        Ok(out)
    });
    let body = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|event| {
            let result = source_downloader_sdk::serde_json::to_vec(
                &DryRunEventResponse::from(event),
            )
            .map(|mut line| {
                line.push(b'\n');
                Bytes::from(line)
            })
            .map_err(std::io::Error::other);
            (result, rx)
        })
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header("x-run-id", run.id.to_string())
        .body(Body::from_stream(body))
        .map_err(|error| AppError::InternalError(error.to_string()))
}

#[axum::debug_handler]
async fn trigger_rename(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<ProcessorRunSnapshot>, AppError> {
    let processor = require_processor(&core, &name)?;
    Ok(Json(core.run_manager.submit_rename(name, async move {
        processor.run_rename().await.map(|_| ()).map_err(|e| e.to_string())
    })))
}

#[axum::debug_handler]
async fn post_items(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(items): Json<Vec<SourceItem>>,
) -> Result<Json<ProcessorRunSnapshot>, AppError> {
    let processor = require_processor(&core, &name)?;
    Ok(Json(core.run_manager.submit_items(name, async move {
        processor.run_items(items).await.map_err(|e| e.to_string())
    })))
}

#[axum::debug_handler]
async fn get_state(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> Result<Json<ProcessorState>, AppError> {
    let state = require_processor(&core, &name)?.source_state().await?;
    Ok(Json(state.into()))
}

fn validate_pointer_source(
    processor_name: &str,
    configured_source_id: &str,
    requested_source_id: &str,
) -> Result<(), AppError> {
    if requested_source_id != configured_source_id {
        return Err(AppError::BadRequest(format!(
            "Source {requested_source_id} is not configured for processor {processor_name}"
        )));
    }
    Ok(())
}

#[axum::debug_handler]
async fn update_pointer(
    State(core): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
    Json(body): Json<PointerPayload>,
) -> Result<(), AppError> {
    let processor = require_processor(&core, &name)?;
    validate_pointer_source(&name, processor.source_id(), &body.source_id)?;
    processor.update_source_pointer(&body.source_id, Value::Object(body.pointer)).await?;
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
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
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
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub last_process_failed_message: Option<String>,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    pub last_start_process_time: Option<OffsetDateTime>,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    pub last_end_process_time: Option<OffsetDateTime>,
    pub processing: bool,
}

impl From<ProcessorRuntimeSnapshot> for RuntimeSnapshot {
    fn from(snapshot: ProcessorRuntimeSnapshot) -> Self {
        Self {
            created_at: snapshot.created_at,
            last_process_failed_message: snapshot.last_process_failed_message,
            last_start_process_time: snapshot.last_start_process_time,
            last_end_process_time: snapshot.last_end_process_time,
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
    use super::validate_processor_name;
    use super::*;

    #[test]
    fn pointer_source_validation_rejects_unconfigured_source() {
        let error =
            validate_pointer_source("processor", "configured", "other").unwrap_err();

        assert!(matches!(error, AppError::BadRequest(message) if
            message == "Source other is not configured for processor processor"));
    }

    #[test]
    fn processor_name_validation_rejects_empty_and_whitespace_only_names() {
        assert!(validate_processor_name("").is_err());
        assert!(validate_processor_name(" \t\n").is_err());
    }

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
    fn dry_run_item_error_uses_discriminated_wire_shape() {
        let event = DryRunEventResponse::ItemError {
            item_hash: "item-hash".to_owned(),
            item: SourceItem {
                title: "failed item".to_owned(),
                link: source_downloader_sdk::http::Uri::from_static(
                    "https://example.com/item",
                ),
                datetime: OffsetDateTime::UNIX_EPOCH,
                content_type: "image".to_owned(),
                download_uri: source_downloader_sdk::http::Uri::from_static(
                    "https://example.com/file",
                ),
                attrs: Map::new(),
                tags: Vec::new(),
                identity: None,
            },
            error: DryRunError {
                message: "resolver failed".to_owned(),
                kind: DryRunErrorKind::NonRetryable,
                skippable: false,
            },
            action: DryRunItemErrorAction::Stop,
        };

        let value = source_downloader_sdk::serde_json::to_value(event).unwrap();

        assert_eq!(value["type"], "itemError");
        assert_eq!(value["itemHash"], "item-hash");
        assert_eq!(value["item"]["title"], "failed item");
        assert_eq!(value["error"]["message"], "resolver failed");
        assert_eq!(value["error"]["kind"], "nonRetryable");
        assert_eq!(value["error"]["skippable"], false);
        assert_eq!(value["action"], "stop");
    }

    #[test]
    fn runtime_snapshot_serializes_timestamps_as_rfc3339() {
        let value = source_downloader_sdk::serde_json::to_value(RuntimeSnapshot::from(
            ProcessorRuntimeSnapshot {
                created_at: OffsetDateTime::UNIX_EPOCH,
                last_process_failed_message: None,
                last_start_process_time: Some(OffsetDateTime::UNIX_EPOCH),
                last_end_process_time: None,
                processing: true,
            },
        ))
        .unwrap();

        assert_eq!(value["createdAt"], "1970-01-01T00:00:00Z");
        assert_eq!(value["lastStartProcessTime"], "1970-01-01T00:00:00Z");
        assert!(value["lastEndProcessTime"].is_null());
    }

    #[test]
    fn processor_state_uses_camel_case_wire_fields() {
        let value = source_downloader_sdk::serde_json::to_value(ProcessorState::from(
            ProcessorSourceState {
                id: Some(7),
                processor_name: "processor".to_owned(),
                source_id: "source".to_owned(),
                last_pointer: source_downloader_sdk::serde_json::json!({"page": 3}),
                last_active_time: Some(OffsetDateTime::UNIX_EPOCH),
                retry_times: 0,
            },
        ))
        .unwrap();

        assert_eq!(value["sourceId"], "source");
        assert_eq!(value["lastActiveTime"], "1970-01-01T00:00:00Z");
        assert_eq!(value["pointer"]["page"], 3);
        assert_eq!(value["retryTimes"], 0);
    }
}

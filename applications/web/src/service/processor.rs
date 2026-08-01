use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_qs::to_string;
use source_downloader_core::application::CoreApplication;
use source_downloader_core::config::ProcessorConfig;
use source_downloader_core::processor_manager::ProcessorWrapper;
use source_downloader_core::source_processor::{
    DryRunOptions as CoreDryRunOptions, DryRunResult, ProcessorRuntimeSnapshot,
    SourceProcessor,
};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::ProcessTask;
use source_downloader_sdk::serde_json::{Map, Value};
use source_downloader_sdk::time::UtcDateTime;
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
                    get(get_processor).put(update_processor).delete(delete_processor),
                )
                .route("/", get(query_processors).post(create_processor))
                .route("/{name}/reload", post(reload_processor))
                .route("/{name}/dry-run", get(dry_run).post(dry_run))
                .route("/{name}/dry-run-stream", get(dry_run_stream).post(dry_run_stream))
                .route("/{name}/trigger", post(trigger_processor))
                .route("/{name}/rename", post(trigger_rename))
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
    info!("post_items name={}, items={}", _name, to_string(&items).unwrap());
    todo!()
}

#[axum::debug_handler]
async fn get_state(
    State(_): State<Arc<CoreApplication>>,
    Path(name): Path<String>,
) -> () {
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
}

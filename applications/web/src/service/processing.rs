use crate::ApplicationContext;
use crate::error_handle::AppError;
use crate::service::component::Qs;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use source_downloader_core::source_processor::decode_files_from_compressed;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{FileContent, FileContentStatus};
use source_downloader_sdk::http::Uri;
use source_downloader_sdk::serde_json::Value;
use source_downloader_sdk::storage::{
    ItemContentCondition, ItemContentLite, ProcessingContent, ProcessingContentQuery,
    ProcessingStatus,
};
use source_downloader_sdk::time::{OffsetDateTime, UtcDateTime};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/processing-content",
            Router::new()
                .route("/{id}", get(get_content))
                .route("/", get(query_contents))
                .route("/{id}", put(update_content))
                .route("/{id}", delete(delete_content))
                .route("/{id}/reprocess", post(reprocess)),
        )
        .with_state(ctx)
}

#[axum::debug_handler]
async fn get_content(
    State(ctx): State<Arc<ApplicationContext>>,
    Path(id): Path<i64>,
) -> Result<Json<ProcessingContentDetail>, AppError> {
    Ok(Json(load_content_detail(&ctx, id).await?))
}

async fn load_content_detail(
    ctx: &ApplicationContext,
    id: i64,
) -> Result<ProcessingContentDetail, AppError> {
    let content = ctx
        .storage
        .find_content_by_id(id)
        .await
        .map_err(|error| AppError::InternalError(error.message))?
        .ok_or_else(|| {
            AppError::NotFound(format!("Processing content {id} not found"))
        })?;
    let file_contents = match ctx
        .storage
        .find_file_contents(id)
        .await
        .map_err(|error| AppError::InternalError(error.message))?
    {
        Some(bytes) => decode_files_from_compressed(&bytes)?,
        None => Vec::new(),
    };
    Ok(ProcessingContentDetail::new(content, file_contents))
}

#[axum::debug_handler]
async fn query_contents(
    State(ctx): State<Arc<ApplicationContext>>,
    Qs(query): Qs<QueryContents>,
) -> Result<Json<Scroll>, AppError> {
    let max_id = query.max_id.filter(|id| *id > 0);
    let statuses = query.status.as_deref().map(parse_processing_statuses).transpose()?;
    let contents = ctx
        .storage
        .query_processing_content(&ProcessingContentQuery {
            id: query.id,
            processor_name: query.processor_name,
            item_hash: query.item_hash.map(|hash| vec![hash]),
            status: statuses,
            item: merge_item_condition(query.item, query.item_title).map(Into::into),
            created_at_start: query.create_time_begin.map(Into::into),
            created_at_end: query.create_time_end.map(Into::into),
            max_id,
            limit: Some(query.limit),
            ..Default::default()
        })
        .await
        .map_err(|error| AppError::InternalError(error.message))?;
    let next_max_id =
        contents.last().and_then(|content| content.id).unwrap_or(max_id.unwrap_or(0));
    Ok(Json(Scroll {
        contents: contents.into_iter().map(ProcessingContentSummary::from).collect(),
        next_max_id,
    }))
}

#[axum::debug_handler]
async fn update_content(
    State(ctx): State<Arc<ApplicationContext>>,
    Path(id): Path<i64>,
    Json(body): Json<UpdateContent>,
) -> Result<Json<ProcessingContentDetail>, AppError> {
    let mut content = ctx
        .storage
        .find_content_by_id(id)
        .await
        .map_err(|error| AppError::InternalError(error.message))?
        .ok_or_else(|| {
            AppError::NotFound(format!("Processing content {id} not found"))
        })?;
    if let Some(status) = body.status {
        content.status = parse_processing_status(&status)?;
    }
    if let Some(rename_times) = body.rename_times {
        content.rename_times = rename_times;
    }
    content.updated_at = Some(OffsetDateTime::now_utc());
    ctx.storage
        .save_processing_content(&content)
        .await
        .map_err(|error| AppError::InternalError(error.message))?;
    Ok(Json(load_content_detail(&ctx, id).await?))
}

#[axum::debug_handler]
async fn delete_content(
    State(ctx): State<Arc<ApplicationContext>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    ctx.storage
        .delete_processing_content(id)
        .await
        .map_err(|error| AppError::InternalError(error.message))?;
    Ok(StatusCode::NO_CONTENT)
}

#[axum::debug_handler]
async fn reprocess(
    State(ctx): State<Arc<ApplicationContext>>,
    Path(id): Path<i64>,
) -> Result<
    Json<source_downloader_core::processor_run_manager::ProcessorRunSnapshot>,
    AppError,
> {
    let content = ctx
        .storage
        .find_content_by_id(id)
        .await
        .map_err(|error| AppError::InternalError(error.message))?
        .ok_or_else(|| {
            AppError::NotFound(format!("Processing content {id} not found"))
        })?;
    let processor_name = content.processor_name.clone();
    let processor = ctx
        .core
        .processor_manager
        .get_processor(&processor_name)
        .and_then(|wrapper| wrapper.processor.clone())
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "Processor {processor_name} not found or unavailable"
            ))
        })?;
    let snapshot = ctx.core.run_manager.submit_reprocess(processor_name, async move {
        processor.reprocess(content).await.map_err(|e| e.to_string())
    });
    Ok(Json(snapshot))
}

fn parse_processing_statuses(
    values: &[String],
) -> Result<Vec<ProcessingStatus>, AppError> {
    values.iter().map(|value| parse_processing_status(value)).collect()
}

fn parse_processing_status(value: &str) -> Result<ProcessingStatus, AppError> {
    let status = match value {
        "WAITING_TO_RENAME" | "WaitingToRename" => ProcessingStatus::WaitingToRename,
        "FILTERED" | "Filtered" => ProcessingStatus::Filtered,
        "DOWNLOAD_FAILED" | "DownloadFailed" => ProcessingStatus::DownloadFailed,
        "TARGET_ALREADY_EXISTS" | "TargetAlreadyExists" => {
            ProcessingStatus::TargetAlreadyExists
        }
        "RENAMED" | "Renamed" => ProcessingStatus::Renamed,
        "NO_FILES" | "NoFiles" => ProcessingStatus::NoFiles,
        "FAILURE" | "Failure" => ProcessingStatus::Failure,
        "CANCELLED" | "Cancelled" => ProcessingStatus::Cancelled,
        _ => {
            return Err(AppError::BadRequest(format!(
                "Unknown processing status: {value}"
            )));
        }
    };
    Ok(status)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Scroll {
    contents: Vec<ProcessingContentSummary>,
    next_max_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProcessingContentSummary {
    id: Option<i64>,
    processor_name: String,
    item_hash: String,
    item_identity: Option<String>,
    item_content: ItemContentSummary,
    rename_times: u32,
    status: ProcessingStatus,
    failure_reason: Option<String>,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    updated_at: Option<OffsetDateTime>,
}

impl From<ProcessingContent> for ProcessingContentSummary {
    fn from(content: ProcessingContent) -> Self {
        let ItemContentLite { source_item, item_variables } = content.item_content;
        Self {
            id: content.id,
            processor_name: content.processor_name,
            item_hash: content.item_hash,
            item_identity: content.item_identity,
            item_content: ItemContentSummary {
                source_item: source_item.into(),
                item_variables: item_variables.into_iter().collect(),
            },
            rename_times: content.rename_times,
            status: content.status,
            failure_reason: content.failure_reason,
            created_at: content.created_at,
            updated_at: content.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemContentSummary {
    source_item: SourceItemResponse,
    item_variables: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceItemResponse {
    title: String,
    link: String,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    datetime: OffsetDateTime,
    content_type: String,
    download_uri: String,
    attrs: BTreeMap<String, Value>,
    tags: Vec<String>,
    identity: Option<String>,
}

impl From<SourceItem> for SourceItemResponse {
    fn from(item: SourceItem) -> Self {
        Self {
            title: item.title,
            link: item.link.to_string(),
            datetime: item.datetime,
            content_type: item.content_type,
            download_uri: item.download_uri.to_string(),
            attrs: item.attrs.into_iter().collect(),
            tags: item.tags,
            identity: item.identity,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessingContentDetail {
    id: Option<i64>,
    processor_name: String,
    item_hash: String,
    item_identity: Option<String>,
    item_content: ItemContentDetail,
    rename_times: u32,
    status: ProcessingStatus,
    failure_reason: Option<String>,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    updated_at: Option<OffsetDateTime>,
}

impl ProcessingContentDetail {
    pub fn new(content: ProcessingContent, file_contents: Vec<FileContent>) -> Self {
        let ItemContentLite { source_item, item_variables } = content.item_content;
        Self {
            id: content.id,
            processor_name: content.processor_name,
            item_hash: content.item_hash,
            item_identity: content.item_identity,
            item_content: ItemContentDetail {
                source_item: source_item.into(),
                file_contents: file_contents.into_iter().map(Into::into).collect(),
                item_variables: item_variables.into_iter().collect(),
            },
            rename_times: content.rename_times,
            status: content.status,
            failure_reason: content.failure_reason,
            created_at: content.created_at,
            updated_at: content.updated_at,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemContentDetail {
    source_item: SourceItemResponse,
    file_contents: Vec<FileContentResponse>,
    item_variables: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileContentResponse {
    download_path: PathBuf,
    file_download_path: PathBuf,
    source_save_path: PathBuf,
    pattern_variables: BTreeMap<String, String>,
    file_save_path_pattern: String,
    filename_pattern: String,
    tags: Vec<String>,
    attrs: BTreeMap<String, Value>,
    file_uri: Option<String>,
    target_save_path: PathBuf,
    target_filename: String,
    exist_target_path: Option<PathBuf>,
    errors: Vec<String>,
    status: FileContentStatus,
    processed_variables: Option<BTreeMap<String, String>>,
}

impl From<FileContent> for FileContentResponse {
    fn from(file: FileContent) -> Self {
        Self {
            download_path: file.download_path,
            file_download_path: file.file_download_path,
            source_save_path: file.source_save_path,
            pattern_variables: file.pattern_variables.into_iter().collect(),
            file_save_path_pattern: file.file_save_path_pattern,
            filename_pattern: file.filename_pattern,
            tags: file.tags,
            attrs: file.attrs.into_iter().collect(),
            file_uri: file.file_uri.map(|uri: Uri| uri.to_string()),
            target_save_path: file.target_save_path,
            target_filename: file.target_filename,
            exist_target_path: file.exist_target_path,
            errors: file.errors,
            status: file.status,
            processed_variables: file
                .processed_variables
                .map(|variables| variables.into_iter().collect()),
        }
    }
}

fn default_limit() -> u64 {
    20
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemCondition {
    title: Option<String>,
    attrs: Option<HashMap<String, String>>,
    variables: Option<HashMap<String, String>>,
    content_type: Option<String>,
    tags: Option<Vec<String>>,
}

impl From<ItemCondition> for ItemContentCondition {
    fn from(condition: ItemCondition) -> Self {
        Self {
            title: condition.title,
            attrs: condition.attrs,
            variables: condition.variables,
            content_type: condition.content_type,
            tags: condition.tags,
        }
    }
}

fn merge_item_condition(
    item: Option<ItemCondition>,
    title: Option<String>,
) -> Option<ItemCondition> {
    match (item, title) {
        (Some(mut item), Some(title)) => {
            item.title = Some(title);
            Some(item)
        }
        (None, Some(title)) => Some(ItemCondition {
            title: Some(title),
            attrs: None,
            variables: None,
            content_type: None,
            tags: None,
        }),
        (item, None) => item,
    }
}

#[derive(Deserialize)]
struct QueryContents {
    #[serde(default = "default_limit")]
    limit: u64,
    #[serde(rename = "maxId")]
    max_id: Option<i64>,
    #[serde(rename = "processorName")]
    processor_name: Option<Vec<String>>,
    status: Option<Vec<String>>,
    id: Option<Vec<i64>>,
    #[serde(rename = "itemHash")]
    item_hash: Option<String>,
    #[serde(rename = "createTime.begin")]
    create_time_begin: Option<UtcDateTime>,
    #[serde(rename = "createTime.end")]
    create_time_end: Option<UtcDateTime>,
    item: Option<ItemCondition>,
    #[serde(rename = "item.title")]
    item_title: Option<String>,
}

#[derive(Deserialize)]
struct UpdateContent {
    #[serde(rename = "renameTimes")]
    rename_times: Option<u32>,
    #[serde(rename = "status")]
    status: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn processing_content() -> ProcessingContent {
        ProcessingContent {
            id: Some(7),
            processor_name: "processor".to_owned(),
            item_hash: "hash".to_owned(),
            item_identity: None,
            item_content: ItemContentLite {
                source_item: SourceItem {
                    title: "title".to_owned(),
                    link: "https://example.com".parse().unwrap(),
                    datetime: OffsetDateTime::now_utc(),
                    content_type: "text/plain".to_owned(),
                    download_uri: "https://example.com/file".parse().unwrap(),
                    attrs: Default::default(),
                    tags: Default::default(),
                    identity: None,
                },
                item_variables: HashMap::new(),
            },
            rename_times: 1,
            status: ProcessingStatus::Renamed,
            failure_reason: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: Some(OffsetDateTime::UNIX_EPOCH),
        }
    }

    #[test]
    fn processing_responses_sort_variables_and_attrs_by_key() {
        let mut content = processing_content();
        content.item_content.source_item.attrs.insert("zeta".to_owned(), Value::from(1));
        content.item_content.source_item.attrs.insert("alpha".to_owned(), Value::from(2));
        content.item_content.item_variables.insert("zeta".to_owned(), "1".to_owned());
        content.item_content.item_variables.insert("alpha".to_owned(), "2".to_owned());

        let summary = source_downloader_sdk::serde_json::to_string(
            &ProcessingContentSummary::from(content.clone()),
        )
        .unwrap();
        assert_key_precedes(&summary, "alpha", "zeta");

        let mut pattern_variables = HashMap::new();
        pattern_variables.insert("zeta".to_owned(), "1".to_owned());
        pattern_variables.insert("alpha".to_owned(), "2".to_owned());
        let mut attrs = source_downloader_sdk::serde_json::Map::new();
        attrs.insert("zeta".to_owned(), Value::from(1));
        attrs.insert("alpha".to_owned(), Value::from(2));
        let file = FileContent {
            download_path: PathBuf::new(),
            file_download_path: PathBuf::new(),
            source_save_path: PathBuf::new(),
            pattern_variables: pattern_variables.clone(),
            file_save_path_pattern: String::new(),
            filename_pattern: String::new(),
            tags: Vec::new(),
            attrs,
            file_uri: None,
            target_save_path: PathBuf::new(),
            target_filename: String::new(),
            exist_target_path: None,
            errors: Vec::new(),
            status: FileContentStatus::Undetected,
            target_path: std::sync::OnceLock::new(),
            data: None,
            processed_variables: Some(pattern_variables),
        };
        let detail = source_downloader_sdk::serde_json::to_string(
            &ProcessingContentDetail::new(content, vec![file]),
        )
        .unwrap();
        assert_eq!(detail.matches("\"alpha\"").count(), 5);
        assert_eq!(detail.matches("\"zeta\"").count(), 5);
        for object in ["itemVariables", "attrs", "patternVariables", "processedVariables"]
        {
            let start = detail.find(&format!("\"{object}\"")).unwrap();
            assert_key_precedes(&detail[start..], "alpha", "zeta");
        }
    }

    fn assert_key_precedes(json: &str, first: &str, second: &str) {
        assert!(
            json.find(&format!("\"{first}\"")).unwrap()
                < json.find(&format!("\"{second}\"")).unwrap()
        );
    }

    #[test]
    fn dotted_item_title_query_is_parsed() {
        let query = serde_qs::from_str::<QueryContents>("item.title=Selected").unwrap();
        let item = merge_item_condition(query.item, query.item_title);

        assert_eq!(
            item.and_then(|condition| condition.title),
            Some("Selected".to_owned())
        );
    }

    #[test]
    fn list_omits_files_while_detail_includes_them() {
        let summary = source_downloader_sdk::serde_json::to_value(
            ProcessingContentSummary::from(processing_content()),
        )
        .unwrap();
        let detail = source_downloader_sdk::serde_json::to_value(
            ProcessingContentDetail::new(processing_content(), Vec::new()),
        )
        .unwrap();

        assert!(summary["itemContent"].get("fileContents").is_none());
        assert_eq!(
            detail["itemContent"]["fileContents"],
            source_downloader_sdk::serde_json::json!([])
        );
        assert_eq!(summary["processorName"], "processor");
        assert!(summary.get("processor_name").is_none());
        assert_eq!(summary["createdAt"], "1970-01-01T00:00:00Z");
        assert_eq!(summary["updatedAt"], "1970-01-01T00:00:00Z");
        assert_eq!(detail["createdAt"], "1970-01-01T00:00:00Z");
        assert_eq!(detail["updatedAt"], "1970-01-01T00:00:00Z");
    }

    #[test]
    fn processing_status_accepts_kotlin_wire_names() {
        assert_eq!(
            parse_processing_status("WAITING_TO_RENAME").unwrap(),
            ProcessingStatus::WaitingToRename
        );
        assert!(parse_processing_status("UNKNOWN").is_err());
    }
}

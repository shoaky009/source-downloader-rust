use crate::ApplicationContext;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use core::application::CoreApplication;
use sdk::time::{OffsetDateTime, UtcDateTime};
use serde::Deserialize;
use std::sync::Arc;
use tracing::info;
use sdk::SourceItem;
use sdk::storage::{ItemContentLite, ProcessingContent, ProcessingStatus};

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
        .with_state(ctx.core.clone())
}

#[axum::debug_handler]
async fn get_content(
    State(_core): State<Arc<CoreApplication>>,
    Path(id): Path<i64>,
) -> Json<ProcessingContent> {
    info!("get_content id={}", id);
    ProcessingContent {
        id: Some(id),
        processor_name: "www".to_string(),
        item_hash: "aaa".to_string(),
        item_identity: None,
        item_content: ItemContentLite {
            source_item: SourceItem {
                title: "".to_string(),
                link: "localhost".parse().unwrap(),
                datetime: OffsetDateTime::now_utc(),
                content_type: "text".to_string(),
                download_uri: "localhost".parse().unwrap(),
                attrs: Default::default(),
                tags: Default::default(),
            },
            item_variables: Default::default(),
        },
        rename_times: 0,
        status: ProcessingStatus::Renamed,
        failure_reason: None,
        created_at: OffsetDateTime::now_utc(),
        updated_at: None,
    }
    .into()
}

#[axum::debug_handler]
async fn query_contents(
    State(_core): State<Arc<CoreApplication>>,
    Query(query): Query<QueryContents>,
) -> Json<Vec<ProcessingContent>> {
    info!(
        "query_contents limit={} offset={}",
        query.limit, query.offset
    );
    vec![].into()
}

#[axum::debug_handler]
async fn update_content(
    State(_core): State<Arc<CoreApplication>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateContent>,
) -> () {
    info!(
        "update_content id={}, status={}, renameTimes={}",
        id,
        body.status.unwrap_or("".to_string()),
        body.rename_times.unwrap_or(0)
    );
}

#[axum::debug_handler]
async fn delete_content(State(_core): State<Arc<CoreApplication>>, Path(id): Path<String>) -> () {
    info!("delete_content id={}", id);
}

#[axum::debug_handler]
async fn reprocess(State(_core): State<Arc<CoreApplication>>, Path(id): Path<String>) -> () {
    info!("reprocess id={}", id);
}

#[allow(dead_code)]
fn default_limit() -> u32 {
    20
}
#[allow(dead_code)]
fn default_offset() -> u64 {
    0
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct QueryContents {
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default = "default_offset")]
    offset: u64,
    #[serde(rename = "processorName")]
    processor_name: Option<Vec<String>>,
    status: Option<Vec<String>>,
    id: Option<Vec<String>>,
    #[serde(rename = "itemHash")]
    item_hash: Option<Vec<String>>,
    #[serde(rename = "createTime.begin")]
    create_time_begin: Option<UtcDateTime>,
    #[serde(rename = "createTime.end")]
    create_time_end: Option<UtcDateTime>,
    //TODO item condition
}

#[derive(Deserialize)]
struct UpdateContent {
    #[serde(rename = "renameTimes")]
    rename_times: Option<u32>,
    #[serde(rename = "status")]
    status: Option<String>,
}

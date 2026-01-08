use crate::ApplicationContext;
use axum::extract::State;
use axum::routing::delete;
use axum::{Json, Router};
use source_downloader_core::application::CoreApplication;
use std::sync::Arc;
use tracing::info;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/target-path",
            Router::new().route("/", delete(delete_target_paths)),
        )
        .with_state(ctx.core.clone())
}

#[axum::debug_handler]
async fn delete_target_paths(
    State(_core): State<Arc<CoreApplication>>,
    Json(_): Json<Vec<String>>,
) -> () {
    info!("delete_target_paths")
}

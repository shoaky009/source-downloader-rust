use crate::ApplicationContext;
use axum::extract::State;
use axum::routing::delete;
use axum::{Json, Router};
use core::application::CoreApplication;
use std::sync::Arc;
use tracing::info;

pub fn register_routers(core_application: Arc<ApplicationContext>) -> Router {
    let core: Arc<CoreApplication> = core_application.core.clone();
    Router::new()
        .nest(
            "/target-path",
            Router::new().route("/", delete(delete_target_paths)),
        )
        .with_state(core)
}

#[axum::debug_handler]
async fn delete_target_paths(
    State(_): State<Arc<CoreApplication>>,
    Json(_): Json<Vec<String>>,
) -> () {
    info!("delete_target_paths")
}

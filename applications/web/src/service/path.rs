use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::extract::State;
use axum::routing::delete;
use axum::{Json, Router};
use std::sync::Arc;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest("/target-path", Router::new().route("/", delete(delete_target_paths)))
        .with_state(ctx)
}

#[axum::debug_handler]
async fn delete_target_paths(
    State(ctx): State<Arc<ApplicationContext>>,
    Json(paths): Json<Vec<String>>,
) -> Result<(), AppError> {
    ctx.storage
        .delete_paths(&paths, None)
        .await
        .map_err(|error| AppError::InternalError(error.message))
}

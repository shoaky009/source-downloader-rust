use crate::ApplicationContext;
use crate::error_handle::{AppError, error_handler};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router, middleware};
use source_downloader_core::application::CoreApplication;
use source_downloader_sdk::serde_json::{Value, json};
use std::sync::Arc;

build_info::build_info!(fn build_info);
build_info::build_info! {
    #[inline]
    pub fn pub_build_info
}

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new().nest(
        "/application",
        Router::new()
            .route("/reload", post(reload_core_application))
            .route("/info", get(get_info))
            .layer(middleware::from_fn(error_handler))
            .with_state(ctx.core.clone()),
    )
}

#[axum::debug_handler]
async fn reload_core_application(
    State(core): State<Arc<CoreApplication>>,
) -> Result<StatusCode, AppError> {
    core.reload().map_err(AppError::InternalError)?;
    Ok(StatusCode::NO_CONTENT)
}

#[axum::debug_handler]
async fn get_info() -> Json<Value> {
    Json(json!({ "buildInfo": build_info() }))
}

#[cfg(test)]
mod tests {
    use super::get_info;

    #[tokio::test]
    async fn application_info_returns_structured_build_info() {
        let response = get_info().await;

        assert!(response.0["buildInfo"].is_object());
    }
}

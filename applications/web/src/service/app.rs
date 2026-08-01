use crate::ApplicationContext;
use crate::error_handle::error_handler;
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
async fn reload_core_application(State(core): State<Arc<CoreApplication>>) -> StatusCode {
    core.reload();
    StatusCode::NO_CONTENT
}

#[axum::debug_handler]
async fn get_info() -> Json<Value> {
    Json(json!({ "buildInfo": format!("{:#?}", build_info()) }))
}

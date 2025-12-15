use crate::ApplicationContext;
use crate::error_handle::error_handler;
use axum::extract::State;
use axum::routing::post;
use axum::{Router, middleware};
use core::application::CoreApplication;
use std::sync::Arc;

pub fn register_routers(core_application: Arc<ApplicationContext>) -> Router {
    let core: Arc<CoreApplication> = core_application.core.clone();
    Router::new().nest(
        "/application",
        Router::new()
            .route("/reload", post(reload_core_application))
            .layer(middleware::from_fn(error_handler))
            .with_state(core.clone()),
    )
}

#[axum::debug_handler]
async fn reload_core_application(State(core): State<Arc<CoreApplication>>) -> () {
    core.reload();
}

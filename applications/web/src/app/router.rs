use crate::error::error_handle::error_handler;
use crate::ApplicationContext;
use axum::extract::State;
use axum::routing::get;
use axum::{middleware, Router};
use core::CoreApplication;
use std::sync::Arc;

pub fn register_routers(core_application: Arc<ApplicationContext>) -> Router {
    let core: Arc<CoreApplication> = core_application.core.clone();
    Router::new().nest(
        "/application",
        Router::new()
            .route("/reload", get(reload_core_application))
            .layer(middleware::from_fn(error_handler))
            .with_state(core.clone()),
    )
}

#[axum::debug_handler]
async fn reload_core_application(State(core): State<Arc<CoreApplication>>) -> () {
    core.reload();
}

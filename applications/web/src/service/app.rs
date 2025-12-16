use crate::ApplicationContext;
use crate::error_handle::error_handler;
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Router, middleware};
use core::application::CoreApplication;
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
async fn reload_core_application(State(core): State<Arc<CoreApplication>>) -> () {
    core.reload();
}

#[axum::debug_handler]
async fn get_info() {
    println!("{:#?}", build_info());
}

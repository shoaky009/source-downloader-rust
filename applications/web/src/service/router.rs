use crate::dao::ComponentDao;
use crate::dao::yaml_file::YamlFileDao;
use crate::error::error_handle::{AppResult, error_handler};
use crate::model::http_model::ApiResponse;
use axum::extract::State;
use axum::{Router, middleware, routing::get};
use core::CoreApplication;
use std::sync::{Arc};

pub fn register_routers(core_application: Arc<CoreApplication>) -> Router {
    let dao: Arc<dyn ComponentDao> = Arc::new(YamlFileDao::new(core_application.clone()));
    Router::new()
        .route("/", get(handler))
        .route("/suppliers", get(list_component_suppliers))
        .layer(middleware::from_fn(error_handler))
        .with_state(dao)
}

async fn handler() -> ApiResponse<String> {
    ApiResponse::success("Hello, World!".to_string())
}

#[axum::debug_handler]
async fn list_component_suppliers(
    State(dao): State<Arc<dyn ComponentDao>>,
) -> AppResult<Vec<String>> {
    // 调用 dao 的方法获取组件供应商列表
    let supplier_types = dao.list_component_suppliers()?;
    // 成功返回
    Ok(ApiResponse::success(supplier_types))
}
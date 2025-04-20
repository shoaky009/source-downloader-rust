mod model;
mod error;

use crate::CoreApplication;
use axum::extract::State;
use axum::{middleware, routing::get, Router};
use std::sync::{Arc, RwLock};

use crate::service::error::{error_handler, AppError, AppResult};
use crate::service::model::ApiResponse;

pub async fn register_routers(core_application: Arc<RwLock<CoreApplication>>) -> Router {
    Router::new()
        .route("/", get(handler))
        .route("/suppliers", get(list_component_suppliers))
        .layer(middleware::from_fn(error_handler))
        .with_state(core_application)
}

async fn handler() -> ApiResponse<String> {
    ApiResponse::success("Hello, World!".to_string())
}


#[axum::debug_handler]
async fn list_component_suppliers(
    State(core_application): State<Arc<RwLock<CoreApplication>>>,
) -> AppResult<Vec<String>> {
    // 使用 ? 运算符处理错误，会自动转换为 AppError
    let app = core_application.read().map_err(|e|
        AppError::InternalError(format!("Failed to acquire read lock: {}", e)))?;

    // 访问 component_manager
    let component_manager = app.component_manager.read().map_err(|e|
        AppError::InternalError(format!("Failed to access component manager: {}", e)))?;

    // 使用 ? 运算符优雅处理错误
    let suppliers = component_manager.get_all_suppliers().map_err(|e|
        AppError::InternalError(format!("Failed to get suppliers: {}", e)))?;

    // 处理数据
    let supplier_types: Vec<String> = suppliers
        .iter()
        .flat_map(|supplier| {
            supplier
                .supply_types()
                .iter()
                .map(|c_type| c_type.name.clone())
                .collect::<Vec<String>>()
        })
        .collect();

    // 成功返回
    Ok(ApiResponse::success(supplier_types))
}

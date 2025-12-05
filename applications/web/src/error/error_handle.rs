use crate::model::http_model::ApiResponse;
use axum::{body::Body, http::Request, middleware::Next, response::IntoResponse};
use sdk::component::ComponentError;
use std::{
    fmt,
    panic::{self, AssertUnwindSafe},
};
use tracing::log;

// 定义错误码
pub const ERROR_INTERNAL: u32 = 500;
pub const ERROR_NOT_FOUND: u32 = 404;
pub const ERROR_BAD_REQUEST: u32 = 400;
pub const ERROR_UNAUTHORIZED: u32 = 401;

// 自定义错误类型
#[derive(Debug)]
pub enum AppError {
    InternalError(String),
    NotFound(String),
    BadRequest(String),
    Unauthorized(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InternalError(msg) => write!(f, "Internal error: {}", msg),
            Self::NotFound(msg) => write!(f, "Not found: {}", msg),
            Self::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            Self::Unauthorized(msg) => write!(f, "Unauthorized: {}", msg),
        }
    }
}

impl std::error::Error for AppError {}

// 从各种标准错误类型转换到 AppError
impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        Self::InternalError(err.to_string())
    }
}

// 移除特定的 PoisonError 实现，只保留泛型版本
impl<T> From<std::sync::PoisonError<T>> for AppError {
    fn from(err: std::sync::PoisonError<T>) -> Self {
        Self::InternalError(format!("Lock poisoned: {}", err))
    }
}

// 为 ComponentError 添加转换实现
impl From<ComponentError> for AppError {
    fn from(err: ComponentError) -> Self {
        Self::InternalError(format!("Component error: {}", err))
    }
}

// 实现 IntoResponse，使 AppError 可以直接作为 Axum 响应返回
impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (code, message) = match &self {
            Self::InternalError(msg) => (ERROR_INTERNAL, msg.clone()),
            Self::NotFound(msg) => (ERROR_NOT_FOUND, msg.clone()),
            Self::BadRequest(msg) => (ERROR_BAD_REQUEST, msg.clone()),
            Self::Unauthorized(msg) => (ERROR_UNAUTHORIZED, msg.clone()),
        };

        // 记录错误日志
        log::error!("{}", self);

        // 返回 API 响应
        ApiResponse::<()>::error(code, message).into_response()
    }
}

// 定义结果类型别名，简化处理函数的返回类型
pub type AppResult<T> = Result<ApiResponse<T>, AppError>;

// 中间件函数 - 修复了类型问题
pub async fn error_handler(request: Request<Body>, next: Next) -> impl IntoResponse {
    // 使用 catch_unwind 捕获 panic
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        // 创建一个新的 future
        async { next.run(request).await }
    }));

    match result {
        Ok(future) => {
            // 正常情况，执行 future 并返回响应
            future.await.into_response()
        }
        Err(panic) => {
            // 处理 panic
            let message = if let Some(message) = panic.downcast_ref::<String>() {
                message.clone()
            } else if let Some(message) = panic.downcast_ref::<&str>() {
                message.to_string()
            } else {
                "Unknown panic occurred".to_string()
            };

            log::error!("Request handler panicked: {}", message);

            // 返回内部服务器错误
            AppError::InternalError(format!("Internal server error: {}", message)).into_response()
        }
    }
}

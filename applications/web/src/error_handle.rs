use axum::http::StatusCode;
use axum::{body::Body, http::Request, middleware::Next, response::IntoResponse};
use problem_details::ProblemDetails;
use source_downloader_sdk::component::ComponentError;
use std::{
    fmt,
    panic::{self, AssertUnwindSafe},
};
use tracing::log;

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
        Self::BadRequest(err.message)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status_code, title, detail) = match &self {
            Self::InternalError(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Internal Server Error".to_string(),
                Some(msg.clone()),
            ),
            Self::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                "Not Found".to_string(),
                Some(msg.clone()),
            ),
            Self::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                "Bad Request".to_string(),
                Some(msg.clone()),
            ),
            Self::Unauthorized(msg) => (
                StatusCode::UNAUTHORIZED,
                "Unauthorized".to_string(),
                Some(msg.clone()),
            ),
        };

        if status_code.as_u16() >= 500 {
            log::error!("{}", self);
        } else {
            log::debug!("{}", self);
        }

        let problem = ProblemDetails::from_status_code(status_code).with_title(title);
        let problem = if let Some(detail_msg) = detail {
            problem.with_detail(detail_msg)
        } else {
            problem
        };
        (status_code, axum::Json(problem)).into_response()
    }
}

pub async fn error_handler(request: Request<Body>, next: Next) -> impl IntoResponse {
    let result = panic::catch_unwind(AssertUnwindSafe(|| async { next.run(request).await }));
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
            AppError::InternalError(format!("Internal server error: {}", message)).into_response()
        }
    }
}

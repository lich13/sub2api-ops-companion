use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::fmt;

#[derive(Debug, Clone)]
pub struct Error {
    pub status: StatusCode,
    pub detail: Value,
}

#[derive(Clone)]
pub(crate) struct InternalFailure(pub Value);

pub fn install_panic_redaction() {
    // Even caught panics call this hook before unwinding. Never format their
    // payload, since it may contain configuration or an upstream response.
    std::panic::set_hook(Box::new(|_| eprintln!("Sub2Ops internal task failed")));
}

impl Error {
    pub fn new(status: u16, detail: impl Into<Value>) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            detail: detail.into(),
        }
    }

    pub fn invalid() -> Self {
        Self::new(
            422,
            json!({"code":"invalid_request","message":"请求参数无效，请刷新后重试"}),
        )
    }

    pub fn internal() -> Self {
        Self::new(
            500,
            json!({"code":"internal_error","message":"服务暂不可用"}),
        )
    }

    pub fn read_only() -> Self {
        Self::new(
            409,
            json!({"code":"read_only","message":"服务处于只读验收模式"}),
        )
    }

    pub fn conflict(message: &str) -> Self {
        Self::new(409, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The inner database/network error may contain credentials or user data.
        write!(f, "service error ({})", self.status.as_u16())
    }
}

impl std::error::Error for Error {}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"detail":self.detail}))).into_response();
        if self.status == StatusCode::INTERNAL_SERVER_ERROR
            && self.detail["code"] == "internal_error"
        {
            response
                .extensions_mut()
                .insert(InternalFailure(self.detail));
        }
        response
    }
}

impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::internal()
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::internal()
    }
}
impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::internal()
    }
}
impl From<reqwest::Error> for Error {
    fn from(_: reqwest::Error) -> Self {
        Self::new(502, "无法连接上游服务")
    }
}

pub type Result<T> = std::result::Result<T, Error>;

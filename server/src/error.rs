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
    use tracing_subscriber::prelude::*;
    let format = tracing_subscriber::fmt::layer()
        .compact()
        .without_time()
        .with_target(false)
        .with_writer(std::io::stderr);
    let _ = tracing_subscriber::registry()
        .with(format)
        .with(
            tracing_subscriber::filter::Targets::new()
                .with_target("sub2ops_server", tracing::Level::INFO),
        )
        .try_init();
    std::panic::set_hook(Box::new(|info| {
        if let Some(loc) = info.location() {
            let file = loc.file().rsplit(['/', '\\']).next().unwrap_or("unknown");
            tracing::error!(file, line = loc.line(), "Sub2Ops internal task failed");
        } else {
            tracing::error!("Sub2Ops internal task failed");
        }
    }));
}

impl Error {
    pub fn new(status: u16, detail: impl Into<Value>) -> Self {
        Self {
            status: StatusCode::from_u16(status).expect("valid HTTP error status"),
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
    fn from(error: std::io::Error) -> Self {
        tracing::error!(category = "io", kind = ?error.kind(), "service_error");
        Self::internal()
    }
}
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        tracing::error!(category = "json", kind = ?error.classify(), "service_error");
        Self::internal()
    }
}
impl From<sqlx::Error> for Error {
    fn from(error: sqlx::Error) -> Self {
        let category = match error {
            sqlx::Error::PoolTimedOut => "db_timeout",
            sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolClosed => "db_connection",
            sqlx::Error::Decode(_)
            | sqlx::Error::ColumnDecode { .. }
            | sqlx::Error::ColumnNotFound(_) => "db_decode",
            _ => "db_query",
        };
        tracing::error!(category, "service_error");
        Self::internal()
    }
}
impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        let category = if error.is_timeout() {
            "upstream_timeout"
        } else if error.is_connect() {
            "upstream_connect"
        } else if error.is_decode() {
            "upstream_decode"
        } else {
            "upstream_http"
        };
        tracing::warn!(category, "service_error");
        Self::new(502, "无法连接上游服务")
    }
}

pub type Result<T> = std::result::Result<T, Error>;

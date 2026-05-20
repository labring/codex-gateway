use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{message}")]
    Http { status: StatusCode, message: String },
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Runtime(#[from] crate::runtime::RuntimeError),
    #[error("Background task channel closed")]
    ChannelClosed,
}

impl AppError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub fn gone(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::GONE,
            message: message.into(),
        }
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Http {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    pub fn status_code(&self) -> StatusCode {
        match self {
            Self::Http { status, .. } => *status,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let payload = Json(json!({
            "error": self.to_string(),
        }));
        (status, payload).into_response()
    }
}

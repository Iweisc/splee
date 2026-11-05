use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("Not signed in")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("Not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    PaymentRequired(String),
    #[error("{0}")]
    TooManyRequests(String),
    #[error("{0}")]
    NotConfigured(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::NotFound,
            sqlx::Error::Database(ref db) if db.code().as_deref() == Some("23505") => {
                AppError::Conflict("Already exists".into())
            }
            other => AppError::Internal(other.into()),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::Conflict(_) => StatusCode::CONFLICT,
            AppError::PaymentRequired(_) => StatusCode::PAYMENT_REQUIRED,
            AppError::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::NotConfigured(_) => StatusCode::NOT_IMPLEMENTED,
            AppError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let message = match &self {
            AppError::Internal(_) => "Something went wrong".to_string(),
            other => other.to_string(),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

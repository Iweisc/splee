use axum::http::{HeaderValue, StatusCode, header};
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
    TooManyRequests(String),
    /// A 429 that says in Retry-After how many seconds to wait.
    #[error("{0}")]
    RetryAfter(String, u64),
    /// Well-formed but not acceptable, like a project id from another organization. The public API's 422.
    #[error("{0}")]
    Unprocessable(String),
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
            AppError::TooManyRequests(_) | AppError::RetryAfter(..) => StatusCode::TOO_MANY_REQUESTS,
            AppError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            AppError::Internal(e) => {
                tracing::error!(error = ?e, "internal error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        let retry_after = if let AppError::RetryAfter(_, secs) = &self { Some(*secs) } else { None };
        let mut res = (status, Json(json!({ "error": self.public_message() }))).into_response();
        if let Some(secs) = retry_after {
            res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        res
    }
}

impl AppError {
    /// What the caller sees. Internal errors are logged by IntoResponse and stay vague.
    pub fn public_message(&self) -> String {
        match self {
            AppError::Internal(_) => "Something went wrong".to_string(),
            other => other.to_string(),
        }
    }
}

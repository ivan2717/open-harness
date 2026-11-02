use crate::service::ServiceError;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Keep fallible handlers small; build the HTTP response only at the boundary.
pub(super) enum RequestError {
    Body(StatusCode, &'static str),
    Service(ServiceError),
}

impl From<ServiceError> for RequestError {
    fn from(error: ServiceError) -> Self {
        Self::Service(error)
    }
}

impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        match self {
            Self::Body(status, message) => {
                (status, Json(serde_json::json!({"error": message}))).into_response()
            }
            Self::Service(error) => error.into_response(),
        }
    }
}

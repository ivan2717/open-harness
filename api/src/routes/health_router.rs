use axum::{Router, routing::get};

pub(super) fn health_router() -> Router {
    Router::new().route("/health", get(|| async { "OK" }))
}

use axum::{Router, routing::get};

pub(super) fn health_router() -> Router<crate::AppState> {
    Router::new().route("/health", get(|| async { "OK" }))
}

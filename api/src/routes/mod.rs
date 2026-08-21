use axum::Router;
mod health_router;

pub(crate) fn router() -> Router {
    Router::new().merge(health_router::health_router())
}

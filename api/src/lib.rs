mod routes;
use axum::Router;

pub fn app() -> Router {
    Router::new().nest("/api", routes::router())
}

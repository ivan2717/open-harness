mod context;
pub mod contracts;
mod demo;
mod event_store;
mod projection;
mod routes;
mod service;
mod snapshot;
mod time;
use axum::Router;
pub use service::AppState;

pub fn app(state: AppState) -> Router {
    Router::new().nest("/api", routes::router(state))
}

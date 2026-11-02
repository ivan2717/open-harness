mod context;
pub mod contracts;
mod demo;
mod event_store;
pub mod extensions;
mod input;
mod memory;
mod model;
mod projection;
mod routes;
pub mod run_view;
mod service;
mod snapshot;
mod time;
use axum::Router;
pub use service::{AppState, RecoveryIssue, RecoveryStage};

pub fn app(state: AppState) -> Router {
    Router::new().nest("/api", routes::router(state))
}

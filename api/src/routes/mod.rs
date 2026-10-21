use crate::{
    AppState,
    contracts::{AppConfig, CreateRunInput, RunDetail, RunSummary, SubmitInput},
    service::ServiceError,
};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use std::{convert::Infallible, time::Duration};
mod health_router;

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .merge(health_router::health_router())
        .route("/config", get(config))
        .route("/runs", get(list).post(create))
        .route("/runs/{id}", get(detail))
        .route("/runs/{id}/cancel", post(cancel))
        .route("/runs/{id}/input", post(input))
        .route("/runs/{id}/events", get(events))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    // IPC/CLI clients do not send Origin. Browser pages must not control a local agent.
    if request.headers().contains_key("origin") {
        return (StatusCode::FORBIDDEN, Json(serde_json::json!({"error":"Browser origins are not accepted; use the desktop bridge"}))).into_response();
    }
    if let Some(token) = state.token() {
        let expected = format!("Bearer {token}");
        if request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            != Some(expected.as_str())
        {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error":"Unauthorized"})),
            )
                .into_response();
        }
    }
    next.run(request).await
}
async fn config(State(state): State<AppState>) -> Json<AppConfig> {
    Json(state.config())
}
async fn list(State(state): State<AppState>) -> Json<Vec<RunSummary>> {
    Json(state.list())
}
async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ServiceError> {
    state.get(&id).map(Json)
}
async fn create(
    State(state): State<AppState>,
    Json(input): Json<CreateRunInput>,
) -> Result<(StatusCode, Json<RunDetail>), ServiceError> {
    state
        .create(input)
        .await
        .map(|detail| (StatusCode::CREATED, Json(detail)))
}
async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ServiceError> {
    state.cancel(&id).await.map(Json)
}
async fn input(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SubmitInput>,
) -> Result<Json<RunDetail>, ServiceError> {
    state.input(&id, input.message).await.map(Json)
}
async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ServiceError> {
    let receiver = state.subscribe(&id)?;
    let stream =
        futures_util::stream::unfold((receiver, true), |(mut receiver, first)| async move {
            if !first && receiver.changed().await.is_err() {
                return None;
            }
            let detail = receiver.borrow_and_update().clone();
            let event = Event::default()
                .event("snapshot")
                .id(detail.summary.revision.to_string())
                .json_data(detail)
                .expect("serializable snapshot");
            Some((Ok::<_, Infallible>(event), (receiver, false)))
        });
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

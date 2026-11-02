//! Original attachment upload/download. No caller-supplied path is opened.
use crate::{AppState, service::ServiceError};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State, rejection::BytesRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadQuery {
    name: String,
}

pub(super) async fn upload(
    State(state): State<AppState>,
    Query(query): Query<UploadQuery>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<agent_core::FileReference>, Response> {
    let bytes = body.map_err(|error| {
        let message = if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
            "单个原文件最多 100 MiB"
        } else {
            "无法读取附件，请重新添加"
        };
        (error.status(), Json(serde_json::json!({"error":message}))).into_response()
    })?;
    state
        .store_attachment(query.name, bytes.to_vec())
        .await
        .map(Json)
        .map_err(IntoResponse::into_response)
}

pub(super) async fn download(
    State(state): State<AppState>,
    Path((id, message, index)): Path<(String, String, usize)>,
) -> Result<Response, ServiceError> {
    let file = state.message_file(&id, &message, index).await?;
    let bytes = state.read_attachment_original(&file).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream"),
            (header::CONTENT_DISPOSITION, "attachment"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

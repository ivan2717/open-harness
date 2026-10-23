use crate::{AppState, extensions::ExtensionSettings, service::ServiceError};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstallPlugin {
    source_path: String,
    expected_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnablePlugin {
    enabled: bool,
}

pub(super) async fn install(
    State(state): State<AppState>,
    Json(input): Json<InstallPlugin>,
) -> Result<Json<ExtensionSettings>, ServiceError> {
    state
        .install_plugin(input.source_path, input.expected_id)
        .await
        .map(Json)
}

pub(super) async fn enable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<EnablePlugin>,
) -> Result<Json<ExtensionSettings>, ServiceError> {
    state.set_plugin_enabled(id, input.enabled).await.map(Json)
}

pub(super) async fn uninstall(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ExtensionSettings>, ServiceError> {
    state.uninstall_plugin(id).await.map(Json)
}

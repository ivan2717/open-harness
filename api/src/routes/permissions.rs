use super::*;
use crate::service::permissions::{RunPermissions, UpdatePermissions};

pub(super) fn router() -> Router<AppState> {
    Router::new()
        .route("/permissions", get(persistent))
        .route("/permissions/revoke", post(revoke_target))
        .route("/runs/{id}/permissions", get(current).patch(update))
        .route("/runs/{id}/permissions/revoke", post(revoke))
}

async fn persistent(
    State(state): State<AppState>,
) -> Json<Vec<crate::extensions::ControlPermission>> {
    Json(state.control_permissions(None))
}

async fn current(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunPermissions>, ServiceError> {
    state.run_permissions(&id).map(Json)
}
async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdatePermissions>,
) -> Result<Json<RunPermissions>, ServiceError> {
    state.update_permissions(&id, input).await.map(Json)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeInput {
    #[serde(default)]
    stop: bool,
}
async fn revoke(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<RevokeInput>,
) -> Result<Json<RunPermissions>, ServiceError> {
    state.revoke_permissions(&id, input.stop).await.map(Json)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeTarget {
    run_id: Option<String>,
    permission: crate::extensions::ControlPermission,
}
async fn revoke_target(
    State(state): State<AppState>,
    Json(input): Json<RevokeTarget>,
) -> Result<Json<Vec<crate::extensions::ControlPermission>>, ServiceError> {
    state
        .revoke_target_permission(input.run_id.as_deref(), &input.permission)
        .map(Json)
}

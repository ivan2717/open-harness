use crate::{
    AppState,
    contracts::{
        AppConfig, ApprovalInput, ConfigureModel, CreateRunInput, ModelInfo, ReplaceModelInput,
        ResumeRunInput, RevertInput, RunDetail, RunSummary, SubmitInput, UpdateRunModelInput,
    },
    service::ServiceError,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{delete, get, patch, post},
};
use std::{convert::Infallible, time::Duration};
mod attachments;
mod documents;
mod health_router;
mod plugins;

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .merge(health_router::health_router())
        .route(
            "/documents/extract",
            post(documents::extract).layer(DefaultBodyLimit::max(
                agent_local::documents::MAX_DOCUMENT_BYTES,
            )),
        )
        .route(
            "/attachments",
            post(attachments::upload).layer(DefaultBodyLimit::max(
                crate::service::attachments::MAX_ATTACHMENT_BYTES,
            )),
        )
        .route("/config", get(config))
        .route("/models", post(configure_model))
        .route("/models/{id}/replace", post(replace_model))
        .route("/extensions", get(extension_settings).post(save_extensions))
        .route("/extensions/skills", get(skill_catalog))
        .route(
            "/extensions/control-permissions/{id}",
            delete(revoke_control_permission),
        )
        .route("/extensions/mcp/{id}/test", post(test_mcp))
        .route("/plugins/install", post(plugins::install))
        .route("/plugins/{id}/enabled", post(plugins::enable))
        .route("/plugins/{id}", delete(plugins::uninstall))
        .route("/runs/{id}/extensions", get(run_extensions))
        .route("/runs/{id}/extensions/control", delete(revoke_control))
        .route("/runs/{id}/memory", get(run_memory))
        .route("/runs/{id}/extensions/refresh", post(refresh_run_skills))
        .route(
            "/runs/{id}/extensions/approvals/{approval}",
            post(approve_extension),
        )
        .route(
            "/runs",
            get(list)
                .post(create)
                .layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/runs/{id}", get(detail))
        .route("/runs/{id}/model", patch(update_run_model))
        .route(
            "/runs/{id}/messages/{message_id}/images/{index}",
            get(message_image),
        )
        .route(
            "/runs/{id}/messages/{message_id}/files/{index}",
            get(message_file),
        )
        .route(
            "/runs/{id}/messages/{message_id}/files/{index}/content",
            get(attachments::download),
        )
        .route("/runs/{id}/cancel", post(cancel))
        .route(
            "/runs/{id}/input",
            post(input).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route(
            "/runs/{id}/resume",
            post(resume).layer(DefaultBodyLimit::max(32 * 1024 * 1024)),
        )
        .route("/runs/{id}/workspace", get(workspace))
        .route(
            "/runs/{id}/workspace/commands/{command}/stop",
            post(stop_command),
        )
        .route("/runs/{id}/approvals/{approval}", post(approve))
        .route(
            "/runs/{id}/workspace/command-grants/revoke",
            post(revoke_command_grants),
        )
        .route("/runs/{id}/revert", post(revert))
        .route("/runs/{id}/compact", post(compact))
        .route("/runs/{id}/compact/cancel", post(cancel_compaction))
        .route("/runs/{id}/events", get(events))
        // UTF-8 file snapshots have separate byte limits; JSON escaping may
        // expand an otherwise valid 512 KiB text payload.
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
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
async fn message_image(
    State(state): State<AppState>,
    Path((id, message, index)): Path<(String, String, usize)>,
) -> Result<Json<agent_core::ModelImage>, ServiceError> {
    state.message_image(&id, &message, index).await.map(Json)
}

async fn extension_settings(
    State(state): State<AppState>,
) -> Json<crate::extensions::ExtensionSettings> {
    Json(state.extension_settings())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillCatalogQuery {
    workspace_path: Option<String>,
}

async fn skill_catalog(
    State(state): State<AppState>,
    Query(query): Query<SkillCatalogQuery>,
) -> Result<Json<crate::extensions::SkillCatalogView>, ServiceError> {
    state
        .skill_catalog(query.workspace_path.as_deref())
        .map(Json)
}
async fn save_extensions(
    State(state): State<AppState>,
    Json(config): Json<crate::extensions::ExtensionConfig>,
) -> Result<Json<crate::extensions::ExtensionSettings>, ServiceError> {
    state.save_extensions(config).map(Json)
}
async fn test_mcp(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<agent_mcp::McpCatalog>, ServiceError> {
    state.test_mcp(&id).await.map(Json)
}
async fn run_extensions(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::extensions::RunExtensions>, ServiceError> {
    state.run_extensions(&id).map(Json)
}
async fn run_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::memory::RunMemoryView>, ServiceError> {
    state.run_memory(&id).await.map(Json)
}
async fn refresh_run_skills(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::extensions::RunExtensions>, ServiceError> {
    state.refresh_run_skills(&id).map(Json)
}
async fn approve_extension(
    State(state): State<AppState>,
    Path((id, approval)): Path<(String, String)>,
    Json(input): Json<ExtensionApprovalInput>,
) -> Result<Json<crate::extensions::RunExtensions>, ServiceError> {
    match input {
        ExtensionApprovalInput::Decision { decision } => {
            state.decide_extension(&id, &approval, decision)
        }
        ExtensionApprovalInput::Legacy { allow, session } => {
            state.approve_extension_scoped(&id, &approval, allow, session)
        }
    }
    .map(Json)
}
#[derive(serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum ExtensionApprovalInput {
    Decision {
        decision: crate::extensions::ApprovalDecision,
    },
    Legacy {
        allow: bool,
        #[serde(default)]
        session: bool,
    },
}
async fn revoke_control_permission(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::extensions::ExtensionSettings>, ServiceError> {
    state.revoke_control_permission(&id).map(Json)
}
async fn revoke_control(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<crate::extensions::RunExtensions>, ServiceError> {
    state.revoke_control(&id).await.map(Json)
}
async fn configure_model(
    State(state): State<AppState>,
    Json(input): Json<ConfigureModelRequest>,
) -> Result<Json<ModelInfo>, ServiceError> {
    state
        .configure_model_for_edit(input.configuration, input.edit_source_id.as_deref())
        .await
        .map(Json)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigureModelRequest {
    #[serde(default)]
    edit_source_id: Option<String>,
    #[serde(flatten)]
    configuration: ConfigureModel,
}
async fn replace_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ReplaceModelInput>,
) -> Result<Json<ModelInfo>, ServiceError> {
    state
        .replace_model_configuration(
            &id,
            &input.replacement_model_id,
            input.allow_configuration_change,
        )
        .await
        .map(Json)
}
async fn update_run_model(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<UpdateRunModelInput>,
) -> Result<Json<RunDetail>, ServiceError> {
    state.update_run_model(&id, input).await.map(Json)
}
async fn compact(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ServiceError> {
    state.compact(&id).await.map(Json)
}
async fn cancel_compaction(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunDetail>, ServiceError> {
    state.cancel_compaction(&id).await.map(Json)
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
async fn message_file(
    State(state): State<AppState>,
    Path((id, message_id, index)): Path<(String, String, usize)>,
) -> Result<Json<agent_core::TextFileReference>, ServiceError> {
    state.message_file(&id, &message_id, index).await.map(Json)
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
    state
        .input_with_files(&id, input.message, input.files)
        .await
        .map(Json)
}
async fn resume(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<ResumeRunInput>,
) -> Result<Json<RunDetail>, ServiceError> {
    state
        .resume_with_files(&id, input.message, input.files)
        .await
        .map(Json)
}
async fn workspace(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<agent_local::WorkspaceReview>, ServiceError> {
    state.workspace_review(&id).await.map(Json)
}
async fn approve(
    State(state): State<AppState>,
    Path((id, approval)): Path<(String, String)>,
    Json(input): Json<ApprovalInput>,
) -> Result<Json<agent_local::WorkspaceReview>, ServiceError> {
    state
        .approve_command(&id, &approval, input.allow, input.scope)
        .await
        .map(Json)
}
async fn revoke_command_grants(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<agent_local::WorkspaceReview>, ServiceError> {
    state.revoke_command_grants(&id).await.map(Json)
}
async fn stop_command(
    State(state): State<AppState>,
    Path((id, command)): Path<(String, String)>,
) -> Result<Json<agent_local::WorkspaceReview>, ServiceError> {
    state.stop_command(&id, &command).await.map(Json)
}
async fn revert(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<RevertInput>,
) -> Result<Json<agent_local::WorkspaceReview>, ServiceError> {
    state.revert_file(&id, &input.path).await.map(Json)
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

//! Single-host structured concurrency over ordinary, independently persisted runs.
//! The model loop knows neither provider protocols nor desktop integrations.
use super::*;
use crate::contracts::{Delegation, ExecutionPolicy};
use agent_runtime::ToolCall;
use serde::Deserialize;
use serde_json::json;
use std::{future::Future, pin::Pin};

mod admission;
mod control;
mod lifecycle;
mod mailbox;
pub(crate) mod tools;
pub(super) use mailbox::Mailboxes;

const DEFAULT_CONCURRENCY: usize = 4;
const MAX_CONCURRENCY: usize = 8;

pub(super) fn instructions(child: bool) -> &'static str {
    if child {
        include_str!("delegate.txt")
    } else {
        include_str!("coordinator.txt")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnInput {
    task_name: String,
    task: String,
    #[serde(default)]
    write_paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlInput {
    action: String,
    agent_id: Option<String>,
    message: Option<String>,
    timeout_seconds: Option<u64>,
    offset: Option<usize>,
    limit: Option<usize>,
}

pub(super) fn validate_policy(policy: &ExecutionPolicy) -> Result<(), ServiceError> {
    if policy.max_steps == Some(0)
        || policy
            .max_agents
            .is_some_and(|n| n == 0 || n > MAX_CONCURRENCY)
    {
        return Err(ServiceError::BadRequest(
            "max_steps 必须大于 0；max_agents 必须在 1–8 之间".into(),
        ));
    }
    Ok(())
}

impl AppState {
    pub(super) fn workspace_access(&self, path: &str) -> io::Result<agent_local::WorkspaceAccess> {
        let mut access = self
            .inner
            .workspace_access
            .lock()
            .map_err(|_| io::Error::other("workspace access lock poisoned"))?;
        if let Some(existing) = access.get(path) {
            return Ok(existing.clone());
        }
        let shared = agent_local::WorkspaceAccess::new(path.into())?;
        access.insert(path.into(), shared.clone());
        Ok(shared)
    }

    pub(super) fn root_id(&self, id: &str) -> Result<String, ServiceError> {
        let summary = self.entry(id)?.snapshot.get().summary;
        Ok(summary.delegation.map_or(summary.id, |d| d.root_run_id))
    }

    fn members(&self, root: &str) -> Vec<Arc<RunEntry>> {
        let mut members: Vec<_> = self
            .inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .values()
            .filter(|entry| {
                let summary = entry.snapshot.get().summary;
                summary.id == root
                    || summary
                        .delegation
                        .as_ref()
                        .is_some_and(|d| d.root_run_id == root)
            })
            .cloned()
            .collect();
        members.sort_by_key(|entry| entry.snapshot.get().summary.created_at);
        members
    }

    fn active(entry: &RunEntry) -> bool {
        let summary = entry.snapshot.get().summary;
        entry.background.load(Ordering::Acquire)
            || entry.runtime.is_executing(&summary.id)
            || summary.status == RunStatus::Running
    }

    fn has_active_children(&self, id: &str) -> bool {
        self.inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .values()
            .any(|entry| {
                entry
                    .snapshot
                    .get()
                    .summary
                    .delegation
                    .as_ref()
                    .is_some_and(|d| d.parent_run_id == id)
                    && Self::active(entry)
            })
    }

    fn require_capacity(&self, root: &RunDetail) -> Result<(), ServiceError> {
        let limit = root
            .summary
            .execution_policy
            .max_agents
            .unwrap_or(DEFAULT_CONCURRENCY);
        let active = self
            .members(&root.summary.id)
            .iter()
            .filter(|entry| {
                entry.snapshot.get().summary.delegation.is_some() && Self::active(entry)
            })
            .count();
        if active >= limit {
            return Err(ServiceError::Conflict(format!(
                "已有 {limit} 个协作任务正在执行，请等待一个完成后再委派，不要反复重试"
            )));
        }
        Ok(())
    }

    fn require_coordinator(&self, caller: &str) -> Result<Arc<RunEntry>, ServiceError> {
        let entry = self.entry(caller)?;
        if entry.snapshot.get().summary.delegation.is_some() {
            return Err(ServiceError::BadRequest(
                "只有主 agent 可以创建、续派或停止子任务".into(),
            ));
        }
        if entry.snapshot.get().summary.status != RunStatus::Running {
            return Err(ServiceError::Conflict("主任务已停止，不能继续委派".into()));
        }
        self.ensure_accepting_work()?;
        Ok(entry)
    }

    /// Erased at the orchestration boundary to avoid recursively sized futures:
    /// execute -> tools -> spawn -> execute uses the same runtime implementation.
    pub(super) fn collaboration_call<'a>(
        &'a self,
        caller: &'a str,
        call: ToolCall,
    ) -> Pin<Box<dyn Future<Output = Result<String, ServiceError>> + Send + 'a>> {
        Box::pin(async move {
            if call.arguments.len() > 64 * 1024 {
                return Err(ServiceError::BadRequest("协作参数过大".into()));
            }
            if call.name == "spawn_agent" {
                let input =
                    serde_json::from_str(&call.arguments).map_err(ServiceError::internal)?;
                // Admission owns its short transaction even if the parent tool
                // is cancelled. Cancel/shutdown use the same gate, then stop
                // every admitted child; no half-created task loses its owner.
                let state = self.clone();
                let caller = caller.to_owned();
                return tokio::spawn(
                    async move { state.spawn_child(&caller, &call.id, input).await },
                )
                .await
                .map_err(ServiceError::internal)?
                .map(|id| json!({"agent_id":id,"status":"running"}).to_string());
            }
            let input: ControlInput =
                serde_json::from_str(&call.arguments).map_err(ServiceError::internal)?;
            self.control_agent(caller, input).await
        })
    }

    pub(super) fn reject_direct_child_input(&self, id: &str) -> Result<(), ServiceError> {
        if self.entry(id)?.snapshot.get().summary.delegation.is_some() {
            return Err(ServiceError::BadRequest(
                "请在主会话补充任务，由主 agent 协调子任务".into(),
            ));
        }
        Ok(())
    }
}

fn bounded(text: &str, bytes: usize) -> String {
    if text.len() <= bytes {
        return text.into();
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated; inspect the source run for the rest]",
        &text[..end]
    )
}

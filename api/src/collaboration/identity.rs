//! Model-facing addresses are resolved from persisted membership, never from
//! summaries. This identity is rebuilt in the agent instructions on restoration.
use super::*;
use crate::contracts::RunSummary;

pub(super) fn target(summary: &RunSummary) -> String {
    summary.delegation.as_ref().map_or_else(
        || "/root".into(),
        |delegation| format!("/root/{}", delegation.task_name),
    )
}

pub(super) fn instructions(summary: &RunSummary) -> String {
    let identity = json!({
        "self": target(summary),
        "run_id": summary.id,
        "parent": summary.delegation.as_ref().map(|_| "/root"),
        "root": "/root",
    });
    format!(
        "Host-provided collaboration identity (not conversation memory): {identity}\nUse agent_control target: \"parent\" to read or message your parent, \"self\" to read your own public transcript, or a member target returned by list/spawn. Short task names also work. Addresses resolve only within this root task; never reconstruct run IDs from summaries. A checkpoint ID is not an agent address."
    )
}

impl AppState {
    pub(super) fn resolve_agent_target(
        &self,
        caller: &str,
        address: &str,
    ) -> Result<String, ServiceError> {
        let summary = self.entry(caller)?.snapshot.get().summary;
        let root = self.root_id(caller)?;
        match address {
            "self" => return Ok(caller.into()),
            "root" | "/root" => return Ok(root),
            "parent" => {
                return summary.delegation.map(|d| d.parent_run_id).ok_or_else(|| {
                    ServiceError::BadRequest("主任务没有 parent；读取自己的记录请用 self".into())
                });
            }
            _ => {}
        }
        // Exact IDs remain accepted for old histories and UI callers. Do not
        // look up arbitrary global entries or expose other conversations.
        let members = self.members(&root);
        if let Some(entry) = members
            .iter()
            .find(|entry| entry.snapshot.get().summary.id == address)
        {
            return Ok(entry.snapshot.get().summary.id);
        }
        let matches: Vec<_> = members
            .iter()
            .filter_map(|entry| {
                let summary = entry.snapshot.get().summary;
                let delegation = summary.delegation.as_ref()?;
                (target(&summary) == address || delegation.task_name == address)
                    .then_some(summary.id)
            })
            .collect();
        match matches.as_slice() {
            [id] => Ok(id.clone()),
            _ => Err(ServiceError::BadRequest(
                "agent not found in this task; use target parent/root/self or an exact target returned by agent_control list".into(),
            )),
        }
    }
}

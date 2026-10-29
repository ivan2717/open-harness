use super::*;
use agent_runtime::{ModelFinishReason, ModelMessage, ModelRequest};
use serde_json::json;
use std::collections::HashSet;

const INSTRUCTIONS: &str = include_str!("reviewer.txt");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    decision: DecisionKind,
    risk: Risk,
    authorized: bool,
    reason: String,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum DecisionKind {
    Allow,
    Manual,
}
#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Risk {
    Low,
    Medium,
    High,
    Unknown,
}

fn parse(output: &agent_runtime::ModelOutput) -> io::Result<Decision> {
    if output.response.finish_reason != ModelFinishReason::Stop
        || !output.response.calls().is_empty()
    {
        return Err(io::Error::other("审查返回不完整，需手动确认。"));
    }
    let decision: Decision = serde_json::from_str(output.response.text().trim())
        .map_err(|_| io::Error::other("审查结果无法验证，需手动确认。"))?;
    if decision.reason.trim().is_empty() || decision.reason.len() > 2_000 {
        return Err(io::Error::other("审查理由无效，需手动确认。"));
    }
    Ok(decision)
}

enum Pending {
    Command(agent_local::PendingApproval),
    Mcp(crate::extensions::ExtensionApproval),
}
impl Pending {
    fn id(&self) -> &str {
        match self {
            Self::Command(p) => &p.id,
            Self::Mcp(p) => &p.id,
        }
    }
    fn operation(&self) -> String {
        match self {
            Self::Command(_) => "命令联网".into(),
            Self::Mcp(p) => p.operation.clone(),
        }
    }
    fn evidence(&self) -> serde_json::Value {
        match self {
            Self::Command(p) => {
                json!({"kind":"command", "command":p.command, "cwd":p.cwd, "access":p.access, "network_access":p.network_access, "sandbox":p.execution_mode})
            }
            Self::Mcp(p) => {
                json!({"kind":"mcp", "server":p.server_name, "operation":p.operation, "arguments":p.arguments, "note":"This server has its own permissions; the command sandbox does not constrain it."})
            }
        }
    }
}

/// Runs alongside execution, not alongside every model request. Dropping this
/// future cancels the review HTTP request and cannot grant a permission.
impl AppState {
    pub(in crate::service) async fn review_pending_loop(&self, id: &str) {
        let mut attempted = HashSet::new();
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let Ok(root) = self.permission_root(id) else {
                return;
            };
            let Ok(root_detail) = self.get(&root) else {
                return;
            };
            if root_detail.summary.execution_policy.approval_mode != ApprovalMode::AutoReview {
                continue;
            }
            let Ok(entry) = self.entry(id) else { return };
            let mut pending = entry
                .workspace
                .as_ref()
                .and_then(|w| w.pending_approvals().ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|p| p.review_mode == ApprovalMode::AutoReview)
                .map(Pending::Command)
                .collect::<Vec<_>>();
            let extensions = self.inner.extensions.run_state(id, None);
            // Computer/browser app and site consent always remains directly
            // user-owned. Ordinary MCP calls are eligible for risk review.
            pending.extend(
                extensions
                    .approvals
                    .into_iter()
                    .filter(|p| {
                        p.session_scope.is_none()
                            && !self.inner.extensions.is_control_server(&p.server_id)
                    })
                    .map(Pending::Mcp),
            );
            for pending in pending {
                if !attempted.insert(pending.id().to_owned()) {
                    continue;
                }
                let already_reviewed = self
                    .inner
                    .permission_records
                    .get(&root)
                    .map(|r| {
                        r.reviews
                            .iter()
                            .any(|review| review.id == pending.id() && review.run_id == id)
                    })
                    .unwrap_or(true);
                if already_reviewed {
                    continue;
                }
                if let Err(error) = self.review_one(&root, id, pending).await {
                    tracing::warn!(%error, "approval review left for manual confirmation");
                }
            }
        }
    }

    async fn review_one(&self, root: &str, id: &str, pending: Pending) -> io::Result<()> {
        let root_detail = self
            .get(root)
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        let user_evidence = user_messages(&root_detail);
        let entry = self
            .entry(id)
            .map_err(|e| io::Error::other(format!("{e:?}")))?;
        let model = entry.runtime.model().clone();
        let mut record = ApprovalReview {
            id: pending.id().into(),
            run_id: id.into(),
            operation: pending.operation(),
            status: "reviewing".into(),
            reason: "正在审查本次操作的风险和用户授权。".into(),
            model_id: entry.snapshot.get().summary.model_id,
            created_at: time::now(),
            usage: None,
        };
        self.inner.permission_records.record(root, record.clone())?;
        let _guard = ReviewGuard {
            records: &self.inner.permission_records,
            root,
            record: record.clone(),
        };
        // User messages are the authority. Tool output and delegated prompts
        // are evidence only; no raw files, images, secrets or private reasoning
        // are copied into this review conversation.
        let users: Vec<_> = root_detail
            .messages
            .iter()
            .rev()
            .filter(|m| m.role == "user")
            .take(6)
            .map(|m| json!({"role":"user", "text":truncate(&m.text, 3_000)}))
            .collect();
        let detail = entry.snapshot.get();
        let evidence = pending.evidence();
        let evidence_text = evidence.to_string();
        let result = if evidence_text.len() > 24_000 {
            Err(io::Error::other(
                "操作参数较长，请手动确认；不会截断参数后自动放行。",
            ))
        } else {
            let request = ModelRequest {
                instructions: INSTRUCTIONS.into(),
                messages: vec![ModelMessage::user(json!({
                    "root_user_goal": truncate(&root_detail.summary.prompt, 4_000),
                    "recent_user_messages_newest_first": users,
                    "delegated_task_evidence_not_authorization": if root == id { None } else { Some(truncate(&detail.summary.prompt, 4_000)) },
                    "proposed_action": evidence,
                }).to_string())],
                tools: Vec::new(), provider_context: None,
                max_output_tokens: Some(4_096.min(model.context_metadata().and_then(|m| m.max_output_tokens).unwrap_or(4_096))),
            };
            match tokio::time::timeout(
                Duration::from_secs(30),
                model.respond_with_metadata(request),
            )
            .await
            {
                Ok(Ok(output)) => {
                    record.usage = output.usage;
                    parse(&output)
                }
                Ok(Err(_)) => Err(io::Error::other("审查模型暂不可用，请手动确认。")),
                Err(_) => Err(io::Error::other("审查超时，请手动确认。")),
            }
        };
        match result {
            Ok(decision)
                if decision.decision == DecisionKind::Allow
                    && decision.risk == Risk::Low
                    && decision.authorized =>
            {
                let current = self
                    .get(root)
                    .map_err(|e| io::Error::other(format!("{e:?}")))?;
                if user_messages(&current) != user_evidence {
                    record.status = "manual".into();
                    record.reason = "用户输入已更新，请根据最新要求手动确认。".into();
                } else if current.summary.execution_policy.approval_mode != ApprovalMode::AutoReview
                {
                    record.status = "manual".into();
                    record.reason = "审批模式已变更，请手动确认。".into();
                } else {
                    // Store the review before effects; review records are not
                    // reusable grants. Only the exact still-pending action runs.
                    record.status = "approved".into();
                    record.reason = decision.reason;
                    self.inner.permission_records.record(root, record.clone())?;
                    let approval = match pending {
                        Pending::Command(p) => entry
                            .workspace
                            .as_ref()
                            .ok_or_else(|| io::Error::other("workspace unavailable"))?
                            .approve_if_current(&p.id, true, CommandApprovalScope::Once, p.epoch),
                        Pending::Mcp(p) => self.inner.extensions.approvals.decide(id, &p.id, true),
                    };
                    if approval.is_err() {
                        record.status = "cancelled".into();
                        record.reason = "请求已完成或授权已撤销，审查结果未执行。".into();
                    }
                }
            }
            Ok(decision) => {
                record.status = "manual".into();
                record.reason = decision.reason;
            }
            Err(error) => {
                record.status = "manual".into();
                record.reason = error.to_string();
            }
        }
        self.inner.permission_records.record(root, record)
    }
}

fn truncate(value: &str, chars: usize) -> String {
    if value.chars().count() <= chars {
        value.into()
    } else {
        format!(
            "{}\n[Earlier text truncated; do not assume missing authorization.]",
            value.chars().take(chars).collect::<String>()
        )
    }
}

fn user_messages(detail: &RunDetail) -> Vec<(String, String)> {
    detail
        .messages
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| (m.id.clone(), m.text.clone()))
        .collect()
}

struct ReviewGuard<'a> {
    records: &'a PermissionRecords,
    root: &'a str,
    record: ApprovalReview,
}
impl Drop for ReviewGuard<'_> {
    fn drop(&mut self) {
        if self.records.get(self.root).is_ok_and(|records| {
            records.reviews.iter().any(|r| {
                r.id == self.record.id && r.run_id == self.record.run_id && r.status == "reviewing"
            })
        }) {
            self.record.status = "manual".into();
            self.record.reason = "审查已中断，未自动批准操作。".into();
            let _ = self.records.record(self.root, self.record.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_or_unstructured_review_never_grants_permission() {
        let mut output: agent_runtime::ModelOutput = agent_runtime::ModelResponse::final_answer(r#"{"decision":"allow","risk":"low","authorized":true,"reason":"Read public documentation"}"#.into()).into();
        assert!(parse(&output).is_ok());
        output.response.finish_reason = ModelFinishReason::Length;
        assert!(parse(&output).is_err());
        output.response = agent_runtime::ModelResponse::final_answer("Sure, go ahead".into());
        assert!(parse(&output).is_err());
        output.response = agent_runtime::ModelResponse::final_answer(
            r#"{"decision":"allow","risk":"low","authorized":true,"reason":""}"#.into(),
        );
        assert!(parse(&output).is_err());
    }
}

use agent_core::agent::{RunEvent, RunEventEnvelope};

/// Rebuild from durable execution history, not the model's compacted context.
/// Count call declarations in their stable order, independent of result timing.
#[derive(Default)]
pub(super) struct ToolRepetitionMonitor {
    observed: usize,
    previous: Option<(String, serde_json::Value)>,
    count: u32,
    pending: Vec<RunEvent>,
    command_output: Option<(String, u64)>,
    agent_activity: Option<String>,
}

impl ToolRepetitionMonitor {
    pub(super) fn pending_warnings(&mut self, events: &[RunEventEnvelope]) -> Vec<RunEvent> {
        let new_events = &events[self.observed..];
        // The driver checks only at completed tool boundaries. Match successful
        // results first, but keep counting declarations in their original order
        // even when tools in a batch complete out of order.
        let outputs: std::collections::HashMap<_, _> = new_events
            .iter()
            .filter_map(|entry| {
                if let RunEvent::ToolCallSucceeded {
                    call_id, output, ..
                } = &entry.event
                {
                    Some((call_id.as_str(), output.as_str()))
                } else {
                    None
                }
            })
            .collect();
        for entry in new_events {
            match &entry.event {
                RunEvent::RunStarted { .. }
                | RunEvent::InputReceived { .. }
                | RunEvent::RunResumed { .. } => {
                    self.previous = None;
                    self.count = 0;
                    self.pending.clear();
                    self.command_output = None;
                    self.agent_activity = None;
                }
                RunEvent::ToolCallRequested {
                    call_id,
                    tool_name,
                    arguments,
                } => {
                    let arguments = serde_json::from_str(arguments)
                        .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
                    let output = outputs.get(call_id.as_str()).copied();
                    let progress = match tool_name.as_str() {
                        "manage_command" => self.command_wait_progress(&arguments, output),
                        "agent_control" => self.agent_wait_progress(&arguments, output),
                        _ => false,
                    };
                    if progress {
                        self.previous = None;
                        self.count = 0;
                        continue;
                    }
                    let signature = (tool_name.clone(), arguments);
                    self.count = if self.previous.as_ref() == Some(&signature) {
                        self.count.saturating_add(1)
                    } else {
                        1
                    };
                    self.previous = Some(signature);
                    if matches!(self.count, 3 | 5 | 8) {
                        self.pending.push(RunEvent::ToolLoopWarning {
                            call_id: call_id.clone(),
                            tool_name: tool_name.clone(),
                            consecutive_calls: self.count,
                        });
                    }
                }
                RunEvent::ToolLoopWarning { call_id, .. } => {
                    self.pending.retain(|event| {
                        !matches!(event,
                        RunEvent::ToolLoopWarning { call_id: pending, .. } if pending == call_id)
                    });
                }
                _ => {}
            }
        }
        self.observed = events.len();
        self.pending.clone()
    }

    /// The host reports task-scoped activity and actual blocking time. Repeated
    /// reads of an unchanged mailbox are not progress, even if the model asks
    /// for a long wait; the host may have returned immediately. A no-progress
    /// result means no other agent can advance the work and must still warn.
    fn agent_wait_progress(&mut self, arguments: &serde_json::Value, output: Option<&str>) -> bool {
        let Some(input) = arguments.as_object() else {
            return false;
        };
        if input.get("action").and_then(serde_json::Value::as_str) != Some("wait")
            || input.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "action" | "timeout_seconds" | "target" | "agent_id"
                )
            })
        {
            return false;
        }
        if input.contains_key("target") && input.contains_key("agent_id") {
            return false;
        }
        if input
            .get("target")
            .or_else(|| input.get("agent_id"))
            .is_some_and(|target| target.as_str().is_none_or(|id| id.trim().is_empty()))
        {
            return false;
        }
        let Some(timeout) = input
            .get("timeout_seconds")
            .map_or(Some(30), serde_json::Value::as_u64)
            .filter(|seconds| *seconds <= 30)
        else {
            return false;
        };
        let Some(output) =
            output.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        else {
            return false;
        };
        let wait = &output["wait"];
        let (Some(outcome), Some(token), Some(waited_ms)) = (
            wait["outcome"].as_str(),
            wait["progress_token"].as_str().filter(|s| !s.is_empty()),
            wait["waited_ms"].as_u64(),
        ) else {
            return false;
        };
        if !matches!(outcome, "message" | "activity" | "timeout" | "no_progress") {
            return false;
        }
        let changed = self.agent_activity.as_deref() != Some(token);
        self.agent_activity = Some(token.into());
        match outcome {
            "message" | "activity" => changed,
            "timeout" => timeout > 0 && waited_ms >= 1000,
            _ => false,
        }
    }

    /// Exclude a successful live-command wait/progress observation, not every
    /// tool whose name sounds like polling. Invalid/failed/completed reads,
    /// zero-wait empty polls and repeated reads of an old output page still
    /// contribute to the ordinary loop warning.
    fn command_wait_progress(
        &mut self,
        arguments: &serde_json::Value,
        output: Option<&str>,
    ) -> bool {
        let Some(input) = arguments.as_object() else {
            return false;
        };
        if input
            .keys()
            .any(|key| !matches!(key.as_str(), "command_id" | "action" | "cursor" | "wait_ms"))
        {
            return false;
        }
        let action = input
            .get("action")
            .map_or(Some("output"), serde_json::Value::as_str);
        if !matches!(action, Some("output" | "status")) {
            return false;
        }
        let Some(id) = input
            .get("command_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return false;
        };
        let cursor = input
            .get("cursor")
            .map_or(Some(0), serde_json::Value::as_u64);
        let wait_ms = input
            .get("wait_ms")
            .map_or(Some(0), serde_json::Value::as_u64);
        let (Some(cursor), Some(wait_ms)) = (cursor, wait_ms) else {
            return false;
        };
        if wait_ms > 30_000 {
            return false;
        }
        let Some(output) =
            output.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        else {
            return false;
        };
        if output["command_id"].as_str() != Some(id)
            || output["status"].as_str() != Some("running")
            || !output
                .get("exit_code")
                .is_some_and(serde_json::Value::is_null)
            || !output["output"].is_string()
        {
            return false;
        }
        let (Some(next), Some(truncated), Some(more)) = (
            output["next_cursor"].as_u64(),
            output["output_truncated"].as_bool(),
            output["has_more_output"].as_bool(),
        ) else {
            return false;
        };
        if next < cursor || (next == cursor && output["output"].as_str() != Some("")) {
            return false;
        }
        let previous = self
            .command_output
            .as_ref()
            .filter(|(command, _)| command == id)
            .map_or(cursor, |(_, next)| *next);
        self.command_output = Some((id.into(), previous.max(next)));
        let new_output = next > previous;
        // The local executor returns unchanged cursors only after waiting (or
        // at wait_ms=0). Require a useful wait so tight empty loops still warn.
        let waited = wait_ms >= 1000 && next == cursor && !truncated && !more;
        new_output || waited
    }
}

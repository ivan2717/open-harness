use agent_core::agent::{RunEvent, RunEventEnvelope};

/// Rebuild from durable execution history, not the model's compacted context.
/// Count call declarations in their stable order, independent of result timing.
#[derive(Default)]
pub(super) struct ToolRepetitionMonitor {
    observed: usize,
    previous: Option<(String, serde_json::Value)>,
    count: u32,
    pending: Vec<RunEvent>,
}

impl ToolRepetitionMonitor {
    pub(super) fn pending_warnings(&mut self, events: &[RunEventEnvelope]) -> Vec<RunEvent> {
        for entry in &events[self.observed..] {
            match &entry.event {
                RunEvent::RunStarted { .. }
                | RunEvent::InputReceived { .. }
                | RunEvent::RunResumed { .. } => {
                    self.previous = None;
                    self.count = 0;
                    self.pending.clear();
                }
                RunEvent::ToolCallRequested {
                    call_id,
                    tool_name,
                    arguments,
                } => {
                    let arguments = serde_json::from_str(arguments)
                        .unwrap_or_else(|_| serde_json::Value::String(arguments.clone()));
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
}

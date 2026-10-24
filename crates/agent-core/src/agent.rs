use std::{collections::HashSet, error::Error, fmt, sync::Arc, time::SystemTime};

use crate::TextFileReference;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub description: String,
    pub instructions: String,
}

impl Agent {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            instructions: instructions.into(),
        }
    }

    pub fn start_run(&self, run_id: impl Into<String>, instruction: impl Into<String>) -> AgentRun {
        AgentRun::new(run_id, self.id.clone(), instruction)
    }

    pub fn start_run_with_files(
        &self,
        run_id: impl Into<String>,
        instruction: impl Into<String>,
        files: Vec<TextFileReference>,
    ) -> AgentRun {
        AgentRun::start_with_files(run_id, self.id.clone(), instruction, files)
    }
}

#[derive(Debug, Clone)]
pub struct AgentRun {
    id: String,
    agent_id: String,
    status: RunStatus,
    current_step: u32,
    phase: StepPhase,
    tool_call_ids: HashSet<String>,
    pending_tool_calls: HashSet<String>,
    agent_message_ids: HashSet<String>,
    revision: Arc<()>,
    events: Vec<RunEventEnvelope>,
}

/// A validated, uncommitted event. Persist `envelope()` before committing it.
///
/// Preparing an event does not clone the run's history or mutate its projection.
/// A prepared event can only be committed to the exact revision it was prepared
/// from. Advancing the destination run invalidates older preparations, including
/// preparations created from a clone before the two histories diverged.
#[derive(Debug)]
pub struct PreparedRunEvent {
    envelope: RunEventEnvelope,
    status: RunStatus,
    phase: StepPhase,
    revision: Arc<()>,
}

impl PreparedRunEvent {
    pub fn envelope(&self) -> &RunEventEnvelope {
        &self.envelope
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepPhase {
    Ready,
    AwaitingModelResponse,
    ModelResponded,
    DeclaringToolCalls,
    AwaitingToolResults,
    ToolsCompleted,
    WaitingForInput,
}

impl AgentRun {
    pub fn new(
        id: impl Into<String>,
        agent_id: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self::start_with_files(id, agent_id, instruction, Vec::new())
    }

    pub fn start_with_files(
        id: impl Into<String>,
        agent_id: impl Into<String>,
        instruction: impl Into<String>,
        files: Vec<TextFileReference>,
    ) -> Self {
        Self {
            id: id.into(),
            agent_id: agent_id.into(),
            status: RunStatus::Running,
            current_step: 0,
            phase: StepPhase::Ready,
            tool_call_ids: HashSet::new(),
            pending_tool_calls: HashSet::new(),
            agent_message_ids: HashSet::new(),
            revision: Arc::new(()),
            events: vec![RunEventEnvelope {
                sequence: 0,
                step: 0,
                occurred_at: SystemTime::now(),
                event: RunEvent::RunStarted {
                    instruction: instruction.into(),
                    files,
                },
            }],
        }
    }

    pub fn restore(
        id: impl Into<String>,
        agent_id: impl Into<String>,
        events: Vec<RunEventEnvelope>,
    ) -> Result<Self, AgentRunHistoryError> {
        let mut events = events.into_iter();
        let Some(first) = events.next() else {
            return Err(AgentRunHistoryError::EmptyHistory);
        };

        if first.sequence != 0
            || first.step != 0
            || !matches!(first.event, RunEvent::RunStarted { .. })
        {
            return Err(AgentRunHistoryError::InvalidFirstEvent {
                sequence: first.sequence,
                step: first.step,
                event: first.event.name(),
            });
        }

        let mut run = Self {
            id: id.into(),
            agent_id: agent_id.into(),
            status: RunStatus::Running,
            current_step: 0,
            phase: StepPhase::Ready,
            tool_call_ids: HashSet::new(),
            pending_tool_calls: HashSet::new(),
            agent_message_ids: HashSet::new(),
            revision: Arc::new(()),
            events: vec![first],
        };

        for (index, envelope) in events.enumerate() {
            let index = index + 1;
            let expected_sequence = index as u64;
            if envelope.sequence != expected_sequence {
                return Err(AgentRunHistoryError::UnexpectedSequence {
                    index,
                    expected: expected_sequence,
                    actual: envelope.sequence,
                });
            }

            if matches!(envelope.event, RunEvent::RunStarted { .. }) {
                return Err(AgentRunHistoryError::DuplicateRunStarted {
                    sequence: envelope.sequence,
                });
            }

            let sequence = envelope.sequence;
            let (status, phase, expected_step) =
                if matches!(envelope.event, RunEvent::ModelRequestStarted) {
                    let step = run.validate_begin_step().map_err(|source| {
                        AgentRunHistoryError::InvalidEvent { sequence, source }
                    })?;
                    (RunStatus::Running, StepPhase::AwaitingModelResponse, step)
                } else {
                    let (status, phase) =
                        run.validate_event(&envelope.event).map_err(|source| {
                            AgentRunHistoryError::InvalidEvent { sequence, source }
                        })?;
                    (status, phase, run.current_step)
                };
            run.validate_restored_step(&envelope, expected_step)?;
            run.apply_event(envelope, status, phase);
        }

        Ok(run)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    /// Release archived image bytes only after the event store has committed
    /// them. Event identity and revisions are unchanged: references identify
    /// the same image content and are resolved by the owning event store.
    pub fn release_historical_image_data(&mut self) -> usize {
        release_historical_image_data(&mut self.events)
    }

    pub fn status(&self) -> RunStatus {
        self.status
    }

    /// Whether the durable lifecycle is `Running`, including a newly created
    /// or restored run that has no active executor. This does not report
    /// whether a runtime is currently polling model or tool futures.
    pub fn is_running(&self) -> bool {
        self.status == RunStatus::Running
    }

    pub fn current_step(&self) -> u32 {
        self.current_step
    }

    pub fn events(&self) -> &[RunEventEnvelope] {
        &self.events
    }

    pub fn instruction(&self) -> &str {
        match &self.events[0].event {
            RunEvent::RunStarted { instruction, .. } => instruction,
            _ => unreachable!("the first run event is always RunStarted"),
        }
    }

    pub fn has_tool_call(&self, call_id: &str) -> bool {
        self.tool_call_ids.contains(call_id)
    }

    pub fn has_agent_message(&self, message_id: &str) -> bool {
        self.agent_message_ids.contains(message_id)
    }

    /// Calls without a durable result at the interrupted boundary. Canonical
    /// responses may contain calls whose declaration was never committed, so
    /// the pending-call set alone is insufficient for protocol-safe recovery.
    pub fn interrupted_tool_calls(&self) -> Vec<InterruptedToolCall> {
        let events: Vec<_> = self
            .events
            .iter()
            .rev()
            .take_while(|entry| {
                entry.step == self.current_step
                    && !matches!(entry.event, RunEvent::RunResumed { .. })
            })
            .collect();
        let mut calls = Vec::new();
        let mut declared = HashSet::new();
        let mut resolved = HashSet::new();
        for entry in events.into_iter().rev() {
            match &entry.event {
                RunEvent::ModelResponseReceived {
                    response: Some(response),
                    ..
                } => {
                    calls.extend(response.calls().into_iter().map(|call| call.id));
                }
                RunEvent::ToolCallRequested { call_id, .. } => {
                    declared.insert(call_id.as_str());
                    if !calls.contains(call_id) {
                        calls.push(call_id.clone());
                    }
                }
                RunEvent::ToolCallSucceeded { call_id, .. }
                | RunEvent::ToolCallFailed { call_id, .. } => {
                    resolved.insert(call_id.as_str());
                }
                _ => {}
            }
        }
        calls
            .into_iter()
            .filter(|id| !resolved.contains(id.as_str()))
            .map(|call_id| InterruptedToolCall {
                outcome: if declared.contains(call_id.as_str()) {
                    InterruptedToolOutcome::Unknown
                } else {
                    InterruptedToolOutcome::NotStarted
                },
                call_id,
            })
            .collect()
    }

    /// Whether execution is at a boundary that can safely start a new model step.
    pub fn can_begin_step(&self) -> bool {
        self.validate_begin_step().is_ok()
    }

    pub fn begin_step(&mut self) -> Result<u32, AgentRunError> {
        let prepared = self.prepare_begin_step()?;
        self.commit_prepared(prepared)?;
        Ok(self.current_step)
    }

    pub fn record(&mut self, event: RunEvent) -> Result<&RunEventEnvelope, AgentRunError> {
        let prepared = self.prepare_event(event)?;
        self.commit_prepared(prepared)
    }

    pub fn prepare_begin_step(&self) -> Result<PreparedRunEvent, AgentRunError> {
        let step = self.validate_begin_step()?;
        Ok(self.prepare(
            RunEvent::ModelRequestStarted,
            step,
            RunStatus::Running,
            StepPhase::AwaitingModelResponse,
        ))
    }

    pub fn prepare_event(&self, event: RunEvent) -> Result<PreparedRunEvent, AgentRunError> {
        let (status, phase) = self.validate_event(&event)?;
        Ok(self.prepare(event, self.current_step, status, phase))
    }

    pub fn commit_prepared(
        &mut self,
        prepared: PreparedRunEvent,
    ) -> Result<&RunEventEnvelope, AgentRunError> {
        if !Arc::ptr_eq(&self.revision, &prepared.revision) {
            return Err(AgentRunError::StalePreparedEvent);
        }

        self.apply_event(prepared.envelope, prepared.status, prepared.phase);
        Ok(self
            .events
            .last()
            .expect("the event was appended immediately before this access"))
    }

    fn prepare(
        &self,
        event: RunEvent,
        step: u32,
        status: RunStatus,
        phase: StepPhase,
    ) -> PreparedRunEvent {
        PreparedRunEvent {
            envelope: RunEventEnvelope {
                sequence: self.events.len() as u64,
                step,
                occurred_at: SystemTime::now(),
                event,
            },
            status,
            phase,
            revision: Arc::clone(&self.revision),
        }
    }

    fn validate_begin_step(&self) -> Result<u32, AgentRunError> {
        if !self.is_running() {
            return Err(AgentRunError::RunNotRunning {
                status: self.status,
            });
        }
        if !matches!(self.phase, StepPhase::Ready | StepPhase::ToolsCompleted) {
            return Err(self.invalid_phase("ModelRequestStarted"));
        }
        self.current_step
            .checked_add(1)
            .ok_or(AgentRunError::StepNumberOverflow)
    }

    fn validate_event(&self, event: &RunEvent) -> Result<(RunStatus, StepPhase), AgentRunError> {
        if let RunEvent::RunResumed {
            content,
            files,
            interrupted_calls,
        } = event
        {
            if (content.trim().is_empty() && files.is_empty())
                || *interrupted_calls != self.interrupted_tool_calls()
            {
                return Err(AgentRunError::InvalidResume);
            }
            return Ok((RunStatus::Running, StepPhase::Ready));
        }
        if matches!(
            event,
            RunEvent::RunStarted { .. } | RunEvent::ModelRequestStarted
        ) {
            return Err(AgentRunError::ReservedEvent {
                event: event.name(),
            });
        }
        if event.requires_active_step() && self.current_step == 0 {
            return Err(AgentRunError::NoActiveStep {
                event: event.name(),
            });
        }
        if self.status.is_terminal() {
            return Err(AgentRunError::RunAlreadyFinished {
                status: self.status,
            });
        }

        match event {
            RunEvent::RunFailed { .. } => return Ok((RunStatus::Failed, self.phase)),
            RunEvent::RunCancelled => return Ok((RunStatus::Cancelled, self.phase)),
            RunEvent::InputReceived { .. } if self.status == RunStatus::WaitingForInput => {
                return Ok((RunStatus::Running, StepPhase::Ready));
            }
            _ => {}
        }
        if self.status != RunStatus::Running {
            return Err(AgentRunError::InvalidTransition {
                status: self.status,
                event: event.name(),
            });
        }

        self.validate_canonical_event(event)?;

        let phase = match event {
            RunEvent::RunPaused { .. }
                if matches!(self.phase, StepPhase::Ready | StepPhase::ToolsCompleted) =>
            {
                return Ok((RunStatus::Paused, self.phase));
            }
            RunEvent::ToolLoopWarning { .. } if self.phase == StepPhase::ToolsCompleted => {
                self.phase
            }
            RunEvent::AgentMessageReceived { message }
                if matches!(
                    self.phase,
                    StepPhase::Ready | StepPhase::ToolsCompleted | StepPhase::ModelResponded
                ) && !message.id.trim().is_empty()
                    && !message.sender.trim().is_empty()
                    && !message.text.trim().is_empty()
                    && !self.has_agent_message(&message.id) =>
            {
                StepPhase::Ready
            }
            RunEvent::ModelResponseReceived { .. }
                if self.phase == StepPhase::AwaitingModelResponse =>
            {
                StepPhase::ModelResponded
            }
            RunEvent::ToolCallRequested { call_id, .. }
                if matches!(
                    self.phase,
                    StepPhase::ModelResponded | StepPhase::DeclaringToolCalls
                ) =>
            {
                if self.has_tool_call(call_id) {
                    return Err(AgentRunError::DuplicateToolCallId {
                        call_id: call_id.clone(),
                    });
                }
                StepPhase::DeclaringToolCalls
            }
            RunEvent::ToolCallSucceeded { call_id, .. }
            | RunEvent::ToolCallFailed { call_id, .. } => {
                if !self.pending_tool_calls.contains(call_id) {
                    return Err(AgentRunError::ToolCallNotPending {
                        call_id: call_id.clone(),
                    });
                }
                if !matches!(
                    self.phase,
                    StepPhase::DeclaringToolCalls | StepPhase::AwaitingToolResults
                ) {
                    return Err(self.invalid_phase(event.name()));
                }
                if self.pending_tool_calls.len() == 1 {
                    StepPhase::ToolsCompleted
                } else {
                    StepPhase::AwaitingToolResults
                }
            }
            RunEvent::InputRequested { .. } if self.phase == StepPhase::ModelResponded => {
                return Ok((RunStatus::WaitingForInput, StepPhase::WaitingForInput));
            }
            RunEvent::RunCompleted { .. }
                if matches!(
                    self.phase,
                    StepPhase::ModelResponded | StepPhase::ToolsCompleted
                ) =>
            {
                return Ok((RunStatus::Completed, self.phase));
            }
            _ => return Err(self.invalid_phase(event.name())),
        };
        Ok((RunStatus::Running, phase))
    }

    /// Canonical response blocks and execution events describe the same calls.
    /// Validate this at the durable boundary as well as in the runtime so a
    /// restored log cannot substitute a different operation or visible answer.
    fn validate_canonical_event(&self, event: &RunEvent) -> Result<(), AgentRunError> {
        use crate::model_protocol::ModelFinishReason;
        if let RunEvent::ModelResponseReceived {
            content,
            response: Some(response),
            ..
        } = event
        {
            if response.validate().is_err() || response.text() != *content {
                return Err(AgentRunError::InvalidCanonicalResponse);
            }
            return Ok(());
        }
        let Some(response) = self
            .events
            .iter()
            .rev()
            .take_while(|entry| entry.step == self.current_step)
            .find_map(|entry| match &entry.event {
                RunEvent::ModelResponseReceived { response, .. } => response.as_ref(),
                _ => None,
            })
        else {
            return Ok(());
        };
        let declared = self
            .events
            .iter()
            .rev()
            .take_while(|entry| entry.step == self.current_step)
            .filter(|entry| matches!(entry.event, RunEvent::ToolCallRequested { .. }))
            .count();
        match event {
            RunEvent::AgentMessageReceived { .. }
                if self.phase == StepPhase::ModelResponded
                    && !matches!(
                        response.finish_reason,
                        ModelFinishReason::Stop | ModelFinishReason::InputRequired
                    ) =>
            {
                return Err(AgentRunError::InvalidCanonicalResponse);
            }
            RunEvent::ToolCallRequested {
                call_id,
                tool_name,
                arguments,
            } => {
                let calls = response.calls();
                if response.finish_reason != ModelFinishReason::ToolCalls
                    || !calls.get(declared).is_some_and(|call| {
                        call.id == *call_id
                            && call.name == *tool_name
                            && call.arguments == *arguments
                    })
                {
                    return Err(AgentRunError::CanonicalToolCallMismatch);
                }
            }
            RunEvent::ToolCallSucceeded { .. } | RunEvent::ToolCallFailed { .. }
                if declared != response.calls().len() =>
            {
                return Err(AgentRunError::CanonicalToolCallMismatch);
            }
            RunEvent::InputRequested { prompt }
                if !matches!(
                    response.finish_reason,
                    ModelFinishReason::InputRequired | ModelFinishReason::Stop
                ) || response.text() != *prompt =>
            {
                return Err(AgentRunError::InvalidCanonicalResponse);
            }
            RunEvent::RunCompleted { answer }
                if response.finish_reason != ModelFinishReason::Stop
                    || response.text() != *answer =>
            {
                return Err(AgentRunError::InvalidCanonicalResponse);
            }
            _ => {}
        }
        Ok(())
    }

    fn invalid_phase(&self, event: &'static str) -> AgentRunError {
        AgentRunError::InvalidStepPhase {
            phase: self.phase,
            event,
        }
    }

    fn validate_restored_step(
        &self,
        envelope: &RunEventEnvelope,
        expected: u32,
    ) -> Result<(), AgentRunHistoryError> {
        if envelope.step != expected {
            return Err(AgentRunHistoryError::UnexpectedStep {
                sequence: envelope.sequence,
                event: envelope.event.name(),
                expected,
                actual: envelope.step,
            });
        }
        Ok(())
    }

    fn apply_event(&mut self, envelope: RunEventEnvelope, status: RunStatus, phase: StepPhase) {
        match &envelope.event {
            RunEvent::AgentMessageReceived { message } => {
                self.agent_message_ids.insert(message.id.clone());
            }
            RunEvent::ToolCallRequested { call_id, .. } => {
                self.tool_call_ids.insert(call_id.clone());
                self.pending_tool_calls.insert(call_id.clone());
            }
            RunEvent::ToolCallSucceeded { call_id, .. }
            | RunEvent::ToolCallFailed { call_id, .. } => {
                self.pending_tool_calls.remove(call_id);
            }
            RunEvent::RunResumed {
                interrupted_calls, ..
            } => {
                self.pending_tool_calls.clear();
                for call in interrupted_calls {
                    self.tool_call_ids.insert(call.call_id.clone());
                }
            }
            _ => {}
        }
        self.current_step = envelope.step;
        self.status = status;
        self.phase = phase;
        self.revision = Arc::new(());
        self.events.push(envelope);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunEventEnvelope {
    pub sequence: u64,
    pub step: u32,
    pub occurred_at: SystemTime,
    pub event: RunEvent,
}

impl RunEventEnvelope {
    /// Stable integrity representation independent of whether image bytes are
    /// resident in memory. Text-only event serialization remains unchanged.
    pub fn with_image_references(&self) -> Result<Self, crate::ImageError> {
        let event = match &self.event {
            RunEvent::ToolCallSucceeded {
                call_id,
                output,
                images,
            } => RunEvent::ToolCallSucceeded {
                call_id: call_id.clone(),
                output: output.clone(),
                images: images
                    .iter()
                    .map(crate::ModelImage::as_reference)
                    .collect::<Result<_, _>>()?,
            },
            other => other.clone(),
        };
        Ok(Self {
            sequence: self.sequence,
            step: self.step,
            occurred_at: self.occurred_at,
            event,
        })
    }
}

/// Keep the newest image-bearing result available for the next model request.
/// Archive readers can still retrieve older raster bytes from the event store.
pub fn release_historical_image_data(events: &mut [RunEventEnvelope]) -> usize {
    let Some(latest) = events.iter().rposition(|event| {
        matches!(&event.event, RunEvent::ToolCallSucceeded { images, .. } if !images.is_empty())
    }) else {
        return 0;
    };
    let mut released = 0;
    for event in &mut events[..latest] {
        if let RunEvent::ToolCallSucceeded { images, .. } = &mut event.event {
            for image in images {
                if image.reference_id().is_none()
                    && let Ok(reference) = image.as_reference()
                {
                    released += image.data_base64.len();
                    *image = reference;
                }
            }
        }
    }
    released
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunEvent {
    RunStarted {
        instruction: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<TextFileReference>,
    },
    ModelRequestStarted,
    ModelResponseReceived {
        content: String,
        /// Ordered canonical response. Absent in legacy logs, whose text and
        /// following ToolCallRequested events are projected on recovery.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response: Option<crate::model_protocol::ModelResponse>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_state: Option<crate::provider_state::ProviderResponseState>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_continuation: Option<Box<crate::provider_state::ProviderContinuation>>,
        /// Successful request usage and its verifiable calibration identity.
        /// Legacy logs omit this field and remain readable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token_usage: Option<Box<crate::token_usage::ModelUsageRecord>>,
    },
    ToolCallRequested {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolCallSucceeded {
        call_id: String,
        output: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<crate::ModelImage>,
    },
    ToolCallFailed {
        call_id: String,
        message: String,
    },
    InputRequested {
        prompt: String,
    },
    InputReceived {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<TextFileReference>,
    },
    /// Explicit user continuation. Unresolved calls are settled atomically as
    /// interrupted, never replayed. This also abandons an unfinished model
    /// request and permits a fresh step after a terminal or interrupted run.
    RunResumed {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<TextFileReference>,
        interrupted_calls: Vec<InterruptedToolCall>,
    },
    RunCompleted {
        answer: String,
    },
    /// A policy boundary with all tool effects settled; explicit continuation
    /// can resume it without treating the task as an execution failure.
    RunPaused {
        reason: RunPauseReason,
    },
    /// Advisory only. The corresponding tool calls have already completed.
    ToolLoopWarning {
        call_id: String,
        tool_name: String,
        consecutive_calls: u32,
    },
    AgentMessageReceived {
        message: crate::AgentMessage,
    },
    RunFailed {
        message: String,
    },
    RunCancelled,
}

impl RunEvent {
    /// The same model-visible representation is used for normal replay,
    /// protected inputs, and native-compaction context.
    pub fn user_input(&self) -> Option<String> {
        match self {
            Self::RunStarted { instruction, files } => {
                Some(crate::format_user_input(instruction, files))
            }
            Self::InputReceived { content, files } | Self::RunResumed { content, files, .. } => {
                Some(crate::format_user_input(content, files))
            }
            _ => None,
        }
    }

    /// Display/history projection. Canonical storage retains the opaque state.
    pub fn without_provider_state(&self) -> Self {
        let mut event = self.clone();
        // Historical text projections must not serialize image bytes into a
        // summary, search response or another model's text input.
        if let Self::ToolCallSucceeded { images, output, .. } = &mut event
            && !images.is_empty()
        {
            output.push_str("\n[Historical image observation omitted; observe the current interface again when needed.]");
            images.clear();
        }
        if let Self::ModelResponseReceived {
            response,
            provider_state,
            provider_continuation,
            token_usage,
            ..
        } = &mut event
        {
            // Summary/history projections use the visible text and separate
            // execution events. Including canonical blocks here would duplicate
            // the answer/calls and expose reasoning to a second model.
            *response = None;
            *provider_state = None;
            *provider_continuation = None;
            *token_usage = None;
        }
        event
    }

    fn name(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "RunStarted",
            Self::ModelRequestStarted => "ModelRequestStarted",
            Self::ModelResponseReceived { .. } => "ModelResponseReceived",
            Self::ToolCallRequested { .. } => "ToolCallRequested",
            Self::ToolCallSucceeded { .. } => "ToolCallSucceeded",
            Self::ToolCallFailed { .. } => "ToolCallFailed",
            Self::InputRequested { .. } => "InputRequested",
            Self::InputReceived { .. } => "InputReceived",
            Self::RunResumed { .. } => "RunResumed",
            Self::RunCompleted { .. } => "RunCompleted",
            Self::RunPaused { .. } => "RunPaused",
            Self::ToolLoopWarning { .. } => "ToolLoopWarning",
            Self::AgentMessageReceived { .. } => "AgentMessageReceived",
            Self::RunFailed { .. } => "RunFailed",
            Self::RunCancelled => "RunCancelled",
        }
    }

    fn requires_active_step(&self) -> bool {
        match self {
            Self::RunStarted { .. }
            | Self::ModelRequestStarted
            | Self::RunResumed { .. }
            | Self::RunPaused { .. }
            | Self::AgentMessageReceived { .. }
            | Self::RunFailed { .. }
            | Self::RunCancelled => false,

            Self::ModelResponseReceived { .. }
            | Self::ToolCallRequested { .. }
            | Self::ToolCallSucceeded { .. }
            | Self::ToolCallFailed { .. }
            | Self::ToolLoopWarning { .. }
            | Self::InputRequested { .. }
            | Self::InputReceived { .. }
            | Self::RunCompleted { .. } => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterruptedToolCall {
    pub call_id: String,
    pub outcome: InterruptedToolOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptedToolOutcome {
    /// No execution declaration was committed, so the runtime never invoked it.
    NotStarted,
    /// Effects may exist despite the absence of a durable tool result.
    Unknown,
}

impl InterruptedToolOutcome {
    pub fn explanation(self) -> &'static str {
        match self {
            Self::NotStarted => {
                "This tool call was not started because execution was interrupted. It was not retried during recovery."
            }
            Self::Unknown => {
                "Execution was interrupted before a durable result was recorded. This operation may have partially or fully taken effect. It was not retried. Inspect the current files or process state before deciding whether another operation is necessary."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    WaitingForInput,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunPauseReason {
    StepLimit { max_steps: u32 },
    SharedTokenBudget { max_tokens: u64 },
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunHistoryError {
    EmptyHistory,
    InvalidFirstEvent {
        sequence: u64,
        step: u32,
        event: &'static str,
    },
    UnexpectedSequence {
        index: usize,
        expected: u64,
        actual: u64,
    },
    UnexpectedStep {
        sequence: u64,
        event: &'static str,
        expected: u32,
        actual: u32,
    },
    DuplicateRunStarted {
        sequence: u64,
    },
    InvalidEvent {
        sequence: u64,
        source: AgentRunError,
    },
}

impl fmt::Display for AgentRunHistoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyHistory => formatter.write_str("cannot restore a run from empty history"),
            Self::InvalidFirstEvent {
                sequence,
                step,
                event,
            } => write!(
                formatter,
                "the first event must be RunStarted at sequence 0 and step 0, got {event} at sequence {sequence} and step {step}"
            ),
            Self::UnexpectedSequence {
                index,
                expected,
                actual,
            } => write!(
                formatter,
                "event at index {index} has sequence {actual}, expected {expected}"
            ),
            Self::UnexpectedStep {
                sequence,
                event,
                expected,
                actual,
            } => write!(
                formatter,
                "event {event} at sequence {sequence} belongs to step {actual}, expected {expected}"
            ),
            Self::DuplicateRunStarted { sequence } => write!(
                formatter,
                "RunStarted may only be the first event, but another occurred at sequence {sequence}"
            ),
            Self::InvalidEvent { sequence, source } => {
                write!(
                    formatter,
                    "event at sequence {sequence} is invalid: {source}"
                )
            }
        }
    }
}

impl Error for AgentRunHistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidEvent { source, .. } => Some(source),
            Self::EmptyHistory
            | Self::InvalidFirstEvent { .. }
            | Self::UnexpectedSequence { .. }
            | Self::UnexpectedStep { .. }
            | Self::DuplicateRunStarted { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRunError {
    InvalidResume,
    InvalidCanonicalResponse,
    CanonicalToolCallMismatch,
    RunNotRunning {
        status: RunStatus,
    },
    RunAlreadyFinished {
        status: RunStatus,
    },
    NoActiveStep {
        event: &'static str,
    },
    InvalidTransition {
        status: RunStatus,
        event: &'static str,
    },
    ReservedEvent {
        event: &'static str,
    },
    InvalidStepPhase {
        phase: StepPhase,
        event: &'static str,
    },
    DuplicateToolCallId {
        call_id: String,
    },
    ToolCallNotPending {
        call_id: String,
    },
    StalePreparedEvent,
    StepNumberOverflow,
}

impl fmt::Display for AgentRunError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResume => formatter.write_str("resume requires nonempty input and the exact unresolved tool outcomes from the current run"),
            Self::InvalidCanonicalResponse => formatter.write_str(
                "canonical response disagrees with its visible text or completion event",
            ),
            Self::CanonicalToolCallMismatch => {
                formatter.write_str("tool execution events disagree with the canonical response")
            }
            Self::RunNotRunning { status } => {
                write!(formatter, "cannot begin a step while the run is {status:?}")
            }
            Self::RunAlreadyFinished { status } => {
                write!(
                    formatter,
                    "cannot record an event after the run became {status:?}"
                )
            }
            Self::NoActiveStep { event } => {
                write!(formatter, "event {event} requires an active step")
            }
            Self::InvalidTransition { status, event } => {
                write!(
                    formatter,
                    "event {event} is invalid while the run is {status:?}"
                )
            }
            Self::ReservedEvent { event } => {
                write!(formatter, "event {event} must be created by AgentRun")
            }
            Self::InvalidStepPhase { phase, event } => {
                write!(
                    formatter,
                    "event {event} is invalid while the step is {phase:?}"
                )
            }
            Self::DuplicateToolCallId { call_id } => {
                write!(
                    formatter,
                    "tool call id {call_id:?} has already been requested"
                )
            }
            Self::ToolCallNotPending { call_id } => {
                write!(
                    formatter,
                    "tool call {call_id:?} has no pending request in this step"
                )
            }
            Self::StalePreparedEvent => {
                formatter.write_str("the prepared event belongs to a different run revision")
            }
            Self::StepNumberOverflow => formatter.write_str("the step number overflowed"),
        }
    }
}

impl Error for AgentRunError {}

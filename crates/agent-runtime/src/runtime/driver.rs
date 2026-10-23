use std::collections::HashSet;

use agent_core::agent::{Agent, AgentRun, RunEvent, RunStatus};

use crate::{
    ContextInputRef, ContextManager, EventStore, HistoryAccess, HistoryContext, Model,
    ModelCapabilities, ModelContextMetadata, ModelFinishReason, ModelOutput, ModelRequest,
    ModelStreamAssembler, ModelStreamEvent, ModelStreamSink, ReplayRequirement, ToolCall,
    ToolDefinition, ToolExecutor,
};

use super::{AgentRuntime, AgentRuntimeError, execution::RunControl};

impl<M, T, C, S, H> AgentRuntime<M, T, C, S, H>
where
    M: Model,
    T: ToolExecutor,
    S: EventStore,
    C: ContextManager,
    H: HistoryAccess<S>,
{
    pub async fn run_until_blocked(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
    ) -> Result<RunStatus, AgentRuntimeError<M::Error>> {
        self.run_until_blocked_with_stream(agent, run, &mut DiscardStream)
            .await
    }

    /// Live events are provisional until ModelResponseReceived is durably
    /// committed. A dropped/failed model stream never authorizes tool effects.
    pub async fn run_until_blocked_with_stream(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<RunStatus, AgentRuntimeError<M::Error>> {
        if run.agent_id() != agent.id {
            return Err(AgentRuntimeError::AgentMismatch {
                expected_agent_id: agent.id.clone(),
                actual_agent_id: run.agent_id().to_owned(),
            });
        }

        let execution = self
            .executions
            .claim(run.id(), run.agent_id())
            .map_err(|_| AgentRuntimeError::RunAlreadyExecuting {
                run_id: run.id().to_owned(),
            })?;
        let control = &execution.control;
        self.synchronize_run(run).await?;
        self.ensure_safe_execution_entry(run)?;
        let tools = self.tools.snapshot_definitions();
        self.validate_tool_definitions(&tools)?;

        let capabilities = self.model.capabilities();
        capabilities
            .validate()
            .map_err(AgentRuntimeError::ModelCapabilities)?;

        let accounting_metadata = self.accounting_metadata();
        let turn_start_step = run
            .events()
            .iter()
            .rev()
            .find_map(|event| {
                (matches!(event.event, RunEvent::RunResumed { .. })
                    || (self.continue_after_response
                        && matches!(event.event, RunEvent::InputReceived { .. })))
                .then_some(event.step)
            })
            .unwrap_or(0);

        while run.is_running() {
            if control.is_cancelled() {
                return self.finish_cancellation(run).await;
            }
            if let Some(max_steps) = self.max_steps
                && run.current_step().saturating_sub(turn_start_step) >= max_steps
            {
                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: format!("maximum number of steps ({max_steps}) reached"),
                    },
                )
                .await?;

                return Err(AgentRuntimeError::StepLimitExceeded { max_steps });
            }

            let preparation = control
                .until_cancelled(|| {
                    self.context_manager.prepare(ContextInputRef {
                        run_id: run.id(),
                        instructions: &agent.instructions,
                        events: run.events(),
                        derived_messages: &[],
                        tools: &tools,
                        deferred_tools: self.history_access.definitions(),
                        tool_output_projector: Some(&self.history_access),
                        model_metadata: accounting_metadata.as_ref(),
                    })
                })
                .await;
            let Some(preparation) = preparation else {
                return self.finish_cancellation(run).await;
            };
            let mut prepared = match preparation {
                Ok(prepared) => prepared,
                Err(source) => {
                    self.record_persisted(
                        run,
                        RunEvent::RunFailed {
                            message: source.to_string(),
                        },
                    )
                    .await?;

                    return Err(AgentRuntimeError::Context(Box::new(source)));
                }
            };
            let prepared_event_count = run.events().len();
            let mut recovered_overflow = false;
            let (assembler, measurement, request_fixed, request_prefix, usage_baseline) = loop {
                if let Err(source) = self.validate_model_request(&prepared.request, &capabilities) {
                    self.record_persisted(
                        run,
                        RunEvent::RunFailed {
                            message: source.to_string(),
                        },
                    )
                    .await?;
                    return Err(source);
                }
                let request_fixed = prepared
                    .request
                    .replay_fixed_fingerprint()
                    .map_err(AgentRuntimeError::ProviderReplay)?;
                let request_prefix = prepared
                    .request
                    .replay_prefix_fingerprint(prepared.request.messages.len())
                    .map_err(AgentRuntimeError::ProviderReplay)?;
                let usage_baseline = self
                    .context_manager
                    .request_usage_baseline(accounting_metadata.as_ref(), &prepared.request);
                let measurement = self.context_manager.measure_request(
                    self.context_input(agent, run, &tools, accounting_metadata.as_ref()),
                    &prepared.request,
                );
                if !recovered_overflow {
                    self.begin_step_persisted(run).await?;
                }

                let mut assembler = ModelStreamAssembler::new();
                let (response, emitted) = {
                    let mut forwarding = ForwardStream {
                        assembler: &mut assembler,
                        sink,
                        emitted: false,
                    };
                    let response = control
                        .until_cancelled(|| {
                            self.model.stream(prepared.request.clone(), &mut forwarding)
                        })
                        .await;
                    (response, forwarding.emitted)
                };
                let Some(response) = response else {
                    return self.finish_cancellation(run).await;
                };
                let Err(source) = response else {
                    break (
                        assembler,
                        measurement,
                        request_fixed,
                        request_prefix,
                        usage_baseline,
                    );
                };

                // Only an explicit provider overflow before any observable stream
                // event may retry. Recovery stays inside this model step, and no
                // tool effect is authorized until the final response is durable.
                if !recovered_overflow && !emitted && self.model.is_context_overflow(&source) {
                    let recovery = control
                        .until_cancelled(|| {
                            let mut input = self.context_input(
                                agent,
                                run,
                                &tools,
                                accounting_metadata.as_ref(),
                            );
                            // The in-flight request marker is not settled
                            // history and must never enter a checkpoint.
                            input.events = &input.events[..prepared_event_count];
                            self.context_manager
                                .recover_overflow(input, &prepared.request)
                        })
                        .await;
                    let Some(recovery) = recovery else {
                        return self.finish_cancellation(run).await;
                    };
                    let candidate = match recovery {
                        Ok(candidate) => candidate,
                        Err(error) => {
                            let error = AgentRuntimeError::ContextRecovery {
                                overflow: source,
                                recovery: Box::new(error),
                            };
                            self.record_persisted(
                                run,
                                RunEvent::RunFailed {
                                    message: error.to_string(),
                                },
                            )
                            .await?;
                            return Err(error);
                        }
                    };
                    if let Some(candidate) = candidate
                        && candidate.request != prepared.request
                        && self.request_is_smaller(&candidate.request, &prepared.request)
                    {
                        prepared = candidate;
                        recovered_overflow = true;
                        continue;
                    }
                }

                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: source.to_string(),
                    },
                )
                .await?;
                return Err(AgentRuntimeError::Model(source));
            };
            let history_context = prepared.omitted_history.map(|omitted| HistoryContext {
                run_id: run.id().to_owned(),
                through_sequence: omitted.readable_through_sequence,
                checkpoint_id: omitted.checkpoint_id,
            });
            let prefix_bound = prepared.request.has_prefix_bound_state();

            let mut response = match assembler.finish() {
                Ok(output) => output,
                Err(source) => {
                    self.record_persisted(
                        run,
                        RunEvent::RunFailed {
                            message: source.to_string(),
                        },
                    )
                    .await?;
                    return Err(AgentRuntimeError::ModelStream(source));
                }
            };
            if let Err(source) = response.response.validate() {
                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: source.to_string(),
                    },
                )
                .await?;
                return Err(AgentRuntimeError::ModelProtocol(source));
            }
            if (response.context_compacted && !capabilities.context.supports_native_compaction)
                || (response.provider_context.is_some()
                    && !capabilities.context.accepts_native_context)
            {
                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: "adapter returned undeclared native context or compaction".into(),
                    },
                )
                .await?;
                return Err(AgentRuntimeError::ProviderContextMismatch);
            }
            if let Some(state) = &response.provider_state
                && let Err(source) = self
                    .model
                    .validate_replay_state(state, &response.response.content)
            {
                let source = crate::ProviderReplayError::InvalidResponse(source);
                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: source.to_string(),
                    },
                )
                .await?;
                return Err(AgentRuntimeError::ProviderReplay(source));
            }
            if let Err(source) = response.bind_provider_state(
                self.model.provider_context_identity(),
                capabilities.context.replay_requirement == ReplayRequirement::Required,
                &request_prefix,
            ) {
                self.record_persisted(
                    run,
                    RunEvent::RunFailed {
                        message: source.to_string(),
                    },
                )
                .await?;
                return Err(AgentRuntimeError::ProviderReplay(source));
            }
            if let Some(window) = &mut response.provider_context
                && (prefix_bound
                    || response
                        .provider_state
                        .as_ref()
                        .is_some_and(|state| state.scope == crate::ProviderReplayScope::Prefix))
            {
                window.prefix_fingerprint = Some(request_fixed);
            }
            tracing::debug!(
                run_id = run.id(),
                estimated_input_tokens = measurement
                    .as_ref()
                    .map(|value| value.estimated_input_tokens),
                decision_input_tokens = measurement
                    .as_ref()
                    .map(|value| value.decision_input_tokens),
                estimation_margin_tokens = measurement
                    .as_ref()
                    .map(|value| value.estimation_margin_tokens),
                actual_input_tokens = response.usage.map(|usage| usage.input_tokens),
                output_tokens = response.usage.map(|usage| usage.output_tokens),
                cached_input_tokens = response.usage.and_then(|usage| usage.cached_input_tokens),
                "model context usage"
            );
            let text = response.response.text();
            self.record_persisted(
                run,
                RunEvent::ModelResponseReceived {
                    content: text.clone(),
                    token_usage: response.usage.map(|usage| {
                        Box::new(agent_core::token_usage::ModelUsageRecord {
                            usage,
                            baseline: if response.context_compacted
                                || response.provider_context.is_some()
                            {
                                None
                            } else {
                                usage_baseline
                            },
                        })
                    }),
                    response: Some(response.response.clone()),
                    provider_state: response.provider_state.clone(),
                    provider_continuation: response.provider_context.clone().map(|window| {
                        Box::new(crate::ProviderContinuation {
                            window,
                            compacted: response.context_compacted,
                        })
                    }),
                },
            )
            .await?;
            match response.response.finish_reason {
                ModelFinishReason::ToolCalls => {
                    self.execute_tool_calls(
                        agent,
                        run,
                        control,
                        &tools,
                        history_context,
                        &response,
                    )
                    .await?;
                }
                ModelFinishReason::Stop | ModelFinishReason::InputRequired => {
                    if !self
                        .observe_model_output(agent, run, control, &tools, &response)
                        .await?
                    {
                        return Ok(run.status());
                    }
                    let event = if response.response.finish_reason == ModelFinishReason::Stop
                        && !self.continue_after_response
                    {
                        RunEvent::RunCompleted { answer: text }
                    } else {
                        RunEvent::InputRequested { prompt: text }
                    };
                    self.record_persisted(run, event).await?;
                }
                ModelFinishReason::Length => {
                    self.record_persisted(
                        run,
                        RunEvent::RunFailed {
                            message: "model output was truncated before completion".into(),
                        },
                    )
                    .await?;
                    return Err(AgentRuntimeError::OutputTruncated);
                }
            }
        }

        Ok(run.status())
    }

    pub(super) fn accounting_metadata(&self) -> Option<ModelContextMetadata> {
        crate::model_context::model_accounting_metadata(&self.model)
    }

    pub(super) fn context_input<'a>(
        &'a self,
        agent: &'a Agent,
        run: &'a AgentRun,
        tools: &'a [ToolDefinition],
        metadata: Option<&'a ModelContextMetadata>,
    ) -> ContextInputRef<'a> {
        ContextInputRef {
            run_id: run.id(),
            instructions: &agent.instructions,
            events: run.events(),
            derived_messages: &[],
            tools,
            deferred_tools: self.history_access.definitions(),
            tool_output_projector: Some(&self.history_access),
            model_metadata: metadata,
        }
    }

    fn validate_model_request(
        &self,
        request: &ModelRequest,
        capabilities: &ModelCapabilities,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        if let Some(window) = &request.provider_context
            && (!capabilities.context.accepts_native_context
                || self.model.provider_context_identity() != Some(&window.identity))
        {
            return Err(AgentRuntimeError::ProviderContextMismatch);
        }
        for message in &request.messages {
            message
                .validate()
                .map_err(AgentRuntimeError::ModelProtocol)?;
        }
        request
            .validate_provider_state_with(
                self.model.provider_context_identity(),
                capabilities.context.replay_requirement == ReplayRequirement::Required,
                |state, content| self.model.validate_replay_state(state, content),
            )
            .map_err(AgentRuntimeError::ProviderReplay)
    }

    fn request_is_smaller(&self, candidate: &ModelRequest, rejected: &ModelRequest) -> bool {
        // Use the same uncalibrated counter on both requests. A rewritten prefix
        // invalidates the usage anchor, so comparing a calibrated before-value
        // against a raw after-value would not establish an actual reduction.
        match (
            self.context_manager.estimate_request_tokens(candidate),
            self.context_manager.estimate_request_tokens(rejected),
        ) {
            (Some(after), Some(before)) => after < before,
            _ => false,
        }
    }

    async fn observe_model_output(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
        control: &RunControl,
        tools: &[ToolDefinition],
        output: &ModelOutput,
    ) -> Result<bool, AgentRuntimeError<M::Error>> {
        let accounting_metadata = self.accounting_metadata();
        let result = control
            .until_cancelled(|| {
                self.context_manager.observe_response(
                    ContextInputRef {
                        run_id: run.id(),
                        instructions: &agent.instructions,
                        events: run.events(),
                        derived_messages: &[],
                        tools,
                        deferred_tools: self.history_access.definitions(),
                        tool_output_projector: Some(&self.history_access),
                        model_metadata: accounting_metadata.as_ref(),
                    },
                    output,
                )
            })
            .await;
        let Some(result) = result else {
            self.finish_cancellation(run).await?;
            return Ok(false);
        };
        if let Err(source) = result {
            self.record_persisted(
                run,
                RunEvent::RunFailed {
                    message: source.to_string(),
                },
            )
            .await?;
            return Err(AgentRuntimeError::Context(Box::new(source)));
        }
        Ok(true)
    }

    async fn execute_tool_calls(
        &self,
        agent: &Agent,
        run: &mut AgentRun,
        control: &RunControl,
        tools: &[ToolDefinition],
        history_context: Option<HistoryContext>,
        output: &ModelOutput,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let calls = output.response.calls();
        if calls.is_empty() {
            self.record_persisted(
                run,
                RunEvent::RunFailed {
                    message: "model returned an empty tool-call list".to_owned(),
                },
            )
            .await?;

            return Err(AgentRuntimeError::EmptyToolCalls);
        }

        let mut call_ids = HashSet::new();
        if let Some(duplicate) = calls
            .iter()
            .find(|call| run.has_tool_call(&call.id) || !call_ids.insert(call.id.as_str()))
        {
            let call_id = duplicate.id.clone();
            self.record_persisted(
                run,
                RunEvent::RunFailed {
                    message: format!("model reused tool call id {call_id:?}"),
                },
            )
            .await?;
            return Err(AgentRuntimeError::DuplicateToolCallId { call_id });
        }

        for call in &calls {
            self.record_persisted(
                run,
                RunEvent::ToolCallRequested {
                    call_id: call.id.clone(),
                    tool_name: call.name.clone(),
                    arguments: call.arguments.clone(),
                },
            )
            .await?;
        }
        if !self
            .observe_model_output(agent, run, control, tools, output)
            .await?
        {
            return Ok(());
        }

        for call in calls {
            let call_id = call.id.clone();
            let result = control
                .until_cancelled(|| self.execute_tool(call, tools, history_context.clone()))
                .await;
            let Some(result) = result else {
                self.finish_cancellation(run).await?;
                return Ok(());
            };

            match result {
                Ok(output) => {
                    self.record_persisted(run, RunEvent::ToolCallSucceeded { call_id, output })
                        .await?;
                }
                Err(message) => {
                    self.record_persisted(run, RunEvent::ToolCallFailed { call_id, message })
                        .await?;
                }
            }
            // Cached/in-memory providers may never yield. Give a
            // cancellation task a chance to run between effects.
            tokio::task::yield_now().await;
        }
        Ok(())
    }

    async fn execute_tool(
        &self,
        call: ToolCall,
        tools: &[ToolDefinition],
        history_context: Option<HistoryContext>,
    ) -> Result<String, String> {
        if self.history_access.owns_tool(&call.name) {
            let context = history_context.ok_or_else(|| {
                "history details are unavailable because this context has not omitted any events"
                    .to_owned()
            })?;
            let max_chars = self.context_manager.max_tool_output_chars();
            let output = self
                .history_access
                .execute_bounded(&self.event_store, context, call, max_chars)
                .await
                .map_err(|error| error.to_string())?;
            if max_chars.is_some_and(|limit| output.chars().count() > limit) {
                return Err("history provider exceeded the tool output character limit".to_owned());
            }
            Ok(output)
        } else if !tools.iter().any(|tool| tool.name == call.name) {
            Err(format!(
                "tool {:?} is not available in this turn",
                call.name
            ))
        } else {
            self.tools
                .execute(call)
                .await
                .map_err(|error| error.to_string())
        }
    }

    async fn finish_cancellation(
        &self,
        run: &mut AgentRun,
    ) -> Result<RunStatus, AgentRuntimeError<M::Error>> {
        self.record_persisted(run, RunEvent::RunCancelled).await?;
        Ok(RunStatus::Cancelled)
    }

    pub(super) fn validate_tool_definitions(
        &self,
        tools: &[ToolDefinition],
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        let mut names = HashSet::new();
        for definition in tools.iter().chain(self.history_access.definitions()) {
            if !names.insert(definition.name.as_str()) {
                return Err(AgentRuntimeError::DuplicateToolName {
                    name: definition.name.clone(),
                });
            }
        }

        Ok(())
    }

    fn ensure_safe_execution_entry(
        &self,
        run: &AgentRun,
    ) -> Result<(), AgentRuntimeError<M::Error>> {
        if !run.is_running() {
            return Ok(());
        }

        let last = run
            .events()
            .last()
            .expect("an AgentRun always contains RunStarted");
        if run.can_begin_step() {
            return Ok(());
        }

        Err(AgentRuntimeError::InterruptedRun {
            run_id: run.id().to_owned(),
            last_sequence: last.sequence,
        })
    }
}

struct DiscardStream;
impl ModelStreamSink for DiscardStream {
    fn emit(&mut self, _event: ModelStreamEvent) {}
}

struct ForwardStream<'a> {
    assembler: &'a mut ModelStreamAssembler,
    sink: &'a mut dyn ModelStreamSink,
    emitted: bool,
}
impl ModelStreamSink for ForwardStream<'_> {
    fn emit(&mut self, event: ModelStreamEvent) {
        self.emitted = true;
        self.assembler.emit(event.clone());
        self.sink.emit(event);
    }
}

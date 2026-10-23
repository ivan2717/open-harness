//! Bridge optional session memory to actual summary-compaction boundaries.
use super::*;
use crate::memory::{CatchUpBudget, WikiMemory};
use agent_runtime::{CompactionPreparation, CompactionPreparationHook, ModelMessage};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct MemoryCompactionHook<'a> {
    memory: Option<&'a Arc<WikiMemory>>,
    caller_messages: Vec<ModelMessage>,
    include_memory: AtomicBool,
    injected: AtomicBool,
    budget: tokio::sync::Mutex<CatchUpBudget>,
}

impl<'a> MemoryCompactionHook<'a> {
    pub fn new(
        memory: Option<&'a Arc<WikiMemory>>,
        input: ContextInputRef<'_>,
        initial_messages: &[ModelMessage],
    ) -> Self {
        Self {
            memory,
            caller_messages: input.derived_messages.to_vec(),
            include_memory: AtomicBool::new(true),
            injected: AtomicBool::new(initial_messages.len() > input.derived_messages.len()),
            budget: tokio::sync::Mutex::new(CatchUpBudget::default()),
        }
    }

    pub fn injected_memory(&self) -> bool {
        self.injected.load(Ordering::Relaxed)
    }

    pub fn omit_memory(&self) {
        self.include_memory.store(false, Ordering::Relaxed);
    }
}

impl CompactionPreparationHook for MemoryCompactionHook<'_> {
    async fn before_compaction(
        &self,
        input: ContextInputRef<'_>,
        through_sequence: u64,
    ) -> CompactionPreparation {
        let mut messages = self.caller_messages.clone();
        let mut ready = true;
        if let Some(memory) = self.memory {
            let mut budget = self.budget.lock().await;
            ready = memory
                .ensure_coverage(input, through_sequence, &mut budget)
                .await;
            if self.include_memory.load(Ordering::Relaxed) {
                let notes = memory.messages(input);
                if !notes.is_empty() {
                    self.injected.store(true, Ordering::Relaxed);
                    messages.extend(notes);
                }
            }
        }
        CompactionPreparation {
            derived_messages: messages,
            ready,
        }
    }
}

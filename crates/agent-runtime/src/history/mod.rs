//! History access contracts and the built-in event-store adapter.

mod config;
mod event_store;
mod protocol;
mod toggle;

use std::{convert::Infallible, error::Error, future::Future};

use crate::{
    storage::EventStore,
    tool::{ToolCall, ToolDefinition, ToolOutputProjector},
};

pub use config::{HistoryConfigurationError, HistoryLimits, HistoryLimitsBuilder};
pub use event_store::{EventStoreHistoryAccess, HistoryAccessError};
pub use protocol::{READ_RUN_HISTORY_TOOL, SEARCH_RUN_HISTORY_TOOL};
pub use toggle::{ToggleHistoryAccess, ToggleHistoryAccessError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryContext {
    pub run_id: String,
    pub through_sequence: u64,
    pub checkpoint_id: String,
}

/// A history adapter owns both its execution and saved-output protocol.
///
/// Implementations must apply output limits before encoding pagination cursors.
/// The paired `ToolOutputProjector` implementation preserves the same protocol
/// when an existing response is shown under a smaller context budget.
pub trait HistoryAccess<S: EventStore>: ToolOutputProjector {
    type Error: Error + Send + Sync + 'static;

    fn definitions(&self) -> &[ToolDefinition];

    fn owns_tool(&self, name: &str) -> bool;

    /// Execute using only the adapter's configured output limit.
    fn execute(
        &self,
        event_store: &S,
        context: HistoryContext,
        call: ToolCall,
    ) -> impl Future<Output = Result<String, Self::Error>> + Send {
        self.execute_bounded(event_store, context, call, None)
    }

    /// Encode a response that fits both the adapter's configured limit and the
    /// optional active context limit, measured in Unicode scalar values.
    ///
    /// Return an error when a valid response cannot fit. This method is required
    /// so an adapter cannot silently inherit an implementation that ignores the
    /// active context's budget.
    fn execute_bounded(
        &self,
        event_store: &S,
        context: HistoryContext,
        call: ToolCall,
        max_output_chars: Option<usize>,
    ) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoHistoryAccess;

impl ToolOutputProjector for NoHistoryAccess {
    fn project_output(
        &self,
        _tool_name: &str,
        _output: &str,
        _max_chars: usize,
    ) -> Option<Result<String, String>> {
        None
    }
}

impl<S: EventStore> HistoryAccess<S> for NoHistoryAccess {
    type Error = Infallible;

    fn definitions(&self) -> &[ToolDefinition] {
        &[]
    }

    fn owns_tool(&self, _name: &str) -> bool {
        false
    }

    async fn execute_bounded(
        &self,
        _event_store: &S,
        _context: HistoryContext,
        _call: ToolCall,
        _max_output_chars: Option<usize>,
    ) -> Result<String, Self::Error> {
        unreachable!("NoHistoryAccess does not own any tools")
    }
}

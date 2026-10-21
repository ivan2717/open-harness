use std::{error::Error, fmt};

use crate::{
    storage::EventStore,
    tool::{ToolCall, ToolDefinition, ToolOutputProjector},
};

use super::{HistoryAccess, HistoryContext};

#[derive(Debug, Clone)]
pub struct ToggleHistoryAccess<H> {
    inner: H,
    enabled: bool,
}

impl<H> ToggleHistoryAccess<H> {
    pub fn new(inner: H, enabled: bool) -> Self {
        Self { inner, enabled }
    }

    pub fn enabled(inner: H) -> Self {
        Self::new(inner, true)
    }

    pub fn disabled(inner: H) -> Self {
        Self::new(inner, false)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub fn inner(&self) -> &H {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut H {
        &mut self.inner
    }

    pub fn into_inner(self) -> H {
        self.inner
    }
}

impl<S, H> HistoryAccess<S> for ToggleHistoryAccess<H>
where
    S: EventStore,
    H: HistoryAccess<S>,
{
    type Error = ToggleHistoryAccessError<H::Error>;

    fn definitions(&self) -> &[ToolDefinition] {
        if self.enabled {
            self.inner.definitions()
        } else {
            &[]
        }
    }

    fn owns_tool(&self, name: &str) -> bool {
        self.enabled && self.inner.owns_tool(name)
    }

    async fn execute_bounded(
        &self,
        event_store: &S,
        context: HistoryContext,
        call: ToolCall,
        max_output_chars: Option<usize>,
    ) -> Result<String, Self::Error> {
        if !self.enabled {
            return Err(ToggleHistoryAccessError::Disabled);
        }

        self.inner
            .execute_bounded(event_store, context, call, max_output_chars)
            .await
            .map_err(ToggleHistoryAccessError::Inner)
    }
}

#[derive(Debug)]
pub enum ToggleHistoryAccessError<E> {
    Disabled,
    Inner(E),
}

impl<E: fmt::Display> fmt::Display for ToggleHistoryAccessError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => write!(formatter, "history access is disabled"),
            Self::Inner(source) => source.fmt(formatter),
        }
    }
}

impl<E: Error + 'static> Error for ToggleHistoryAccessError<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Disabled => None,
            Self::Inner(source) => Some(source),
        }
    }
}
impl<H: ToolOutputProjector> ToolOutputProjector for ToggleHistoryAccess<H> {
    fn project_output(
        &self,
        tool_name: &str,
        output: &str,
        max_chars: usize,
    ) -> Option<Result<String, String>> {
        // Disabling new calls must not invalidate already persisted responses.
        self.inner.project_output(tool_name, output, max_chars)
    }
}

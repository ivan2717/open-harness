use std::{error::Error, future::Future};

pub use agent_core::provider_state::ProviderToolCall as ToolCall;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

/// Projects saved structured tool outputs without corrupting their protocol.
///
/// Return `Some` for an owned tool, including an error when a valid response
/// cannot fit. Return `None` to let the context manager clip ordinary text.
/// The limit counts Unicode scalar values, not bytes. Successful output and
/// error messages must fit the limit, and pagination cursors must describe the
/// content actually retained in the projected response.
///
/// Projection must be deterministic and free of side effects: context planning
/// may call it multiple times before constructing the final model request.
pub trait ToolOutputProjector: Send + Sync {
    fn project_output(
        &self,
        tool_name: &str,
        output: &str,
        max_chars: usize,
    ) -> Option<Result<String, String>>;
}

pub trait ToolExecutor: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn definitions(&self) -> &[ToolDefinition];

    /// Captures the tools available for one execution turn. Dynamic executors
    /// may refresh configuration here; the runtime keeps this snapshot stable
    /// throughout model requests, context accounting, and tool dispatch.
    /// Execution must still enforce current permissions after revocation.
    fn snapshot_definitions(&self) -> Vec<ToolDefinition> {
        self.definitions().to_vec()
    }

    /// Executes one call. The runtime drops this future when cancellation wins.
    ///
    /// Implementations must make dropping the future stop any locally owned
    /// work (for example, kill child processes on drop), and must not detach
    /// side-effecting work without their own cancellation and reconciliation.
    /// Already committed external effects cannot be undone by the runtime.
    fn execute(&self, call: ToolCall) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

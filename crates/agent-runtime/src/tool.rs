use std::{error::Error, future::Future};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: String,
}

pub trait ToolExecutor: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn definitions(&self) -> &[ToolDefinition];

    fn execute(&self, call: ToolCall) -> impl Future<Output = Result<String, Self::Error>> + Send;
}

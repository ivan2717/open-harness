use std::{error::Error, future::Future};

use crate::{ToolCall, ToolDefinition};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRequest {
    pub instructions: String,
    pub messages: Vec<ModelMessage>,
    pub tools: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelMessage {
    User {
        content: String,
    },
    Assistant {
        content: String,
    },
    ToolCall {
        call_id: String,
        tool_name: String,
        arguments: String,
    },
    ToolResult {
        call_id: String,
        output: String,
    },
    ToolError {
        call_id: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelResponse {
    FinalAnswer {
        answer: String,
    },
    ToolCalls {
        content: String,
        calls: Vec<ToolCall>,
    },
    InputRequired {
        prompt: String,
    },
}

pub trait Model: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn respond(
        &self,
        request: ModelRequest,
    ) -> impl Future<Output = Result<ModelResponse, Self::Error>> + Send;
}

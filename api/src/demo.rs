//! Deterministic offline adapter. It exercises the real runtime without claiming
//! to answer arbitrary prompts or report provider-measured token usage.
use agent_runtime::{
    ContentBlock, Model, ModelBlockKind, ModelFinishReason, ModelRequest, ModelResponse,
    ModelStreamEvent, ModelStreamSink, ToolCall, ToolDefinition, ToolExecutor,
};
use std::{
    convert::Infallible,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

pub const MODEL_ID: &str = "local-demo";
pub const WINDOW_TOKENS: usize = 8192;

pub struct DemoModel {
    round: AtomicU32,
    ask: bool,
    delay: Duration,
}

impl DemoModel {
    pub fn new(round: u32, ask: bool, delay: Duration) -> Self {
        Self {
            round: AtomicU32::new(round),
            ask,
            delay,
        }
    }
    fn response(&self) -> ModelResponse {
        let round = self.round.fetch_add(1, Ordering::Relaxed);
        if self.ask && round == 0 {
            return ModelResponse::input_required(
                "这是本地演示：请补充一个验证重点，继续观察工具调用与上下文压缩。".into(),
            );
        }
        let effective_round = round - u32::from(self.ask);
        let reasoning = ContentBlock::Reasoning {
            text: format!(
                "本地脚本演示第 {} 轮：读取诊断样本，检查真实上下文预算，并保留近期工具结果。此内容由演示脚本生成，不是真实模型推理。",
                effective_round + 1
            ),
        };
        if effective_round < 4 {
            ModelResponse {
                content: vec![
                    reasoning,
                    ContentBlock::Text {
                        text: format!("读取第 {} 组本地诊断样本。", effective_round + 1),
                    },
                    ContentBlock::ToolCall {
                        id: format!("demo-call-{round}"),
                        name: "read_demo_sample".into(),
                        arguments: format!("{{\"round\":{}}}", effective_round + 1),
                    },
                ],
                finish_reason: ModelFinishReason::ToolCalls,
            }
        } else {
            ModelResponse { content: vec![reasoning, ContentBlock::Text { text: "本地演示已完成。四轮诊断工具通过真实 AgentRuntime 执行；右侧可查看实际输入估算、压缩检查点与完整事件记录。刷新或重启后可恢复已提交历史。此适配器不调用外部模型，也不执行真实文件或网络操作。".into() }], finish_reason: ModelFinishReason::Stop }
        }
    }
}

impl Model for DemoModel {
    type Error = Infallible;
    async fn respond(&self, _: ModelRequest) -> Result<ModelResponse, Self::Error> {
        Ok(self.response())
    }
    async fn stream(
        &self,
        _: ModelRequest,
        sink: &mut dyn ModelStreamSink,
    ) -> Result<(), Self::Error> {
        let response = self.response();
        for (index, block) in response.content.into_iter().enumerate() {
            let kind = match &block {
                ContentBlock::Text { .. } => ModelBlockKind::Text,
                ContentBlock::Reasoning { .. } => ModelBlockKind::Reasoning,
                ContentBlock::ToolCall { .. } => ModelBlockKind::ToolCall,
                ContentBlock::ToolResult { .. } => ModelBlockKind::ToolResult,
                ContentBlock::Image { .. } => unreachable!("demo produces only assistant output"),
            };
            sink.emit(ModelStreamEvent::BlockStart { index, kind });
            if let ContentBlock::Text { text } | ContentBlock::Reasoning { text } = &block {
                let chars = text.chars().collect::<Vec<_>>();
                for chunk in chars.chunks(12) {
                    tokio::time::sleep(self.delay).await;
                    let text = chunk.iter().collect();
                    sink.emit(if kind == ModelBlockKind::Reasoning {
                        ModelStreamEvent::ReasoningDelta { index, text }
                    } else {
                        ModelStreamEvent::TextDelta { index, text }
                    });
                }
            }
            sink.emit(ModelStreamEvent::BlockEnd { index, block });
        }
        sink.emit(ModelStreamEvent::Finish {
            finish_reason: response.finish_reason,
            provider_state: None,
            provider_context: None,
            context_compacted: false,
        });
        Ok(())
    }
}

pub struct DemoTools {
    definitions: Vec<ToolDefinition>,
    delay: Duration,
}
impl DemoTools {
    pub fn new(delay: Duration) -> Self {
        Self { definitions: vec![ToolDefinition { name: "read_demo_sample".into(), description: "Return a deterministic local diagnostic sample for context-budget testing; no network or filesystem access.".into(), input_schema: r#"{"type":"object","properties":{"round":{"type":"integer","minimum":1,"maximum":4}},"required":["round"],"additionalProperties":false}"#.into() }], delay }
    }
}
impl ToolExecutor for DemoTools {
    type Error = std::io::Error;
    fn definitions(&self) -> &[ToolDefinition] {
        &self.definitions
    }
    async fn execute(&self, call: ToolCall) -> Result<String, Self::Error> {
        if call.name != "read_demo_sample" {
            return Err(std::io::Error::other("unknown demonstration tool"));
        }
        let value: serde_json::Value = serde_json::from_str(&call.arguments)?;
        let round = value["round"]
            .as_u64()
            .filter(|round| (1..=4).contains(round))
            .ok_or_else(|| std::io::Error::other("invalid demonstration round"))?;
        tokio::time::sleep(self.delay).await;
        let mut result = format!(
            "LOCAL DEMO SAMPLE {round}\nSynthetic observations for testing compaction; no external actions occurred.\n"
        );
        for index in 0..72 {
            result.push_str(&format!("sample={round}; observation={index:03}; outcome=ok; note=local-context-diagnostic\n"));
        }
        Ok(result)
    }
}

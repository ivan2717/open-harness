//! Normalize complete provider usage snapshots before they enter the runtime.

use std::{error::Error, fmt};

use agent_core::token_usage::ModelUsage;
use serde_json::{Map, Value};

use crate::WireProtocol;

/// Invalid provider accounting must not become a plausible zero-token sample.
/// Errors identify field names only, never response contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsageDecodeError {
    MissingField(&'static str),
    InvalidField(&'static str),
    InconsistentFields(&'static str),
    Overflow,
}

impl fmt::Display for UsageDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(field) => write!(f, "missing usage field {field}"),
            Self::InvalidField(field) => write!(f, "invalid usage field {field}"),
            Self::InconsistentFields(field) => write!(f, "inconsistent usage field {field}"),
            Self::Overflow => f.write_str("provider token usage exceeds the supported range"),
        }
    }
}

impl Error for UsageDecodeError {}

impl WireProtocol {
    /// Decode usage from a complete response envelope (not a bare usage object).
    /// Missing or null usage is `None`; explicit zero counts remain `Some`.
    ///
    /// This accepts a settled snapshot. In particular, Anthropic streaming
    /// `message_start` and `message_delta` usage must first be merged by the
    /// transport adapter. Snapshots replace earlier samples; never sum them.
    /// Provider-internal multi-hop/tool execution may report billed input beyond
    /// the original prompt. Transports enabling such modes must disable input
    /// calibration in their model metadata with
    /// `ModelContextMetadata::with_input_usage_calibration(false)`, while still
    /// passing the usage to the runtime for spending accounting. For example,
    /// Gemini's `toolUsePromptTokenCount` counts additional internal prompts.
    pub fn decode_usage(self, response: &Value) -> Result<Option<ModelUsage>, UsageDecodeError> {
        let response = response
            .as_object()
            .ok_or(UsageDecodeError::InvalidField("response"))?;
        let field = match self {
            Self::GeminiGenerateContent => "usageMetadata",
            _ => "usage",
        };
        let Some(usage) = optional_object(response, field)? else {
            return Ok(None);
        };
        let usage = match self {
            Self::ChatCompletions => chat(usage)?,
            Self::Responses => responses(usage)?,
            Self::AnthropicMessages => anthropic(usage)?,
            Self::GeminiGenerateContent => gemini(usage)?,
        };
        if usage
            .cached_input_tokens
            .is_some_and(|cached| cached > usage.input_tokens)
        {
            return Err(UsageDecodeError::InconsistentFields("cached_input_tokens"));
        }
        Ok(Some(usage))
    }
}

fn chat(usage: &Map<String, Value>) -> Result<ModelUsage, UsageDecodeError> {
    let input_tokens = required_count(usage, "prompt_tokens")?;
    let output_tokens = required_count(usage, "completion_tokens")?;
    // DeepSeek exposes the same cache-hit count under both names. Neither is
    // additional input, and neither may be added to prompt_tokens.
    let details = nested_count(usage, "prompt_tokens_details", "cached_tokens")?;
    let deepseek = optional_count(usage, "prompt_cache_hit_tokens")?;
    if details.zip(deepseek).is_some_and(|(a, b)| a != b) {
        return Err(UsageDecodeError::InconsistentFields(
            "prompt_cache_hit_tokens",
        ));
    }
    if let (Some(hit), Some(miss)) = (
        details.or(deepseek),
        optional_count(usage, "prompt_cache_miss_tokens")?,
    ) && add(hit, miss)? != input_tokens
    {
        return Err(UsageDecodeError::InconsistentFields(
            "prompt_cache_miss_tokens",
        ));
    }
    validate_reasoning(
        nested_count(usage, "completion_tokens_details", "reasoning_tokens")?,
        output_tokens,
    )?;
    validate_total(usage, "total_tokens", input_tokens, output_tokens)?;
    Ok(ModelUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens: details.or(deepseek),
    })
}

fn responses(usage: &Map<String, Value>) -> Result<ModelUsage, UsageDecodeError> {
    let input_tokens = required_count(usage, "input_tokens")?;
    let output_tokens = required_count(usage, "output_tokens")?;
    validate_reasoning(
        nested_count(usage, "output_tokens_details", "reasoning_tokens")?,
        output_tokens,
    )?;
    validate_total(usage, "total_tokens", input_tokens, output_tokens)?;
    Ok(ModelUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens: nested_count(usage, "input_tokens_details", "cached_tokens")?,
    })
}

fn anthropic(usage: &Map<String, Value>) -> Result<ModelUsage, UsageDecodeError> {
    // Anthropic's input_tokens excludes both cache reads and cache writes.
    // The cache_creation breakdown by TTL is already included in its total.
    let uncached = required_count(usage, "input_tokens")?;
    let read = optional_count(usage, "cache_read_input_tokens")?;
    let written = optional_count(usage, "cache_creation_input_tokens")?;
    Ok(ModelUsage {
        input_tokens: add(add(uncached, read.unwrap_or(0))?, written.unwrap_or(0))?,
        // Extended thinking is already included in output_tokens.
        output_tokens: required_count(usage, "output_tokens")?,
        cached_input_tokens: read,
    })
}

fn gemini(usage: &Map<String, Value>) -> Result<ModelUsage, UsageDecodeError> {
    // promptTokenCount includes cached content but excludes provider tool-use
    // prompts. Candidate output excludes thoughts, unlike OpenAI/Anthropic.
    let prompt = required_count(usage, "promptTokenCount")?;
    let input_tokens = add(
        prompt,
        optional_count(usage, "toolUsePromptTokenCount")?.unwrap_or(0),
    )?;
    let output_tokens = add(
        optional_count(usage, "candidatesTokenCount")?.unwrap_or(0),
        optional_count(usage, "thoughtsTokenCount")?.unwrap_or(0),
    )?;
    let cached_input_tokens = optional_count(usage, "cachedContentTokenCount")?;
    if cached_input_tokens.is_some_and(|cached| cached > prompt) {
        return Err(UsageDecodeError::InconsistentFields(
            "cachedContentTokenCount",
        ));
    }
    validate_total(usage, "totalTokenCount", input_tokens, output_tokens)?;
    Ok(ModelUsage {
        input_tokens,
        output_tokens,
        cached_input_tokens,
    })
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'a Map<String, Value>>, UsageDecodeError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(value)) => Ok(Some(value)),
        Some(_) => Err(UsageDecodeError::InvalidField(field)),
    }
}

fn required_count(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<usize, UsageDecodeError> {
    optional_count(object, field)?.ok_or(UsageDecodeError::MissingField(field))
}

fn optional_count(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<usize>, UsageDecodeError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let count = value
                .as_u64()
                .ok_or(UsageDecodeError::InvalidField(field))?;
            usize::try_from(count)
                .map(Some)
                .map_err(|_| UsageDecodeError::Overflow)
        }
    }
}

fn nested_count(
    usage: &Map<String, Value>,
    details: &'static str,
    field: &'static str,
) -> Result<Option<usize>, UsageDecodeError> {
    optional_object(usage, details)?
        .map(|object| optional_count(object, field))
        .transpose()
        .map(Option::flatten)
}

fn validate_reasoning(tokens: Option<usize>, output: usize) -> Result<(), UsageDecodeError> {
    if tokens.is_some_and(|tokens| tokens > output) {
        return Err(UsageDecodeError::InconsistentFields("reasoning_tokens"));
    }
    Ok(())
}

fn validate_total(
    usage: &Map<String, Value>,
    field: &'static str,
    input: usize,
    output: usize,
) -> Result<(), UsageDecodeError> {
    if let Some(total) = optional_count(usage, field)?
        && total != add(input, output)?
    {
        return Err(UsageDecodeError::InconsistentFields(field));
    }
    Ok(())
}

fn add(a: usize, b: usize) -> Result<usize, UsageDecodeError> {
    a.checked_add(b).ok_or(UsageDecodeError::Overflow)
}

#[cfg(test)]
mod tests;

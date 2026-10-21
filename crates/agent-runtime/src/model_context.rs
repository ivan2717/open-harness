use std::{error::Error, fmt};

/// Capabilities of the configured model, supplied by its adapter or catalog.
///
/// `context_window` is the normal operating window; a larger
/// `max_context_window` is available only when the caller explicitly opts in.
/// Provider and model names should identify the deployed model/revision so
/// observations from a different model cannot calibrate its token accounting.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelContextMetadata {
    pub provider: String,
    pub model: String,
    pub context_window: usize,
    pub max_context_window: Option<usize>,
    pub auto_compact_token_limit: Option<usize>,
    pub effective_context_window_percent: u8,
    pub max_output_tokens: Option<usize>,
}

impl ModelContextMetadata {
    pub fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        context_window: usize,
    ) -> Result<Self, ModelContextMetadataError> {
        let metadata = Self {
            provider: provider.into(),
            model: model.into(),
            context_window,
            max_context_window: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            max_output_tokens: None,
        };
        metadata.validate()?;
        Ok(metadata)
    }

    /// Validate catalog data again after callers customize its public fields.
    pub fn validate(&self) -> Result<(), ModelContextMetadataError> {
        if self.provider.trim().is_empty() || self.model.trim().is_empty() {
            return Err(ModelContextMetadataError::EmptyIdentity);
        }
        if self.context_window == 0 {
            return Err(ModelContextMetadataError::ZeroContextWindow);
        }
        if let Some(maximum) = self.max_context_window
            && maximum < self.context_window
        {
            return Err(ModelContextMetadataError::MaximumBelowDefault {
                context_window: self.context_window,
                max_context_window: maximum,
            });
        }
        if !(1..=100).contains(&self.effective_context_window_percent) {
            return Err(ModelContextMetadataError::InvalidEffectiveWindowPercent(
                self.effective_context_window_percent,
            ));
        }
        if self.auto_compact_token_limit == Some(0) {
            return Err(ModelContextMetadataError::ZeroAutoCompactTokenLimit);
        }
        if self.max_output_tokens == Some(0) {
            return Err(ModelContextMetadataError::ZeroMaxOutputTokens);
        }
        Ok(())
    }

    /// Without a separately advertised maximum, the known window is the cap.
    pub fn maximum_context_window(&self) -> usize {
        self.max_context_window.unwrap_or(self.context_window)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelContextMetadataError {
    EmptyIdentity,
    ZeroContextWindow,
    MaximumBelowDefault {
        context_window: usize,
        max_context_window: usize,
    },
    InvalidEffectiveWindowPercent(u8),
    ZeroAutoCompactTokenLimit,
    ZeroMaxOutputTokens,
}

impl fmt::Display for ModelContextMetadataError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentity => {
                write!(formatter, "model provider and identity must be nonempty")
            }
            Self::ZeroContextWindow => write!(formatter, "model context window must be positive"),
            Self::MaximumBelowDefault {
                context_window,
                max_context_window,
            } => write!(
                formatter,
                "maximum context window ({max_context_window}) is below the default ({context_window})"
            ),
            Self::InvalidEffectiveWindowPercent(percent) => write!(
                formatter,
                "effective context window percentage ({percent}) must be between 1 and 100"
            ),
            Self::ZeroAutoCompactTokenLimit => {
                write!(
                    formatter,
                    "model automatic compaction limit must be positive"
                )
            }
            Self::ZeroMaxOutputTokens => {
                write!(formatter, "model maximum output tokens must be positive")
            }
        }
    }
}

impl Error for ModelContextMetadataError {}

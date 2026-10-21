use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryLimits {
    max_results: usize,
    max_events: usize,
    max_output_chars: usize,
    preview_chars: usize,
}

impl HistoryLimits {
    /// Start from the default limits and override individual limits by name.
    pub fn builder() -> HistoryLimitsBuilder {
        HistoryLimitsBuilder::default()
    }

    fn validate(
        max_results: usize,
        max_events: usize,
        max_output_chars: usize,
        preview_chars: usize,
    ) -> Result<Self, HistoryConfigurationError> {
        for (name, value) in [
            ("max_results", max_results),
            ("max_events", max_events),
            ("max_output_chars", max_output_chars),
            ("preview_chars", preview_chars),
        ] {
            if value == 0 {
                return Err(HistoryConfigurationError::ZeroLimit { name });
            }
        }

        if preview_chars > max_output_chars {
            return Err(HistoryConfigurationError::PreviewExceedsOutput {
                preview_chars,
                max_output_chars,
            });
        }

        Ok(Self {
            max_results,
            max_events,
            max_output_chars,
            preview_chars,
        })
    }

    pub fn max_results(self) -> usize {
        self.max_results
    }

    pub fn max_events(self) -> usize {
        self.max_events
    }

    pub fn max_output_chars(self) -> usize {
        self.max_output_chars
    }

    pub fn preview_chars(self) -> usize {
        self.preview_chars
    }
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_results: 8,
            max_events: 20,
            max_output_chars: 16_000,
            preview_chars: 400,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryConfigurationError {
    ZeroLimit {
        name: &'static str,
    },
    PreviewExceedsOutput {
        preview_chars: usize,
        max_output_chars: usize,
    },
}

impl fmt::Display for HistoryConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroLimit { name } => write!(formatter, "history limit {name} must be non-zero"),
            Self::PreviewExceedsOutput {
                preview_chars,
                max_output_chars,
            } => write!(
                formatter,
                "history preview limit ({preview_chars}) exceeds output limit ({max_output_chars})"
            ),
        }
    }
}

impl Error for HistoryConfigurationError {}

/// Named configuration for history search, reads, and encoded payloads.
///
/// All sizes count Unicode scalar values where applicable. Validation runs once
/// at `build`, before the limits are accepted by a history adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct HistoryLimitsBuilder {
    limits: HistoryLimits,
}

impl HistoryLimitsBuilder {
    pub fn max_results(mut self, max_results: usize) -> Self {
        self.limits.max_results = max_results;
        self
    }

    pub fn max_events(mut self, max_events: usize) -> Self {
        self.limits.max_events = max_events;
        self
    }

    pub fn max_output_chars(mut self, max_output_chars: usize) -> Self {
        self.limits.max_output_chars = max_output_chars;
        self
    }

    pub fn preview_chars(mut self, preview_chars: usize) -> Self {
        self.limits.preview_chars = preview_chars;
        self
    }

    pub fn build(self) -> Result<HistoryLimits, HistoryConfigurationError> {
        HistoryLimits::validate(
            self.limits.max_results,
            self.limits.max_events,
            self.limits.max_output_chars,
            self.limits.preview_chars,
        )
    }
}

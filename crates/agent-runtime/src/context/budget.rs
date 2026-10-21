use std::{error::Error, fmt};

/// A model window split into input, maximum generated output, and safety margin.
///
/// The margin accounts for estimation and provider framing overhead. Providers
/// with exact token counts can reduce it; an approximate counter is not a
/// guarantee that a request fits the provider's tokenizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelContextBudget {
    context_window: usize,
    output_token_reserve: usize,
    safety_margin: usize,
    effective_context_window_percent: u8,
}

impl ModelContextBudget {
    pub fn new(
        context_window: usize,
        output_token_reserve: usize,
        safety_margin: usize,
    ) -> Result<Self, ModelContextBudgetError> {
        if context_window == 0 {
            return Err(ModelContextBudgetError::ZeroContextWindow);
        }
        if context_window
            .checked_sub(output_token_reserve)
            .and_then(|remaining| remaining.checked_sub(safety_margin))
            .is_none_or(|remaining| remaining == 0)
        {
            return Err(ModelContextBudgetError::NoInputCapacity {
                context_window,
                output_token_reserve,
                safety_margin,
            });
        }
        Ok(Self {
            context_window,
            output_token_reserve,
            safety_margin,
            effective_context_window_percent: 100,
        })
    }

    /// Caps input using a provider's usable-window percentage as well as the
    /// explicit output and safety reservations. The two margins are not added.
    pub fn with_effective_context_window_percent(
        mut self,
        percent: u8,
    ) -> Result<Self, ModelContextBudgetError> {
        if !(1..=100).contains(&percent) {
            return Err(ModelContextBudgetError::InvalidEffectiveWindowPercent(
                percent,
            ));
        }
        self.effective_context_window_percent = percent;
        if self.input_token_limit() == 0 {
            return Err(ModelContextBudgetError::NoInputCapacity {
                context_window: self.context_window,
                output_token_reserve: self.output_token_reserve,
                safety_margin: self.safety_margin,
            });
        }
        Ok(self)
    }

    pub fn effective_context_window_percent(self) -> u8 {
        self.effective_context_window_percent
    }

    pub fn context_window(self) -> usize {
        self.context_window
    }

    pub fn output_token_reserve(self) -> usize {
        self.output_token_reserve
    }

    pub fn safety_margin(self) -> usize {
        self.safety_margin
    }

    pub fn input_token_limit(self) -> usize {
        (self.context_window - self.output_token_reserve - self.safety_margin).min(percentage(
            self.context_window,
            self.effective_context_window_percent,
        ))
    }

    /// Zero reserve is retained for callers supplying a legacy input-only limit.
    pub fn output_token_limit(self) -> Option<usize> {
        (self.output_token_reserve > 0).then_some(self.output_token_reserve)
    }
}

impl Default for ModelContextBudget {
    fn default() -> Self {
        Self {
            context_window: 128_000,
            output_token_reserve: 8_192,
            safety_margin: 2_048,
            effective_context_window_percent: 100,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelContextBudgetError {
    ZeroContextWindow,
    InvalidEffectiveWindowPercent(u8),
    NoInputCapacity {
        context_window: usize,
        output_token_reserve: usize,
        safety_margin: usize,
    },
}

impl fmt::Display for ModelContextBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroContextWindow => write!(formatter, "model context window must be positive"),
            Self::InvalidEffectiveWindowPercent(percent) => write!(
                formatter,
                "effective context window percentage ({percent}) must be between 1 and 100"
            ),
            Self::NoInputCapacity {
                context_window,
                output_token_reserve,
                safety_margin,
            } => write!(
                formatter,
                "model window ({context_window}) must exceed output reserve ({output_token_reserve}) plus safety margin ({safety_margin})"
            ),
        }
    }
}

impl Error for ModelContextBudgetError {}

/// Floor of a percentage without overflowing even for `usize::MAX`.
pub(super) fn percentage(tokens: usize, percent: u8) -> usize {
    tokens / 100 * usize::from(percent) + tokens % 100 * usize::from(percent) / 100
}

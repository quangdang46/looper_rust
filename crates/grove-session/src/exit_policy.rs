#![allow(clippy::unwrap_used, clippy::expect_used)]
use grove_types::IterationAnalysis;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDecision {
    Continue,
    Success,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitPolicy {
    pub completion_indicator_threshold: u32,
    pub require_explicit_exit: bool,
}

impl Default for ExitPolicy {
    fn default() -> Self {
        Self {
            completion_indicator_threshold: 2,
            require_explicit_exit: true,
        }
    }
}

impl ExitPolicy {
    #[must_use]
    pub fn evaluate(&self, analysis: &IterationAnalysis) -> ExitDecision {
        if analysis.has_explicit_exit_false {
            return ExitDecision::Continue;
        }

        if analysis.has_explicit_exit_true
            && (analysis.completion_indicators >= self.completion_indicator_threshold
                || !analysis.artifacts_mentioned.is_empty()
                || !analysis.lessons.is_empty()
                || !analysis.decisions.is_empty())
        {
            return ExitDecision::Success;
        }

        if !self.require_explicit_exit
            && analysis.completion_indicators >= self.completion_indicator_threshold
        {
            ExitDecision::Success
        } else {
            ExitDecision::Continue
        }
    }
}

#[cfg(test)]
mod tests;

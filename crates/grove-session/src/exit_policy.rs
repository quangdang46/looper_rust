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

        if self.require_explicit_exit {
            if analysis.has_explicit_exit_true
                && analysis.completion_indicators >= self.completion_indicator_threshold
            {
                ExitDecision::Success
            } else {
                ExitDecision::Continue
            }
        } else if analysis.completion_indicators >= self.completion_indicator_threshold {
            ExitDecision::Success
        } else {
            ExitDecision::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grove_types::IterationAnalysis;

    #[test]
    fn explicit_exit_false_always_continues() {
        let analysis = IterationAnalysis {
            completion_indicators: 10,
            has_explicit_exit_true: true,
            has_explicit_exit_false: true,
            ..IterationAnalysis::default()
        };

        assert_eq!(
            ExitPolicy::default().evaluate(&analysis),
            ExitDecision::Continue
        );
    }

    #[test]
    fn explicit_exit_true_with_threshold_met_succeeds() {
        let analysis = IterationAnalysis {
            completion_indicators: 2,
            has_explicit_exit_true: true,
            ..IterationAnalysis::default()
        };

        assert_eq!(
            ExitPolicy::default().evaluate(&analysis),
            ExitDecision::Success
        );
    }

    #[test]
    fn explicit_exit_true_without_enough_indicators_continues() {
        let analysis = IterationAnalysis {
            completion_indicators: 1,
            has_explicit_exit_true: true,
            ..IterationAnalysis::default()
        };

        assert_eq!(
            ExitPolicy::default().evaluate(&analysis),
            ExitDecision::Continue
        );
    }

    #[test]
    fn missing_explicit_exit_continues_when_required() {
        let analysis = IterationAnalysis {
            completion_indicators: 5,
            ..IterationAnalysis::default()
        };

        assert_eq!(
            ExitPolicy::default().evaluate(&analysis),
            ExitDecision::Continue
        );
    }

    #[test]
    fn threshold_only_can_succeed_when_explicit_exit_not_required() {
        let analysis = IterationAnalysis {
            completion_indicators: 2,
            ..IterationAnalysis::default()
        };
        let policy = ExitPolicy {
            completion_indicator_threshold: 2,
            require_explicit_exit: false,
        };

        assert_eq!(policy.evaluate(&analysis), ExitDecision::Success);
    }
}

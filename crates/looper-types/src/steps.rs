use crate::error::DomainError;
use crate::loop_type::LoopType;

/// Validate that a step name belongs to the given loop type's step sequence.
pub fn assert_step_belongs_to_loop_type(loop_type: LoopType, step: &str) -> Result<(), DomainError> {
    if loop_type.steps().contains(&step) {
        Ok(())
    } else {
        Err(DomainError::StepNotInLoopType {
            step: step.to_string(),
            loop_type,
        })
    }
}

/// Return all known steps across all loop types.
pub fn all_steps() -> Vec<&'static str> {
    use LoopType::*;
    let mut steps: Vec<&str> = Vec::new();
    for lt in &[Planner, Reviewer, Worker, Fixer] {
        for s in lt.steps() {
            if !steps.contains(s) {
                steps.push(s);
            }
        }
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_step_for_loop_type() {
        assert!(assert_step_belongs_to_loop_type(LoopType::Planner, "discover").is_ok());
        assert!(assert_step_belongs_to_loop_type(LoopType::Worker, "implement").is_ok());
        assert!(assert_step_belongs_to_loop_type(LoopType::Fixer, "patch").is_ok());
        assert!(assert_step_belongs_to_loop_type(LoopType::Reviewer, "review").is_ok());
    }

    #[test]
    fn test_invalid_step_for_loop_type() {
        assert!(assert_step_belongs_to_loop_type(LoopType::Planner, "patch").is_err());
        assert!(assert_step_belongs_to_loop_type(LoopType::Fixer, "discover").is_err());
    }

    #[test]
    fn test_all_planner_steps_are_valid() {
        for s in LoopType::Planner.steps() {
            assert_step_belongs_to_loop_type(LoopType::Planner, s).unwrap();
        }
    }

    #[test]
    fn test_all_steps_deduplicated() {
        let steps = all_steps();
        let mut sorted = steps.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(steps.len(), sorted.len(), "all_steps() should not contain duplicates");
    }
}

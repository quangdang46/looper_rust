//! Environment variable building for agent processes.

use std::collections::HashMap;

/// Git environment keys that are stripped to prevent directory confusion attacks.
const UNSAFE_GIT_ENV_KEYS: &[&str] = &[
    "OLDPWD",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
];

/// Build the environment variable list for an agent process.
///
/// Starts from the current process environment, applies config env sources,
/// strips unsafe Git env vars, and injects LOOPER_PROMPT, LOOPER_COMPLETION_MARKER,
/// and PWD.
pub fn build_command_env(
    working_directory: &str,
    prompt: &str,
    env_sources: &[&HashMap<String, String>],
) -> Vec<String> {
    // Start with current process env
    let mut env_map: HashMap<String, String> = HashMap::new();

    // Inherit current process environment
    for (key, value) in std::env::vars() {
        env_map.insert(key, value);
    }

    // Apply config env sources in order
    for source in env_sources {
        for (key, value) in (*source).iter() {
            env_map.insert(key.clone(), value.clone());
        }
    }

    // Strip unsafe Git env vars
    for key in UNSAFE_GIT_ENV_KEYS {
        env_map.remove(*key);
    }

    // Set PWD
    env_map.insert("PWD".to_string(), working_directory.to_string());

    // Set LOOPER_PROMPT
    env_map.insert("LOOPER_PROMPT".to_string(), prompt.to_string());

    // Set LOOPER_COMPLETION_MARKER
    env_map.insert("LOOPER_COMPLETION_MARKER".to_string(), crate::types::COMPLETION_MARKER.to_string());

    // Convert to KEY=VALUE format and sort
    let mut result: Vec<String> = env_map.into_iter().map(|(k, v)| format!("{}={}", k, v)).collect();
    result.sort();

    result
}

pub fn detect_agent_setup_failure(message: &str) -> bool {
    let lower = message.to_lowercase();

    // Codex version mismatch
    if lower.contains("model requires a newer version of codex")
        || (lower.contains("please upgrade") && lower.contains("codex"))
    {
        return true;
    }

    // Model setup failures
    let setup_phrases =
        ["unsupported model", "unknown model", "invalid model", "model is not supported", "unrecognized model"];

    let has_setup_phrase = setup_phrases.iter().any(|p| lower.contains(p));
    let has_agent_name = ["codex", "claude", "opencode", "cursor", "hermes"].iter().any(|n| lower.contains(n));
    let has_model_context = lower.contains("model");

    has_setup_phrase && has_agent_name && has_model_context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_command_env_includes_basic_vars() {
        let env = build_command_env("/tmp/worktree", "hello", &[]);
        assert!(env.iter().any(|e| e.starts_with("PWD=")));
        assert!(env.iter().any(|e| e.starts_with("LOOPER_PROMPT=")));
        assert!(env.iter().any(|e| e.starts_with("LOOPER_COMPLETION_MARKER=")));
        // Check PWD is set correctly
        assert!(env.iter().any(|e| e == "PWD=/tmp/worktree"));
    }

    #[test]
    fn test_unsafe_git_keys_stripped() {
        // Temporarily set some unsafe vars
        let env = build_command_env("/tmp/worktree", "hello", &[]);
        // These should NOT be in the output
        assert!(!env.iter().any(|e| e.starts_with("GIT_DIR=")));
        assert!(!env.iter().any(|e| e.starts_with("GIT_WORK_TREE=")));
        assert!(!env.iter().any(|e| e.starts_with("OLDPWD=")));
    }

    #[test]
    fn test_env_sources_applied_in_order() {
        let mut source1 = HashMap::new();
        source1.insert("MY_VAR".to_string(), "from_source1".to_string());
        let mut source2 = HashMap::new();
        source2.insert("MY_VAR".to_string(), "from_source2".to_string());

        let env = build_command_env("/tmp/worktree", "hello", &[&source1, &source2]);
        let my_var = env.iter().find(|e| e.starts_with("MY_VAR="));
        assert!(my_var.is_some());
        assert_eq!(my_var.unwrap(), "MY_VAR=from_source2");
    }

    #[test]
    fn test_setup_failure_detection_codex() {
        assert!(detect_agent_setup_failure("This model requires a newer version of codex"));
        assert!(detect_agent_setup_failure("unsupported model gpt-5 for codex"));
        assert!(!detect_agent_setup_failure("unknown command: foobar"));
        assert!(!detect_agent_setup_failure("connection refused"));
    }

    #[test]
    fn test_env_is_sorted() {
        let env = build_command_env("/tmp/worktree", "hello", &[]);
        for pair in env.windows(2) {
            assert!(pair[0] <= pair[1]);
        }
    }
}

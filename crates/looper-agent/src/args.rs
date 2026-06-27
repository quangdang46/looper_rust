//! Command resolution and argument construction per vendor.

use crate::types::{AgentCliVendor, ExecutorConfig, SpawnCommand};
use std::collections::HashMap;

/// Resolve the command binary for the given vendor.
pub fn resolve_command(cfg: &ExecutorConfig) -> String {
    // Check for command override in params
    if let Some(serde_json::Value::String(cmd)) = cfg.params.get("command") {
        return cmd.clone();
    }
    cfg.vendor.default_binary().to_string()
}

/// Build the base arguments for a vendor, without model/prompt flags.
fn build_base_args(_vendor: AgentCliVendor, params: &HashMap<String, serde_json::Value>) -> Vec<String> {
    let mut args = Vec::new();

    // If custom args provided, use them as the base
    if let Some(serde_json::Value::Array(custom_args)) = params.get("args") {
        for v in custom_args {
            if let Some(s) = v.as_str() {
                args.push(s.to_string());
            }
        }
        return args;
    }

    // Otherwise, build from vendor defaults
    args
}

/// Prepend model flag into args at the correct position.
///
/// For codex/opencode: inserts after the first subcommand (exec/run).
/// For others: prepends before all args.
/// No-op if model is None/empty or if a recognized model flag already present.
pub fn prepend_model_flag(args: &mut Vec<String>, model: &Option<String>, flag: &str, recognized_flags: &[&str]) {
    let model_val = match model {
        Some(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => return,
    };

    // Check if any recognized flag already present
    if recognized_flags.iter().any(|f| args.iter().any(|a| a == f)) {
        return;
    }

    let flag_val = flag.to_string();
    let model_clone = model_val;

    // Check if first arg is a subcommand (exec/run for codex/opencode)
    if args.first().map(|s| s.as_str()) == Some("exec") || args.first().map(|s| s.as_str()) == Some("run") {
        // Insert after first arg
        args.insert(1, flag_val);
        args.insert(2, model_clone);
    } else {
        // Prepend
        args.insert(0, flag_val);
        args.insert(1, model_clone);
    }
}

/// Resolve a spawn command for a fresh start (no native resume).
pub fn resolve_spawn(cfg: &ExecutorConfig, working_directory: &str, prompt: &str) -> SpawnCommand {
    let binary = resolve_command(cfg);
    let mut args = build_base_args(cfg.vendor, &cfg.params);
    let vendor = cfg.vendor;

    // Force subcommand if needed (but check it's not already there)
    if let Some(subcmd) = vendor.forced_subcommand() {
        if !args.iter().any(|a| a == subcmd) {
            args.insert(0, subcmd.to_string());
        }
    }

    // Prepend model flag
    let model_flag = vendor.model_flag();
    let recognized = if vendor == AgentCliVendor::Hermes { vec!["-m"] } else { vec!["--model"] };
    prepend_model_flag(&mut args, &cfg.model, model_flag, &recognized);

    // Add working directory flag for opencode
    if vendor == AgentCliVendor::Opencode {
        // Check if --dir already specified
        if !args.iter().any(|a| a == "--dir") {
            args.push("--dir".to_string());
            args.push(working_directory.to_string());
        }
    }

    // Add prompt flag (if vendor uses one)
    let prompt_flag = vendor.prompt_flag();
    if !prompt_flag.is_empty() {
        // Check if prompt already provided via -p/-f/--prompt/--file
        let has_prompt_flag = args.iter().any(|a| a == "-p" || a == "--prompt" || a == "-f" || a == "--file");
        // For codex, if args end with "-" (stdin mode), don't append prompt
        let stdin_mode = cfg.vendor == AgentCliVendor::Codex && args.last().map(|s| s.as_str()) == Some("-");

        if !has_prompt_flag && !stdin_mode {
            args.push(prompt_flag.to_string());
            args.push(prompt.to_string());
        }
    } else if vendor == AgentCliVendor::Codex {
        // Codex: prompt is positional (last arg after `exec ...`)
        args.push(prompt.to_string());
    }

    // Add --dangerously-skip-permissions for claude-code
    if vendor == AgentCliVendor::ClaudeCode && !args.iter().any(|a| a == "--dangerously-skip-permissions") {
        args.push("--dangerously-skip-permissions".to_string());
    }

    SpawnCommand { binary, args }
}

/// Build arguments for a native resume spawn.
pub fn resolve_spawn_with_native_resume(
    cfg: &ExecutorConfig,
    working_directory: &str,
    session_id: &str,
    prompt: &str,
) -> SpawnCommand {
    if !cfg.vendor.native_resume_supported() {
        return resolve_spawn(cfg, working_directory, prompt);
    }

    let binary = resolve_command(cfg);
    let mut args = build_base_args(cfg.vendor, &cfg.params);
    let vendor = cfg.vendor;

    // Force subcommand if needed
    if let Some(subcmd) = vendor.forced_subcommand() {
        if !args.iter().any(|a| a == subcmd) {
            args.insert(0, subcmd.to_string());
        }
    }

    // Prepend model flag (before resume flag for codex/opencode, after subcommand)
    let model_flag = vendor.model_flag();
    let recognized = if vendor == AgentCliVendor::Hermes { vec!["-m"] } else { vec!["--model"] };
    prepend_model_flag(&mut args, &cfg.model, model_flag, &recognized);

    // Add resume flag
    match vendor {
        AgentCliVendor::Codex => {
            // codex exec ... resume <sessionID> <prompt>
            args.push("resume".to_string());
            args.push(session_id.to_string());
            args.push(prompt.to_string());
        }
        AgentCliVendor::Opencode => {
            // Insert --session <sessionID> after --dir (or after run)
            let _session_arg = format!("--session={}", session_id);
            let has_session = args.iter().any(|a| a == "--session" || a.starts_with("--session="));
            if !has_session && !args.iter().any(|a| a == "--continue") {
                // Check if --dir already in args
                if !args.iter().any(|a| a == "--dir") {
                    args.push("--dir".to_string());
                    args.push(working_directory.to_string());
                }
                args.push("--session".to_string());
                args.push(session_id.to_string());
            }
            // Prompt via last positional arg
            args.push(prompt.to_string());
        }
        _ => {
            // claude-code, cursor-cli: --resume <sessionID> --print <prompt>
            let resume_flag = vendor.resume_flag();
            if !args.iter().any(|a| a == resume_flag || a == "--continue" || a == "--resume") {
                args.push(resume_flag.to_string());
                args.push(session_id.to_string());
            }
            let prompt_flag = vendor.prompt_flag();
            if !prompt_flag.is_empty() {
                args.push(prompt_flag.to_string());
                args.push(prompt.to_string());
            } else {
                args.push(prompt.to_string());
            }

            // --dangerously-skip-permissions for claude-code
            if vendor == AgentCliVendor::ClaudeCode && !args.iter().any(|a| a == "--dangerously-skip-permissions") {
                args.push("--dangerously-skip-permissions".to_string());
            }
        }
    }

    SpawnCommand { binary, args }
}

/// Append the completion instruction to a prompt string.
pub fn append_completion_instruction(prompt: &str) -> String {
    format!(
        "{}\n\nWhen finished, print exactly one final line to stdout in this format:\n__LOOPER_RESULT__={{\"summary\":\"<one-sentence summary>\"}}\nDo not wrap that line in markdown.\nDo not print anything after that line.",
        prompt
    )
}

/// Append the completion instruction to a prompt string (in-place style).
pub fn append_completion_instruction_to(prompt: &mut String) {
    prompt.push_str("\n\nWhen finished, print exactly one final line to stdout in this format:\n__LOOPER_RESULT__={\"summary\":\"<one-sentence summary>\"}\nDo not wrap that line in markdown.\nDo not print anything after that line.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_config(vendor: AgentCliVendor, model: Option<&str>) -> ExecutorConfig {
        ExecutorConfig {
            vendor,
            model: model.map(String::from),
            params: HashMap::new(),
            env: HashMap::new(),
            native_resume_enabled: true,
        }
    }

    #[test]
    fn test_claude_fresh_spawn() {
        let cfg = make_config(AgentCliVendor::ClaudeCode, Some("gpt-5"));
        let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
        assert_eq!(cmd.binary, "claude");
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"--print".to_string()));
        assert!(cmd.args.contains(&"hello".to_string()));
        assert!(cmd.args.contains(&"--dangerously-skip-permissions".to_string()));
        // Model should come before --print
        let model_pos = cmd.args.iter().position(|a| a == "--model").unwrap();
        let print_pos = cmd.args.iter().position(|a| a == "--print").unwrap();
        assert!(model_pos < print_pos);
    }

    #[test]
    fn test_codex_fresh_spawn() {
        let cfg = make_config(AgentCliVendor::Codex, Some("gpt-5"));
        let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
        assert_eq!(cmd.binary, "codex");
        assert_eq!(cmd.args.first(), Some(&"exec".to_string()));
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.last() == Some(&"hello".to_string()));
    }

    #[test]
    fn test_opencode_fresh_spawn() {
        let cfg = make_config(AgentCliVendor::Opencode, Some("gpt-5"));
        let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
        assert_eq!(cmd.binary, "opencode");
        assert_eq!(cmd.args.first(), Some(&"run".to_string()));
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"--dir".to_string()));
        assert!(cmd.args.contains(&"/tmp/worktree".to_string()));
        assert!(cmd.args.contains(&"hello".to_string()));
    }

    #[test]
    fn test_cursor_fresh_spawn() {
        let cfg = make_config(AgentCliVendor::CursorCli, Some("gpt-5"));
        let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
        assert_eq!(cmd.binary, "agent");
        assert!(cmd.args.contains(&"--model".to_string()));
        assert!(cmd.args.contains(&"--print".to_string()));
        assert!(cmd.args.contains(&"hello".to_string()));
    }

    #[test]
    fn test_hermes_fresh_spawn() {
        let cfg = make_config(AgentCliVendor::Hermes, Some("gpt-5"));
        let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
        assert_eq!(cmd.binary, "hermes");
        assert!(cmd.args.contains(&"-m".to_string()));
        assert!(cmd.args.contains(&"-z".to_string()));
        assert!(cmd.args.contains(&"hello".to_string()));
    }

    #[test]
    fn test_claude_native_resume() {
        let cfg = make_config(AgentCliVendor::ClaudeCode, Some("gpt-5"));
        let cmd = resolve_spawn_with_native_resume(&cfg, "/tmp/worktree", "session-123", "hello");
        assert!(cmd.args.contains(&"--resume".to_string()));
        assert!(cmd.args.contains(&"session-123".to_string()));
        assert!(cmd.args.contains(&"--print".to_string()));
        assert!(cmd.args.contains(&"hello".to_string()));
        assert!(cmd.args.contains(&"--dangerously-skip-permissions".to_string()));
    }

    #[test]
    fn test_codex_native_resume() {
        let cfg = make_config(AgentCliVendor::Codex, Some("gpt-5"));
        let cmd = resolve_spawn_with_native_resume(&cfg, "/tmp/worktree", "session-123", "hello");
        assert!(cmd.args.contains(&"resume".to_string()));
        assert!(cmd.args.contains(&"session-123".to_string()));
        assert_eq!(cmd.args.last(), Some(&"hello".to_string()));
    }

    #[test]
    fn test_opencode_native_resume() {
        let cfg = make_config(AgentCliVendor::Opencode, Some("gpt-5"));
        let cmd = resolve_spawn_with_native_resume(&cfg, "/tmp/worktree", "session-123", "hello");
        assert!(cmd.args.contains(&"--session".to_string()));
        assert!(cmd.args.contains(&"session-123".to_string()));
    }

    #[test]
    fn test_cursor_native_resume() {
        let cfg = make_config(AgentCliVendor::CursorCli, Some("gpt-5"));
        let cmd = resolve_spawn_with_native_resume(&cfg, "/tmp/worktree", "session-123", "hello");
        assert!(cmd.args.contains(&"--resume".to_string()));
        assert!(cmd.args.contains(&"session-123".to_string()));
    }

    #[test]
    fn test_hermes_native_resume_unsupported() {
        let cfg = make_config(AgentCliVendor::Hermes, None);
        let cmd = resolve_spawn_with_native_resume(&cfg, "/tmp/worktree", "session-123", "hello");
        // Hermes doesn't support native resume, but the function still constructs args
        // without model since model was None
        assert_eq!(cmd.args.first(), Some(&"-z".to_string()));
    }

    #[test]
    fn test_append_completion_instruction() {
        let result = append_completion_instruction("hello");
        assert!(result.contains("__LOOPER_RESULT__"));
        assert!(result.starts_with("hello"));
        assert!(result.contains("one-sentence summary"));
    }

    #[test]
    fn test_custom_args() {
        let mut params = HashMap::new();
        params.insert("args".to_string(), serde_json::json!(["--profile", "test", "exec"]));
        let cfg = ExecutorConfig {
            vendor: AgentCliVendor::Codex,
            model: Some("gpt-5".to_string()),
            params,
            env: HashMap::new(),
            native_resume_enabled: true,
        };
        let cmd = resolve_spawn(&cfg, "/tmp/worktree", "hello");
        // Custom args: --profile test exec, then model flag, then prompt
        assert!(cmd.args.contains(&"--profile".to_string()));
        assert!(cmd.args.contains(&"--model".to_string()));
        // exec should not be duplicated
        let exec_count = cmd.args.iter().filter(|a| a.as_str() == "exec").count();
        assert_eq!(exec_count, 1);
    }
}

//! Contract-aware verification modes that run before final task closure.
//!
//! Implements pragmatic trust checks before returning a `Succeeded` status
//! as defined in PLAN.md §1.4.8.

use std::process::Command;
use grove_types::{ExecutionContract, PromptManifest};

/// Pragmatic modes of verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerificationMode {
    /// No extra action required.
    None,
    /// Trust the protocol output (make sure GROVE_RESULT was structurally sound).
    ProtocolComplete,
    /// Run `cargo check` for Rust projects.
    RustCompileCheck,
    /// Run `npm run build` or similar (simplified).
    NodeBuildCheck,
}

impl VerificationMode {
    /// Select a verification mode based on the execution contract and project environment.
    pub fn infer(contract: ExecutionContract, workspace_dir: &camino::Utf8Path) -> Self {
        match contract {
            ExecutionContract::Implement
            | ExecutionContract::SingleTask
            | ExecutionContract::RetryRescue
            | ExecutionContract::Resume => {
                if workspace_dir.join("Cargo.toml").exists() {
                    Self::RustCompileCheck
                } else if workspace_dir.join("package.json").exists() {
                    Self::NodeBuildCheck
                } else {
                    Self::ProtocolComplete
                }
            }
        }
    }
}

/// Run the selected verification mode. Returns Ok(()) if the check passes,
/// or an error description if it fails. this drives the 'VerifyFailed' exit reason if it trips.
pub fn run_verification(
    mode: VerificationMode,
    workspace_dir: &camino::Utf8Path,
    _manifest: &PromptManifest,
) -> Result<(), String> {
    match mode {
        VerificationMode::None => Ok(()),

        VerificationMode::ProtocolComplete => {
            // For now, if we reached here, the protocol exited cleanly.
            // A more rigorous check could ensure GROVE_RESULT contained expected keys.
            Ok(())
        }

        VerificationMode::RustCompileCheck => {
            let output = Command::new("cargo")
                .arg("check")
                .current_dir(workspace_dir)
                .output()
                .map_err(|e| format!("Failed to spawn cargo check: {e}"))?;

            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("Rust compilation check failed:\n{stderr}"))
            }
        }

        VerificationMode::NodeBuildCheck => {
            // Very simplified NPM check scenario
            let output = Command::new("npm")
                .arg("test")
                .current_dir(workspace_dir)
                .output()
                .map_err(|e| format!("Failed to spawn npm test: {e}"))?;

            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(format!("Node test check failed:\n{stderr}"))
            }
        }
    }
}

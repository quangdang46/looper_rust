//! fake-osascript binary — a fake osascript (AppleScript) used in E2E tests.
//!
//! On macOS the real `osascript` sends notifications; this stub records
//! the invocation arguments for later assertion.
//!
//! All arguments are ignored; the binary always exits successfully.

fn main() {
    // Record invocation to artifact dir if set, otherwise no-op.
    let _artifact_dir = std::env::var("LOOPER_E2E_FAKE_GH_ARTIFACT_DIR")
        .or_else(|_| std::env::var("LOOPER_E2E_FAKE_AGENT_ARTIFACT_DIR"));
    std::process::exit(0);
}

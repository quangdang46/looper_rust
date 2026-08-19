//! Plugin system for extending looper daemon behavior.
//!
//! Plugins implement the [`Plugin`] trait and are invoked at lifecycle hooks:
//! - `on_loop_start` — when a loop begins execution
//! - `on_loop_complete` — when a loop finishes (success or failure)
//! - `on_run_complete` — when a single agent run finishes
//! - `on_error` — when an error occurs during execution
//!
//! # Registration
//!
//! Plugins are registered at daemon startup via [`PluginManager`]:
//!
//! ```rust,no_run
//! use looper_infra::plugin::{PluginManager, NoopPlugin};
//!
//! let mut manager = PluginManager::new();
//! manager.register(Box::new(NoopPlugin));
//! ```
//!
//! # External plugins
//!
//! For dynamic loading via `libloading`, see [`DynamicPlugin`].

use async_trait::async_trait;
use std::fmt;
use std::sync::Arc;

/// Context passed to plugin hooks when a loop starts.
#[derive(Debug, Clone)]
pub struct LoopContext {
    /// Project name.
    pub project: String,
    /// Loop sequence number.
    pub loop_seq: i64,
    /// Loop type (planner/reviewer/worker/fixer).
    pub loop_type: String,
    /// Target issue or PR number, if any.
    pub target_number: Option<u64>,
    /// Repository full name (owner/repo).
    pub repo: Option<String>,
}

/// Context passed to plugin hooks when a loop completes.
#[derive(Debug, Clone)]
pub struct LoopResult {
    /// Final loop status (completed/failed/terminated).
    pub status: String,
    /// Total runs executed in this loop.
    pub run_count: u32,
    /// Error message, if the loop failed.
    pub error: Option<String>,
}

/// Context passed to plugin hooks when a single agent run finishes.
#[derive(Debug, Clone)]
pub struct RunContext {
    /// Project name.
    pub project: String,
    /// Loop sequence number.
    pub loop_seq: i64,
    /// Run ID.
    pub run_id: String,
    /// Step name (e.g. "write-spec", "review", "repair").
    pub step: String,
    /// Agent vendor used (claude-code/codex/etc).
    pub vendor: String,
}

/// Result of a single agent run.
#[derive(Debug, Clone)]
pub struct RunResult {
    /// Run status (completed/failed/timeout/killed).
    pub status: String,
    /// Duration in seconds.
    pub duration_secs: f64,
    /// Error message, if the run failed.
    pub error: Option<String>,
}

/// Context passed to plugin hooks on error.
#[derive(Debug, Clone)]
pub struct ErrorContext {
    /// Project name, if known.
    pub project: Option<String>,
    /// Loop sequence number, if known.
    pub loop_seq: Option<i64>,
    /// Phase where the error occurred (e.g. "agent", "git", "review").
    pub phase: String,
}

/// A plugin that hooks into the looper daemon lifecycle.
///
/// All methods have default no-op implementations so plugins only need to
/// override the hooks they care about.
#[async_trait]
pub trait Plugin: Send + Sync {
    /// Plugin name (for logging and identification).
    fn name(&self) -> &str;

    /// Plugin version.
    fn version(&self) -> &str;

    /// Called when a loop starts execution.
    async fn on_loop_start(&self, _ctx: &LoopContext) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called when a loop finishes (success or failure).
    async fn on_loop_complete(&self, _ctx: &LoopContext, _result: &LoopResult) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called when a single agent run finishes.
    async fn on_run_complete(&self, _ctx: &RunContext, _result: &RunResult) -> anyhow::Result<()> {
        Ok(())
    }

    /// Called when an error occurs during execution.
    async fn on_error(&self, _ctx: &ErrorContext, _error: &anyhow::Error) -> anyhow::Result<()> {
        Ok(())
    }
}

impl fmt::Debug for dyn Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Plugin({} v{})", self.name(), self.version())
    }
}

/// A no-op plugin useful for testing.
pub struct NoopPlugin;

#[async_trait]
impl Plugin for NoopPlugin {
    fn name(&self) -> &str {
        "noop"
    }

    fn version(&self) -> &str {
        "0.1.0"
    }
}

/// Manages registered plugins and dispatches lifecycle events.
pub struct PluginManager {
    plugins: Vec<Arc<dyn Plugin>>,
}

impl PluginManager {
    /// Create an empty plugin manager.
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    /// Register a plugin.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        tracing::info!(plugin = %plugin.name(), version = %plugin.version(), "Registering plugin");
        self.plugins.push(Arc::from(plugin));
    }

    /// Register a plugin wrapped in Arc.
    pub fn register_arc(&mut self, plugin: Arc<dyn Plugin>) {
        tracing::info!(plugin = %plugin.name(), version = %plugin.version(), "Registering plugin");
        self.plugins.push(plugin);
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether no plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Notify all plugins that a loop has started.
    pub async fn on_loop_start(&self, ctx: &LoopContext) {
        for plugin in &self.plugins {
            if let Err(e) = plugin.on_loop_start(ctx).await {
                tracing::warn!(
                    plugin = %plugin.name(),
                    error = %e,
                    "Plugin on_loop_start failed"
                );
            }
        }
    }

    /// Notify all plugins that a loop has completed.
    pub async fn on_loop_complete(&self, ctx: &LoopContext, result: &LoopResult) {
        for plugin in &self.plugins {
            if let Err(e) = plugin.on_loop_complete(ctx, result).await {
                tracing::warn!(
                    plugin = %plugin.name(),
                    error = %e,
                    "Plugin on_loop_complete failed"
                );
            }
        }
    }

    /// Notify all plugins that a run has completed.
    pub async fn on_run_complete(&self, ctx: &RunContext, result: &RunResult) {
        for plugin in &self.plugins {
            if let Err(e) = plugin.on_run_complete(ctx, result).await {
                tracing::warn!(
                    plugin = %plugin.name(),
                    error = %e,
                    "Plugin on_run_complete failed"
                );
            }
        }
    }

    /// Notify all plugins of an error.
    pub async fn on_error(&self, ctx: &ErrorContext, error: &anyhow::Error) {
        for plugin in &self.plugins {
            if let Err(e) = plugin.on_error(ctx, error).await {
                tracing::warn!(
                    plugin = %plugin.name(),
                    error = %e,
                    "Plugin on_error failed"
                );
            }
        }
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Marker trait for plugins that can be loaded dynamically.
///
/// Implement this on a struct, then use `libloading` to load a shared library
/// that exports a `fn() -> Box<dyn DynamicPlugin>` factory function.
pub trait DynamicPlugin: Plugin {
    /// The shared library filename pattern (e.g. `libloop_plugin_foo.so`).
    fn library_name() -> &'static str
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestPlugin {
        name: String,
    }

    #[async_trait]
    impl Plugin for TestPlugin {
        fn name(&self) -> &str {
            &self.name
        }

        fn version(&self) -> &str {
            "1.0.0"
        }

        async fn on_loop_start(&self, _ctx: &LoopContext) -> anyhow::Result<()> {
            Ok(())
        }

        async fn on_loop_complete(&self, _ctx: &LoopContext, _result: &LoopResult) -> anyhow::Result<()> {
            Ok(())
        }

        async fn on_run_complete(&self, _ctx: &RunContext, _result: &RunResult) -> anyhow::Result<()> {
            Ok(())
        }

        async fn on_error(&self, _ctx: &ErrorContext, _error: &anyhow::Error) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn plugin_manager_new_is_empty() {
        let manager = PluginManager::new();
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn plugin_manager_register() {
        let mut manager = PluginManager::new();
        manager.register(Box::new(NoopPlugin));
        assert_eq!(manager.len(), 1);
        assert!(!manager.is_empty());
    }

    #[test]
    fn plugin_manager_register_multiple() {
        let mut manager = PluginManager::new();
        manager.register(Box::new(NoopPlugin));
        manager.register(Box::new(TestPlugin { name: "test".to_string() }));
        assert_eq!(manager.len(), 2);
    }

    #[tokio::test]
    async fn plugin_manager_hooks_do_not_panic() {
        let mut manager = PluginManager::new();
        manager.register(Box::new(NoopPlugin));

        let ctx = LoopContext {
            project: "test".to_string(),
            loop_seq: 1,
            loop_type: "planner".to_string(),
            target_number: Some(42),
            repo: Some("owner/repo".to_string()),
        };

        manager.on_loop_start(&ctx).await;

        let loop_result = LoopResult { status: "completed".to_string(), run_count: 3, error: None };
        manager.on_loop_complete(&ctx, &loop_result).await;

        let run_ctx = RunContext {
            project: "test".to_string(),
            loop_seq: 1,
            run_id: "run-1".to_string(),
            step: "write-spec".to_string(),
            vendor: "claude-code".to_string(),
        };
        let run_result = RunResult { status: "completed".to_string(), duration_secs: 5.0, error: None };
        manager.on_run_complete(&run_ctx, &run_result).await;

        let err_ctx = ErrorContext { project: Some("test".to_string()), loop_seq: Some(1), phase: "agent".to_string() };
        let err = anyhow::anyhow!("test error");
        manager.on_error(&err_ctx, &err).await;
    }

    #[test]
    fn noop_plugin_metadata() {
        let plugin = NoopPlugin;
        assert_eq!(plugin.name(), "noop");
        assert_eq!(plugin.version(), "0.1.0");
    }

    #[test]
    fn plugin_debug_format() {
        let plugin = NoopPlugin;
        let debug = format!("{:?}", &plugin as &dyn Plugin);
        assert!(debug.contains("noop"));
        assert!(debug.contains("0.1.0"));
    }

    #[test]
    fn loop_context_clone() {
        let ctx = LoopContext {
            project: "test".to_string(),
            loop_seq: 1,
            loop_type: "planner".to_string(),
            target_number: Some(42),
            repo: Some("owner/repo".to_string()),
        };
        let cloned = ctx.clone();
        assert_eq!(cloned.project, "test");
        assert_eq!(cloned.loop_seq, 1);
    }
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A handle to an active execution that can be killed.
pub trait KillableExecution: Send + Sync {
    fn kill(&self, reason: &str) -> Result<(), String>;
}

/// Registry of currently-running agent executions, keyed by
/// `"{loop_id}\x00{run_id}\x00{execution_id}"`.
pub struct ActiveExecutionRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<dyn KillableExecution>>>>,
}

impl ActiveExecutionRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Format: `{loop_id}\x00{run_id}\x00{execution_id}`
    pub fn active_execution_key(loop_id: &str, run_id: &str, execution_id: &str) -> String {
        format!("{}\x00{}\x00{}", loop_id, run_id, execution_id)
    }

    /// Register a new execution and return an unregister handle.
    pub fn register(
        &self,
        loop_id: &str,
        run_id: &str,
        execution_id: &str,
        execution: Arc<dyn KillableExecution>,
    ) -> Box<dyn FnOnce() + Send> {
        let key = Self::active_execution_key(loop_id, run_id, execution_id);
        if let Ok(mut map) = self.inner.lock() {
            map.insert(key.clone(), execution);
        }
        let inner = Arc::clone(&self.inner);
        Box::new(move || {
            if let Ok(mut map) = inner.lock() {
                map.remove(&key);
            }
        })
    }

    /// Kill a specific execution. Returns `true` if found and killed.
    pub fn kill(&self, loop_id: &str, run_id: &str, execution_id: &str, reason: &str) -> Result<bool, String> {
        let key = Self::active_execution_key(loop_id, run_id, execution_id);
        let exec = {
            let map = self.inner.lock().map_err(|e| e.to_string())?;
            map.get(&key).map(Arc::clone)
        };
        match exec {
            Some(e) => {
                e.kill(reason)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Return the number of active executions.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Return whether there are no active executions.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the list of active execution keys (for debugging / recovery).
    pub fn list_keys(&self) -> Vec<String> {
        self.inner.lock().map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    /// Kill all active executions. Used during shutdown / recovery.
    pub fn kill_all(&self, reason: &str) -> Vec<String> {
        let keys: Vec<String> = self.list_keys();
        let mut errors = Vec::new();
        for key in &keys {
            let parts: Vec<&str> = key.split('\x00').collect();
            if parts.len() == 3 {
                if let Err(e) = self.kill(parts[0], parts[1], parts[2], reason) {
                    errors.push(format!("{}: {}", key, e));
                }
            }
        }
        errors
    }
}

impl Default for ActiveExecutionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockExecution;
    impl KillableExecution for MockExecution {
        fn kill(&self, _reason: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn test_register_and_kill() {
        let reg = ActiveExecutionRegistry::new();
        let exec = Arc::new(MockExecution);
        let unregister = reg.register("loop1", "run1", "exec1", exec);

        assert_eq!(reg.len(), 1);
        assert!(reg.kill("loop1", "run1", "exec1", "test").unwrap());

        unregister();
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn test_kill_nonexistent() {
        let reg = ActiveExecutionRegistry::new();
        assert!(!reg.kill("loop1", "run1", "no-such", "test").unwrap());
    }

    #[test]
    fn test_kill_all() {
        let reg = ActiveExecutionRegistry::new();
        reg.register("l1", "r1", "e1", Arc::new(MockExecution));
        reg.register("l2", "r2", "e2", Arc::new(MockExecution));
        assert_eq!(reg.len(), 2);

        let errors = reg.kill_all("shutdown");
        assert!(errors.is_empty());
        // kill_all doesn't remove them; just calls kill on each
        // unregister handles would need to be called separately
    }

    #[test]
    fn test_key_format() {
        let key = ActiveExecutionRegistry::active_execution_key("loop", "run", "exec");
        assert_eq!(key, "loop\x00run\x00exec");
    }
}

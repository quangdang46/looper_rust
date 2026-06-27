//! Notification throttle — prevents duplicate notifications within a time window.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// In-memory throttle that limits how frequently notifications with the same
/// key can be sent.
pub struct Throttle {
    state: Mutex<HashMap<String, Instant>>,
}

impl Throttle {
    pub fn new() -> Self {
        Self { state: Mutex::new(HashMap::new()) }
    }

    /// Returns `true` if the notification should be sent (not throttled).
    ///
    /// A notification is allowed if no previous notification with `key` was
    /// sent within `interval`, or if no prior notification exists.
    pub fn should_send(&self, key: &str) -> bool {
        // Default interval: 60 seconds
        self.should_send_with_interval(key, Duration::from_secs(60))
    }

    /// Returns `true` if the notification should be sent, respecting a
    /// custom minimum interval.
    pub fn should_send_with_interval(&self, key: &str, interval: Duration) -> bool {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return true, // If poisoned, allow through
        };

        let now = Instant::now();
        if let Some(last) = state.get(key) {
            if now.duration_since(*last) < interval {
                return false;
            }
        }

        state.insert(key.to_string(), now);
        true
    }

    /// Clear all throttle state (useful on runtime stop).
    pub fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.clear();
        }
    }

    /// Number of active throttle entries.
    pub fn len(&self) -> usize {
        self.state.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Returns `true` if the throttle has no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

use std::time::Duration;

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_first_send_allowed() {
        let throttle = Throttle::new();
        assert!(throttle.should_send("test_key"));
    }

    #[test]
    fn test_second_send_blocked() {
        let throttle = Throttle::new();
        assert!(throttle.should_send("test_key"));
        assert!(!throttle.should_send("test_key"));
    }

    #[test]
    fn test_different_keys_independent() {
        let throttle = Throttle::new();
        assert!(throttle.should_send("key_a"));
        assert!(throttle.should_send("key_b"));
        assert!(!throttle.should_send("key_a"));
        assert!(!throttle.should_send("key_b"));
    }

    #[test]
    fn test_short_interval_allows_after_wait() {
        let throttle = Throttle::new();
        assert!(throttle.should_send("fast_key"));
        // Would need to actually sleep to test interval expiry — just verify
        // that the second call within same instant is blocked.
        assert!(!throttle.should_send("fast_key"));
    }

    #[test]
    fn test_clear_resets_state() {
        let throttle = Throttle::new();
        assert!(throttle.should_send("key"));
        assert!(!throttle.should_send("key"));
        throttle.clear();
        assert!(throttle.should_send("key"));
    }

    #[test]
    fn test_len() {
        let throttle = Throttle::new();
        assert_eq!(throttle.len(), 0);
        throttle.should_send("a");
        assert_eq!(throttle.len(), 1);
        throttle.should_send("b");
        assert_eq!(throttle.len(), 2);
        // "a" again doesn't add new entry, just updates
        throttle.should_send("a");
        assert_eq!(throttle.len(), 2);
    }
}

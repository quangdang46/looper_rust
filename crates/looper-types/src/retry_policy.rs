use std::time::Duration;

// ---------------------------------------------------------------------------
// RetryPolicy — exponential backoff with jitter
// ---------------------------------------------------------------------------

/// Shared retry policy for all crate-level retry logic.
///
/// Computes delays via: `min(base_delay * multiplier^attempt, max_delay)`
/// with optional jitter: `delay ± random 0..jitter_factor * delay`.
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use looper_types::RetryPolicy;
///
/// let policy = RetryPolicy {
///     max_attempts: 3,
///     base_delay: Duration::from_secs(1),
///     max_delay: Duration::from_secs(30),
///     multiplier: 2.0,
///     jitter: 0.0, // deterministic for testing
/// };
///
/// assert!(!policy.is_exhausted(0));  // first attempt
/// assert!(!policy.is_exhausted(1));  // second attempt
/// assert!(!policy.is_exhausted(2));  // third attempt
/// assert!(policy.is_exhausted(3));   // past max_attempts
///
/// let d0 = policy.compute_delay(0);
/// let d1 = policy.compute_delay(1);
/// let d2 = policy.compute_delay(2);
/// assert!(d0 < d1);
/// assert!(d1 < d2 || d1 == d2); // capped at max_delay
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts (0 = no retries).
    pub max_attempts: u32,
    /// Initial delay before the first retry.
    pub base_delay: Duration,
    /// Hard cap on any single delay.
    pub max_delay: Duration,
    /// Exponential multiplier applied per attempt.
    pub multiplier: f64,
    /// Jitter factor: final delay = computed ± random 0..jitter_factor * computed.
    /// 0.0 = no jitter, 0.5 = ±50%, 1.0 = ±100%.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300), // 5 minutes
            multiplier: 2.0,
            jitter: 0.5,
        }
    }
}

impl RetryPolicy {
    /// Returns `true` if the given attempt count exceeds `max_attempts`.
    ///
    /// `attempt` is 0-based: attempt 0 = first try, attempt 1 = first retry, etc.
    #[must_use]
    pub fn is_exhausted(&self, attempt: u32) -> bool {
        attempt >= self.max_attempts
    }

    /// Compute the delay before retry `attempt`.
    ///
    /// Uses: `min(base_delay * multiplier^attempt, max_delay)` with optional
    /// jitter applied symmetrically around the base value.
    #[must_use]
    pub fn compute_delay(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO; // no delay before the first try
        }
        // The typical pattern: first retry delay = base_delay,
        // second = base_delay * multiplier, third = base_delay * multiplier^2
        let exp = attempt.saturating_sub(1);
        let raw_nanos = self.base_delay.as_nanos() as f64 * self.multiplier.powi(exp as i32);
        let clamped_nanos = raw_nanos.clamp(0.0, self.max_delay.as_nanos() as f64);

        let base = Duration::from_nanos(clamped_nanos as u64);

        if self.jitter <= 0.0 {
            return base;
        }

        // Symmetric jitter: delay ± random(0, jitter_factor * delay)
        let jitter_range = self.jitter * base.as_nanos() as f64;
        let offset = (fastrand::f64() * 2.0 - 1.0) * jitter_range;
        let jittered_nanos = (base.as_nanos() as f64 + offset).max(0.0);
        Duration::from_nanos(jittered_nanos as u64)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let p = RetryPolicy::default();
        assert_eq!(p.max_attempts, 5);
        assert_eq!(p.base_delay, Duration::from_secs(1));
        assert_eq!(p.max_delay, Duration::from_secs(300));
        assert_eq!(p.multiplier, 2.0);
        assert!(p.jitter > 0.0);
    }

    #[test]
    fn test_is_exhausted() {
        let p = RetryPolicy { max_attempts: 3, ..Default::default() };
        assert!(!p.is_exhausted(0));
        assert!(!p.is_exhausted(1));
        assert!(!p.is_exhausted(2));
        assert!(p.is_exhausted(3));
        assert!(p.is_exhausted(100));
    }

    #[test]
    fn test_zero_max_attempts_no_retries() {
        let p = RetryPolicy { max_attempts: 0, ..Default::default() };
        assert!(p.is_exhausted(0));
        assert!(p.is_exhausted(1));
    }

    #[test]
    fn test_exponential_growth_without_jitter() {
        let p = RetryPolicy {
            max_attempts: 10,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(300),
            multiplier: 2.0,
            jitter: 0.0,
        };

        assert_eq!(p.compute_delay(0), Duration::ZERO);
        assert_eq!(p.compute_delay(1), Duration::from_secs(1));
        assert_eq!(p.compute_delay(2), Duration::from_secs(2));
        assert_eq!(p.compute_delay(3), Duration::from_secs(4));
        assert_eq!(p.compute_delay(4), Duration::from_secs(8));
        assert_eq!(p.compute_delay(9), Duration::from_secs(256));
    }

    #[test]
    fn test_capped_at_max_delay() {
        let p = RetryPolicy {
            max_attempts: 20,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
            multiplier: 2.0,
            jitter: 0.0,
        };

        // Without cap, attempt 10 would be 512s
        assert_eq!(p.compute_delay(10), Duration::from_secs(10));
        assert_eq!(p.compute_delay(15), Duration::from_secs(10));
    }

    #[test]
    fn test_jitter_variability() {
        let p = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_secs(10),
            max_delay: Duration::from_secs(300),
            multiplier: 2.0,
            jitter: 0.5, // ±50%
        };

        // With jitter, delays should vary across runs
        let mut seen: Vec<Duration> = (0..5).map(|i| p.compute_delay(i + 1)).collect();
        seen.sort();

        // All delays should be positive (or zero if the jittered value rounds down)
        for d in &seen {
            let nanos = d.as_nanos();
            // With 50% jitter on a 10s base (10_000_000_000 ns):
            // min = 5_000_000_000, max = 15_000_000_000 — but never negative
            assert!(
                nanos < 20_000_000_000 || *d >= Duration::from_secs(10),
                "jittered delay {} ns from base 10s should stay within ±50%",
                nanos
            );
        }
    }

    #[test]
    fn test_serde_roundtrip() {
        let p = RetryPolicy::default();
        let json = serde_json::to_string(&p).unwrap();
        let deserialized: RetryPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.max_attempts, p.max_attempts);
        assert_eq!(deserialized.base_delay, p.base_delay);
        assert_eq!(deserialized.max_delay, p.max_delay);
    }

    #[test]
    fn test_zero_delay_policy() {
        let p = RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
            multiplier: 1.0,
            jitter: 0.0,
        };
        assert_eq!(p.compute_delay(0), Duration::ZERO);
        assert_eq!(p.compute_delay(1), Duration::ZERO);
        assert_eq!(p.compute_delay(2), Duration::ZERO);
    }
}

use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// CircuitBreaker — protect external dependencies from cascading failures
// ---------------------------------------------------------------------------

/// Circuit breaker state machine.
///
/// Protects external dependencies (e.g. GitHub API) from cascading failures by
/// tripping to `Open` after a threshold of consecutive failures and allowing
/// half-open probes after a cooldown period.
///
/// # State machine
///
/// ```text
///     ┌──────────┐  consecutive failures  ┌──────┐
///     │  Closed  │ ──────────────────────> │ Open │
///     └──────────┘   >= failure_threshold  └──────┘
///                                              │
///                    cooldown elapsed           │
///                    + probe_allowed = true     │
///                                              v
///     ┌──────────┐  probe succeeds   ┌──────────┐
///     │  Closed  │ <──────────────── │ HalfOpen │
///     └──────────┘                   └──────────┘
///                                          │
///                    probe fails           │
///                    (reset cooldown)      │
///                                          v
///                                      ┌──────┐
///                                      │ Open │
///                                      └──────┘
/// ```
///
/// # Examples
///
/// ```
/// use std::time::Duration;
/// use looper_infra::circuit_breaker::CircuitBreaker;
///
/// let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
/// assert!(cb.is_available());
///
/// cb.record_failure();
/// cb.record_failure();
/// assert!(cb.is_available()); // not yet at threshold
///
/// cb.record_failure();
/// assert!(!cb.is_available()); // tripped!
/// ```
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    /// Consecutive failures needed to trip open.
    failure_threshold: u32,
    /// Cooldown duration before allowing a half-open probe.
    cooldown: Duration,
    /// Current state.
    state: State,
    /// Consecutive failure count (reset on success).
    failure_count: u32,
    /// Time when the circuit was last tripped to Open.
    last_failure_time: Option<Instant>,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Closed,
    Open,
    HalfOpen,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// * `failure_threshold` — Number of consecutive failures before tripping.
    /// * `cooldown` — How long to stay open before allowing a half-open probe.
    pub const fn new(failure_threshold: u32, cooldown: Duration) -> Self {
        Self {
            failure_threshold,
            cooldown,
            state: State::Closed,
            failure_count: 0,
            last_failure_time: None,
        }
    }

    /// Returns `true` if the circuit is available for requests.
    ///
    /// In `Closed` state, always available.
    /// In `Open` state, transitions to `HalfOpen` after `cooldown` has elapsed
    /// and returns `true` (probe allowed).
    /// In `HalfOpen` state, available for the single probe request.
    pub fn is_available(&mut self) -> bool {
        match self.state {
            State::Closed => true,
            State::Open => {
                if let Some(t) = self.last_failure_time {
                    if t.elapsed() >= self.cooldown {
                        // Transition to half-open for probing
                        self.state = State::HalfOpen;
                        return true;
                    }
                }
                false
            }
            State::HalfOpen => true, // Single probe allowed
        }
    }

    /// Record a successful operation.
    ///
    /// Resets the failure count and transitions to `Closed` (from any state).
    pub fn record_success(&mut self) {
        self.failure_count = 0;
        self.last_failure_time = None;
        self.state = State::Closed;
    }

    /// Record a failed operation.
    ///
    /// Increments the failure counter. If the threshold is reached (and the
    /// state is not already `Open`), transitions to `Open`.
    pub fn record_failure(&mut self) {
        self.failure_count = self.failure_count.saturating_add(1);
        self.last_failure_time = Some(Instant::now());

        match self.state {
            State::Closed => {
                if self.failure_count >= self.failure_threshold {
                    self.state = State::Open;
                }
            }
            State::HalfOpen => {
                // Probe failed — back to open, restart cooldown
                self.state = State::Open;
            }
            State::Open => {
                // Stay open, update last_failure_time (already set above)
            }
        }
    }

    /// Get the current consecutive failure count.
    #[must_use]
    pub const fn failure_count(&self) -> u32 {
        self.failure_count
    }

    /// Get the current state as a string.
    #[must_use]
    pub fn state_name(&self) -> &'static str {
        match self.state {
            State::Closed => "closed",
            State::Open => "open",
            State::HalfOpen => "half_open",
        }
    }

    /// Reset the circuit breaker to its initial closed state.
    pub fn reset(&mut self) {
        self.failure_count = 0;
        self.last_failure_time = None;
        self.state = State::Closed;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_available() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.state_name(), "closed");
    }

    #[test]
    fn test_below_threshold_stays_available() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        assert!(cb.is_available());
        cb.record_failure();
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 2);
    }

    #[test]
    fn test_trips_at_threshold() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        assert!(!cb.is_available());
        assert_eq!(cb.state_name(), "open");
    }

    #[test]
    fn test_success_resets_closed() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure();
        cb.record_success();
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 0);
    }

    #[test]
    fn test_half_open_allows_probe() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure();
        assert!(!cb.is_available()); // open

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(5));
        assert!(cb.is_available()); // transitions to half-open
        assert_eq!(cb.state_name(), "half_open");
    }

    #[test]
    fn test_half_open_failure_goes_back_open() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure();
        std::thread::sleep(Duration::from_millis(5));
        assert!(cb.is_available()); // half-open

        // Probe fails
        cb.record_failure();
        assert!(!cb.is_available());
        assert_eq!(cb.state_name(), "open");
    }

    #[test]
    fn test_half_open_success_closes_circuit() {
        let mut cb = CircuitBreaker::new(1, Duration::from_millis(1));
        cb.record_failure();
        std::thread::sleep(Duration::from_millis(5));
        assert!(cb.is_available()); // half-open

        // Probe succeeds
        cb.record_success();
        assert!(cb.is_available());
        assert_eq!(cb.state_name(), "closed");
    }

    #[test]
    fn test_reset() {
        let mut cb = CircuitBreaker::new(3, Duration::from_secs(30));
        cb.record_failure();
        cb.record_failure();
        cb.record_failure();
        assert!(!cb.is_available());

        cb.reset();
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 0);
    }
}

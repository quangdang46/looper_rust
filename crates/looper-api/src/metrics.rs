//! Prometheus metrics collection and `/metrics` endpoint.
//!
//! Metrics are organized by dimension:
//! - **Loop metrics**: per-loop run duration, success/failure totals
//! - **Role metrics**: per-role (planner/reviewer/worker/fixer) run counts
//! - **Vendor metrics**: agent runs by vendor (claude-code/codex/opencode/cursor/custom)
//! - **Queue metrics**: queue depth, claim duration

use prometheus::{Encoder, Histogram, HistogramOpts, IntCounter, IntGauge, Opts, Registry, TextEncoder};
use std::sync::OnceLock;

/// Global metrics registry.
static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// Get or initialize the global Prometheus registry.
pub fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

// ── Loop Metrics ──────────────────────────────────────────────────────────────

/// Duration of agent runs in seconds, labeled by loop type.
pub fn run_duration_seconds(loop_type: &str) -> Histogram {
    static HIST: OnceLock<prometheus::HistogramVec> = OnceLock::new();
    let vec = HIST.get_or_init(|| {
        let opts = HistogramOpts::new("looper_run_duration_seconds", "Duration of agent runs in seconds")
            .buckets(vec![0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0]);
        prometheus::HistogramVec::new(opts, &["loop_type"]).unwrap()
    });
    registry().register(Box::new(vec.clone())).ok();
    vec.with_label_values(&[loop_type])
}

/// Total successful runs, labeled by loop type.
pub fn run_success_total(loop_type: &str) -> IntCounter {
    static COUNTER: OnceLock<prometheus::IntCounterVec> = OnceLock::new();
    let vec = COUNTER.get_or_init(|| {
        let opts = Opts::new("looper_run_success_total", "Total successful runs");
        prometheus::IntCounterVec::new(opts, &["loop_type"]).unwrap()
    });
    registry().register(Box::new(vec.clone())).ok();
    vec.with_label_values(&[loop_type])
}

/// Total failed runs, labeled by loop type.
pub fn run_failure_total(loop_type: &str) -> IntCounter {
    static COUNTER: OnceLock<prometheus::IntCounterVec> = OnceLock::new();
    let vec = COUNTER.get_or_init(|| {
        let opts = Opts::new("looper_run_failure_total", "Total failed runs");
        prometheus::IntCounterVec::new(opts, &["loop_type"]).unwrap()
    });
    registry().register(Box::new(vec.clone())).ok();
    vec.with_label_values(&[loop_type])
}

// ── Role Metrics ──────────────────────────────────────────────────────────────

/// Total runs per role (planner/reviewer/worker/fixer/coordinator).
pub fn role_runs_total(role: &str) -> IntCounter {
    static COUNTER: OnceLock<prometheus::IntCounterVec> = OnceLock::new();
    let vec = COUNTER.get_or_init(|| {
        let opts = Opts::new("looper_role_runs_total", "Total runs per role");
        prometheus::IntCounterVec::new(opts, &["role"]).unwrap()
    });
    registry().register(Box::new(vec.clone())).ok();
    vec.with_label_values(&[role])
}

// ── Vendor Metrics ────────────────────────────────────────────────────────────

/// Total agent runs per vendor (claude-code/codex/opencode/cursor/custom/hermes).
pub fn agent_runs_by_vendor_total(vendor: &str) -> IntCounter {
    static COUNTER: OnceLock<prometheus::IntCounterVec> = OnceLock::new();
    let vec = COUNTER.get_or_init(|| {
        let opts = Opts::new("looper_agent_runs_by_vendor_total", "Total agent runs by vendor");
        prometheus::IntCounterVec::new(opts, &["vendor"]).unwrap()
    });
    registry().register(Box::new(vec.clone())).ok();
    vec.with_label_values(&[vendor])
}

// ── Queue Metrics ─────────────────────────────────────────────────────────────

/// Current queue depth (number of items waiting to be claimed).
pub fn queue_depth() -> IntGauge {
    static GAUGE: OnceLock<IntGauge> = OnceLock::new();
    let gauge = GAUGE.get_or_init(|| {
        let opts = Opts::new("looper_queue_depth", "Current queue depth");
        IntGauge::with_opts(opts).unwrap()
    });
    registry().register(Box::new(gauge.clone())).ok();
    gauge.clone()
}

/// Duration of queue claim operations in seconds.
pub fn queue_claim_duration_seconds() -> Histogram {
    static HIST: OnceLock<Histogram> = OnceLock::new();
    let hist = HIST.get_or_init(|| {
        let opts = HistogramOpts::new("looper_queue_claim_duration_seconds", "Duration of queue claim operations")
            .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]);
        Histogram::with_opts(opts).unwrap()
    });
    registry().register(Box::new(hist.clone())).ok();
    hist.clone()
}

// ── Daemon Metrics ────────────────────────────────────────────────────────────

/// Daemon uptime in seconds.
pub fn daemon_uptime_seconds() -> IntGauge {
    static GAUGE: OnceLock<IntGauge> = OnceLock::new();
    let gauge = GAUGE.get_or_init(|| {
        let opts = Opts::new("looper_daemon_uptime_seconds", "Daemon uptime in seconds");
        IntGauge::with_opts(opts).unwrap()
    });
    registry().register(Box::new(gauge.clone())).ok();
    gauge.clone()
}

/// Total scheduler ticks.
pub fn scheduler_ticks_total() -> IntCounter {
    static COUNTER: OnceLock<IntCounter> = OnceLock::new();
    let counter = COUNTER.get_or_init(|| {
        let opts = Opts::new("looper_scheduler_ticks_total", "Total scheduler ticks");
        IntCounter::with_opts(opts).unwrap()
    });
    registry().register(Box::new(counter.clone())).ok();
    counter.clone()
}

/// Serialize all metrics in Prometheus text format.
pub fn encode_metrics() -> String {
    let encoder = TextEncoder::new();
    let metric_families = registry().gather();
    let mut buffer = Vec::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_registry_is_singleton() {
        let r1 = registry();
        let r2 = registry();
        // Both should point to the same registry
        assert!(std::ptr::eq(r1 as *const _, r2 as *const _));
    }

    #[test]
    fn encode_metrics_returns_valid_text() {
        // Register some metrics to ensure they appear
        let _ = run_success_total("planner");
        let _ = run_failure_total("reviewer");
        let _ = queue_depth();

        let text = encode_metrics();
        assert!(text.contains("looper_run_success_total"));
        assert!(text.contains("looper_run_failure_total"));
        assert!(text.contains("looper_queue_depth"));
        // Prometheus text format has HELP and TYPE lines
        assert!(text.contains("# HELP"));
        assert!(text.contains("# TYPE"));
    }

    #[test]
    fn counter_increments() {
        let counter = run_success_total("worker");
        let before = counter.get();
        counter.inc();
        assert_eq!(counter.get(), before + 1);
    }

    #[test]
    fn gauge_set() {
        let gauge = queue_depth();
        gauge.set(42);
        assert_eq!(gauge.get(), 42);
        gauge.set(0);
        assert_eq!(gauge.get(), 0);
    }

    #[test]
    fn histogram_records_observation() {
        let hist = run_duration_seconds("fixer");
        hist.observe(1.5);
        hist.observe(3.0);
        assert_eq!(hist.get_sample_count(), 2);
        assert!((hist.get_sample_sum() - 4.5).abs() < 0.001);
    }

    #[test]
    fn role_metrics_work() {
        let counter = role_runs_total("planner");
        counter.inc();
        counter.inc_by(5);
        assert_eq!(counter.get(), 6);
    }

    #[test]
    fn vendor_metrics_work() {
        let counter = agent_runs_by_vendor_total("claude-code");
        counter.inc();
        assert_eq!(counter.get(), 1);
    }

    #[test]
    fn daemon_metrics_work() {
        let uptime = daemon_uptime_seconds();
        uptime.set(120);
        assert_eq!(uptime.get(), 120);

        let ticks = scheduler_ticks_total();
        ticks.inc();
        assert_eq!(ticks.get(), 1);
    }
}

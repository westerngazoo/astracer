//! # lcw-telemetry
//!
//! Native observability, kept intentionally *lightweight* (Manifesto
//! Principle III): structured logging via `tracing`, plus a tiny in-process
//! counter registry for perf/invariant signals. **No network exporters** are
//! wired up by default, so instrumentation never adds network latency.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub use tracing::{debug, error, info, trace, warn};

/// Initialize the global tracing subscriber. Idempotent: calling twice is a
/// no-op (handy for tests). `level` is a directive like `"info"` or
/// `"lcw_engine=debug,warn"`. The `RUST_LOG` env var, if set, wins.
pub fn init(level: &str) {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(level))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    // `try_init` returns Err if a subscriber is already set; that's fine.
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------------------------------------------------------------------------
// Counters
// ---------------------------------------------------------------------------

fn registry() -> &'static Mutex<BTreeMap<&'static str, &'static AtomicU64>> {
    static REG: OnceLock<Mutex<BTreeMap<&'static str, &'static AtomicU64>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Add `n` to a named counter, creating it on first use.
pub fn count(name: &'static str, n: u64) {
    let mut reg = registry().lock().expect("counter registry poisoned");
    let slot = reg
        .entry(name)
        .or_insert_with(|| Box::leak(Box::new(AtomicU64::new(0))));
    slot.fetch_add(n, Ordering::Relaxed);
}

/// Increment a named counter by one.
pub fn incr(name: &'static str) {
    count(name, 1);
}

/// Snapshot all counters as `(name, value)` pairs, sorted by name.
pub fn snapshot() -> Vec<(&'static str, u64)> {
    let reg = registry().lock().expect("counter registry poisoned");
    reg.iter()
        .map(|(k, v)| (*k, v.load(Ordering::Relaxed)))
        .collect()
}

/// Reset all counters to zero (mostly for tests).
pub fn reset() {
    let reg = registry().lock().expect("counter registry poisoned");
    for v in reg.values() {
        v.store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Perf spans
// ---------------------------------------------------------------------------

/// RAII timer: logs the elapsed wall-clock time at `debug` level when dropped.
/// Zero cost when the `debug` level is filtered out apart from an `Instant`.
#[must_use = "hold the guard for the duration you want to measure"]
pub struct PerfGuard {
    label: &'static str,
    start: Instant,
}

impl PerfGuard {
    pub fn new(label: &'static str) -> Self {
        PerfGuard {
            label,
            start: Instant::now(),
        }
    }

    /// Milliseconds elapsed so far.
    pub fn elapsed_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

impl Drop for PerfGuard {
    fn drop(&mut self) {
        debug!(target: "lcw::perf", label = self.label, elapsed_ms = self.elapsed_ms());
    }
}

/// Time a closure, returning its result. Logs elapsed at `debug`.
pub fn timed<T>(label: &'static str, f: impl FnOnce() -> T) -> T {
    let _g = PerfGuard::new(label);
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate() {
        reset();
        incr("t.a");
        count("t.a", 4);
        let snap = snapshot();
        let a = snap.iter().find(|(k, _)| *k == "t.a").unwrap().1;
        assert_eq!(a, 5);
    }
}

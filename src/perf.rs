//! Optional performance counters for hot-path attribution.
//!
//! Enabled with Cargo feature `perf_trace`. When disabled, [`timed`] is an
//! inlinable pass-through with no atomic traffic.
//! Timers record completed closure durations; nested spans are not subtracted.
//! Their sum and shares are diagnostic, not an exclusive wall-time partition.
//! `ApplyIntegrate` counts committed wire operations, including deferred effects,
//! rather than expanded document nodes or timer invocations. Validation records
//! its actual header/admission/drain scopes; decoding records decoded envelopes.

use std::time::Instant;

/// Named hot-path spans measured when `perf_trace` is enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum Span {
    BlockLookup = 0,
    UnitExpand = 1,
    SequenceApply = 2,
    EnvelopeEncode = 3,
    SyncLogAppend = 4,
    ApplyValidate = 5,
    ApplyDecode = 6,
    ApplyIntegrate = 7,
    /// Full incremental ordering oracle, including metadata construction and comparison.
    SequenceOrderOracle = 8,
}

const SPAN_COUNT: usize = 9;

/// Snapshot of cumulative counts and nanoseconds per [`Span`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PerfSnapshot {
    pub counts: [u64; SPAN_COUNT],
    pub nanos: [u64; SPAN_COUNT],
}

impl PerfSnapshot {
    #[must_use]
    pub fn count(&self, span: Span) -> u64 {
        self.counts[span as usize]
    }

    #[must_use]
    pub fn nanos(&self, span: Span) -> u64 {
        self.nanos[span as usize]
    }

    /// Share of total recorded nanos for `span` (0.0–1.0). Zero if empty.
    #[must_use]
    pub fn share(&self, span: Span) -> f64 {
        let total: u64 = self.nanos.iter().sum();
        if total == 0 {
            0.0
        } else {
            self.nanos[span as usize] as f64 / total as f64
        }
    }

    #[must_use]
    pub fn total_nanos(&self) -> u64 {
        self.nanos.iter().sum()
    }
}

#[cfg(feature = "perf_trace")]
mod active {
    use super::{PerfSnapshot, SPAN_COUNT, Span};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Instant;

    static ENABLED: AtomicBool = AtomicBool::new(false);
    static COUNTS: [AtomicU64; SPAN_COUNT] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];
    static NANOS: [AtomicU64; SPAN_COUNT] = [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ];

    pub fn set_enabled(enabled: bool) {
        ENABLED.store(enabled, Ordering::SeqCst);
    }

    pub fn is_enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    pub fn reset() {
        for i in 0..SPAN_COUNT {
            COUNTS[i].store(0, Ordering::SeqCst);
            NANOS[i].store(0, Ordering::SeqCst);
        }
    }

    pub fn snapshot() -> PerfSnapshot {
        let mut out = PerfSnapshot::default();
        for i in 0..SPAN_COUNT {
            out.counts[i] = COUNTS[i].load(Ordering::SeqCst);
            out.nanos[i] = NANOS[i].load(Ordering::SeqCst);
        }
        out
    }

    fn record_count(span: Span, nanos: u64, count: u64) {
        if !is_enabled() {
            return;
        }
        let i = span as usize;
        COUNTS[i].fetch_add(count, Ordering::Relaxed);
        NANOS[i].fetch_add(nanos, Ordering::Relaxed);
    }

    pub fn record(span: Span, nanos: u64) {
        record_count(span, nanos, 1);
    }

    #[inline]
    pub fn timed_count<T>(span: Span, count: usize, f: impl FnOnce() -> T) -> T {
        if !is_enabled() {
            return f();
        }
        let start = Instant::now();
        let value = f();
        record_count(span, start.elapsed().as_nanos() as u64, count as u64);
        value
    }
}

#[cfg(not(feature = "perf_trace"))]
mod inactive {
    use super::{PerfSnapshot, Span};

    #[inline]
    pub fn set_enabled(_enabled: bool) {}

    #[inline]
    pub fn is_enabled() -> bool {
        false
    }

    #[inline]
    pub fn reset() {}

    #[inline]
    pub fn snapshot() -> PerfSnapshot {
        PerfSnapshot::default()
    }

    #[inline]
    pub fn record(_span: Span, _nanos: u64) {}

    #[inline]
    pub fn timed_count<T>(_span: Span, _count: usize, f: impl FnOnce() -> T) -> T {
        f()
    }
}

#[cfg(feature = "perf_trace")]
use active as backend;
#[cfg(not(feature = "perf_trace"))]
use inactive as backend;

/// Enable or disable recording (no-op without `perf_trace`).
pub fn set_enabled(enabled: bool) {
    backend::set_enabled(enabled);
}

/// Whether recording is currently enabled.
#[must_use]
pub fn is_enabled() -> bool {
    backend::is_enabled()
}

/// Clear all counters.
pub fn reset() {
    backend::reset();
}

/// Read cumulative counters.
#[must_use]
pub fn snapshot() -> PerfSnapshot {
    backend::snapshot()
}

/// Record a completed span duration in nanoseconds.
pub fn record(span: Span, nanos: u64) {
    backend::record(span, nanos);
}

/// Time a closure under `span` when recording is enabled.
#[inline]
pub fn timed<T>(span: Span, f: impl FnOnce() -> T) -> T {
    timed_count(span, 1, f)
}

/// Attribute one real publication to its committed wire operations. Duration is
/// measured once, using the same completed-closure timer as `timed`.
#[inline]
pub(crate) fn timed_count<T>(span: Span, committed_operations: usize, f: impl FnOnce() -> T) -> T {
    backend::timed_count(span, committed_operations, f)
}

/// Convenience: reset, enable, run `f`, disable, return snapshot + result.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, PerfSnapshot) {
    reset();
    set_enabled(true);
    let value = f();
    set_enabled(false);
    (value, snapshot())
}

/// Wall-time helper for one-off attribution outside the feature (always works).
#[must_use]
pub fn wall_nanos(f: impl FnOnce()) -> u64 {
    let start = Instant::now();
    f();
    start.elapsed().as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// Global counters are process-wide; serialize tests that touch them.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_counters() -> MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn disabled_path_is_zero_snapshot() {
        let _guard = lock_counters();
        set_enabled(false);
        reset();
        let _ = timed(Span::BlockLookup, || 1 + 1);
        let snap = snapshot();
        assert_eq!(snap.total_nanos(), 0);
        assert_eq!(snap.count(Span::BlockLookup), 0);
    }

    #[cfg(feature = "perf_trace")]
    #[test]
    fn enabled_path_records_counts_and_nanos() {
        let _guard = lock_counters();
        set_enabled(false);
        reset();
        set_enabled(true);
        timed(Span::UnitExpand, || {
            std::thread::sleep(std::time::Duration::from_micros(50));
        });
        timed(Span::EnvelopeEncode, || {});
        set_enabled(false);
        let snap = snapshot();
        assert_eq!(snap.count(Span::UnitExpand), 1);
        assert_eq!(snap.count(Span::EnvelopeEncode), 1);
        assert!(snap.nanos(Span::UnitExpand) >= 10_000);
        assert!(snap.share(Span::UnitExpand) > 0.0);
        assert!(snap.share(Span::UnitExpand) <= 1.0);
        reset();
        assert_eq!(snapshot().total_nanos(), 0);
    }

    #[cfg(feature = "perf_trace")]
    #[test]
    fn measure_wrapper_enables_and_disables() {
        let _guard = lock_counters();
        set_enabled(false);
        let (value, snap) = measure(|| timed(Span::SyncLogAppend, || 42));
        assert_eq!(value, 42);
        assert_eq!(snap.count(Span::SyncLogAppend), 1);
        assert!(!is_enabled());
    }

    #[cfg(not(feature = "perf_trace"))]
    #[test]
    fn measure_without_feature_stays_empty() {
        let _guard = lock_counters();
        let (value, snap) = measure(|| timed(Span::SyncLogAppend, || 7));
        assert_eq!(value, 7);
        assert_eq!(snap.total_nanos(), 0);
        assert!(!is_enabled());
    }

    #[test]
    fn wall_nanos_always_measures() {
        let n = wall_nanos(|| {
            std::thread::sleep(std::time::Duration::from_micros(20));
        });
        assert!(n >= 5_000);
    }
}

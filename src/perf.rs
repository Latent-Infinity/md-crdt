//! Optional performance counters for hot-path attribution.
//!
//! Enabled with Cargo feature `perf_trace`. When disabled, [`timed`] is an
//! always-inlined pass-through with no atomic traffic.

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
}

const SPAN_COUNT: usize = 8;

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

    pub fn record(span: Span, nanos: u64) {
        if !is_enabled() {
            return;
        }
        let i = span as usize;
        COUNTS[i].fetch_add(1, Ordering::Relaxed);
        NANOS[i].fetch_add(nanos, Ordering::Relaxed);
    }

    #[inline]
    pub fn timed<T>(span: Span, f: impl FnOnce() -> T) -> T {
        if !is_enabled() {
            return f();
        }
        let start = Instant::now();
        let value = f();
        record(span, start.elapsed().as_nanos() as u64);
        value
    }
}

#[cfg(not(feature = "perf_trace"))]
mod inactive {
    use super::{PerfSnapshot, Span};

    #[inline(always)]
    pub fn set_enabled(_enabled: bool) {}

    #[inline(always)]
    pub fn is_enabled() -> bool {
        false
    }

    #[inline(always)]
    pub fn reset() {}

    #[inline(always)]
    pub fn snapshot() -> PerfSnapshot {
        PerfSnapshot::default()
    }

    #[inline(always)]
    pub fn record(_span: Span, _nanos: u64) {}

    #[inline(always)]
    pub fn timed<T>(_span: Span, f: impl FnOnce() -> T) -> T {
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
    backend::timed(span, f)
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

    #[cfg(feature = "perf_trace")]
    #[test]
    fn insert_text_records_local_hot_path_spans() {
        use crate::doc::block_id_from_op;
        use crate::session::CollaborativeDocument;

        let _guard = lock_counters();
        let mut session = CollaborativeDocument::new(1);
        let elem = session
            .insert_paragraph(None, &"x".repeat(64))
            .expect("seed");
        let block_id = block_id_from_op(elem);

        let (_, snap) = measure(|| session.insert_text(block_id, 32, "y").expect("insert"));

        assert_eq!(snap.count(Span::BlockLookup), 1);
        assert_eq!(snap.count(Span::UnitExpand), 1);
        assert_eq!(snap.count(Span::EnvelopeEncode), 1);
        assert_eq!(snap.count(Span::SequenceApply), 1);
        assert_eq!(snap.count(Span::SyncLogAppend), 1);
        assert!(snap.total_nanos() > 0);
        // Local insert does not touch remote apply spans.
        assert_eq!(snap.count(Span::ApplyValidate), 0);
        assert_eq!(snap.count(Span::ApplyDecode), 0);
        assert_eq!(snap.count(Span::ApplyIntegrate), 0);
    }

    #[cfg(feature = "perf_trace")]
    #[test]
    fn apply_remote_records_validate_decode_integrate() {
        use crate::core::StateVector;
        use crate::session::CollaborativeDocument;
        use crate::sync::ValidationLimits;

        let _guard = lock_counters();
        let mut source = CollaborativeDocument::new(1);
        source.insert_paragraph(None, "hello").expect("seed");
        let full = source
            .encode_changes_since(&StateVector::default())
            .expect("encode");

        let mut peer = CollaborativeDocument::new(2);
        let (_, snap) = measure(|| {
            peer.apply_remote(full, &ValidationLimits::default())
                .expect("apply")
        });

        assert!(snap.count(Span::ApplyValidate) >= 1);
        assert!(snap.count(Span::ApplyDecode) >= 1);
        assert!(snap.count(Span::ApplyIntegrate) >= 1);
        assert!(snap.total_nanos() > 0);
    }

    #[test]
    fn wall_nanos_always_measures() {
        let n = wall_nanos(|| {
            std::thread::sleep(std::time::Duration::from_micros(20));
        });
        assert!(n >= 5_000);
    }
}

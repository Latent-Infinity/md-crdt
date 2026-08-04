//! Setup-outside-timer helpers for batched Criterion routines.
//!
//! Criterion wiring lands later. These helpers encode the batch policy so unit
//! tests can prove restore/decode setup never runs inside the measured closure
//! and that measured outputs are returned for drop outside the timer.

use crate::scenario::BatchPolicy;
use std::cell::Cell;

/// Default batch policy for destructive / allocate-heavy comparison cases.
pub const DEFAULT_DESTRUCTIVE_BATCH: BatchPolicy = BatchPolicy::LargeInput;

/// One batched iteration: `setup` runs first, then `measure` on the prepared value.
///
/// Criterion will map this to `iter_batched_ref` with `BatchSize::LargeInput`.
/// The setup value is dropped **after** `measure` returns so destructor cost is
/// outside the measured routine (matching Criterion's batched_ref contract).
#[inline]
pub fn run_batched_iteration<I, O, S, M>(setup: S, measure: M) -> O
where
    S: FnOnce() -> I,
    M: FnOnce(&mut I) -> O,
{
    let mut input = setup();
    let output = measure(&mut input);
    drop(input);
    output
}

/// Same as [`run_batched_iteration`] but records call order for tests.
pub fn run_batched_iteration_traced<I, O, S, M>(setup: S, measure: M, trace: &CallTrace) -> O
where
    S: FnOnce() -> I,
    M: FnOnce(&mut I) -> O,
{
    trace.record_setup();
    let mut input = setup();
    trace.record_measure_enter();
    let output = measure(&mut input);
    trace.record_measure_exit();
    drop(input);
    trace.record_input_dropped();
    output
}

/// Call-order spy used by harness unit tests.
#[derive(Debug, Default)]
pub struct CallTrace {
    pub setup_count: Cell<usize>,
    pub measure_enter_count: Cell<usize>,
    pub measure_exit_count: Cell<usize>,
    pub input_drop_count: Cell<usize>,
    /// True if setup was ever observed after measure entered (violation).
    pub setup_after_measure: Cell<bool>,
    /// True if input drop was observed while measure is active (violation).
    pub drop_during_measure: Cell<bool>,
    measure_depth: Cell<usize>,
}

impl CallTrace {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn record_setup(&self) {
        if self.measure_depth.get() > 0 {
            self.setup_after_measure.set(true);
        }
        self.setup_count
            .set(self.setup_count.get().saturating_add(1));
    }

    fn record_measure_enter(&self) {
        self.measure_enter_count
            .set(self.measure_enter_count.get().saturating_add(1));
        self.measure_depth
            .set(self.measure_depth.get().saturating_add(1));
    }

    fn record_measure_exit(&self) {
        self.measure_exit_count
            .set(self.measure_exit_count.get().saturating_add(1));
        self.measure_depth
            .set(self.measure_depth.get().saturating_sub(1));
    }

    fn record_input_dropped(&self) {
        if self.measure_depth.get() > 0 {
            self.drop_during_measure.set(true);
        }
        self.input_drop_count
            .set(self.input_drop_count.get().saturating_add(1));
    }

    /// Assert a single legal setup → measure → drop cycle.
    pub fn assert_single_legal_cycle(&self) {
        assert_eq!(self.setup_count.get(), 1);
        assert_eq!(self.measure_enter_count.get(), 1);
        assert_eq!(self.measure_exit_count.get(), 1);
        assert_eq!(self.input_drop_count.get(), 1);
        assert!(!self.setup_after_measure.get());
        assert!(!self.drop_during_measure.get());
        assert_eq!(self.measure_depth.get(), 0);
    }
}

/// Tracks whether a value was dropped (for isolation tests).
#[derive(Debug)]
pub struct DropProbe {
    pub dropped: Cell<bool>,
}

impl DropProbe {
    #[must_use]
    pub fn new() -> Self {
        Self {
            dropped: Cell::new(false),
        }
    }
}

impl Default for DropProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for DropProbe {
    fn drop(&mut self) {
        self.dropped.set(true);
    }
}

/// Input that owns a [`DropProbe`] so tests can observe destructor timing.
pub struct TracedInput {
    pub label: String,
    pub probe: DropProbe,
    /// Mutable payload touched by measure.
    pub value: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    #[test]
    fn default_batch_policy_is_large_input() {
        assert_eq!(DEFAULT_DESTRUCTIVE_BATCH, BatchPolicy::LargeInput);
    }

    #[test]
    fn setup_runs_before_measure_and_input_drops_after() {
        let trace = CallTrace::new();
        let probe = Rc::new(DropProbe::new());
        let probe_for_setup = Rc::clone(&probe);

        let out = run_batched_iteration_traced(
            || {
                assert!(!probe_for_setup.dropped.get());
                TracedInput {
                    label: "seed".into(),
                    probe: DropProbe::new(),
                    value: 7,
                }
            },
            |input| {
                // Setup already finished; input must still be live.
                assert!(!input.probe.dropped.get());
                input.value = input.value.saturating_add(1);
                input.value
            },
            &trace,
        );

        assert_eq!(out, 8);
        trace.assert_single_legal_cycle();
        // setup's local Rc probe is not the input probe; input DropProbe dropped after measure.
    }

    #[test]
    fn measured_output_is_returned_for_caller_drop() {
        let output_probe = Rc::new(DropProbe::new());
        let out = run_batched_iteration(
            || 1usize,
            |n: &mut usize| {
                *n = n.saturating_add(1);
                Rc::clone(&output_probe)
            },
        );
        assert!(!output_probe.dropped.get());
        // `output_probe` + `out`
        assert_eq!(Rc::strong_count(&out), 2);
        drop(out);
        assert_eq!(Rc::strong_count(&output_probe), 1);
    }

    #[test]
    fn multiple_iterations_keep_setup_outside_measure() {
        let trace = CallTrace::new();
        for i in 0..5usize {
            let got = run_batched_iteration_traced(
                || i * 10,
                |v: &mut usize| {
                    *v = v.saturating_add(1);
                    *v
                },
                &trace,
            );
            assert_eq!(got, i * 10 + 1);
        }
        assert_eq!(trace.setup_count.get(), 5);
        assert_eq!(trace.measure_enter_count.get(), 5);
        assert_eq!(trace.measure_exit_count.get(), 5);
        assert_eq!(trace.input_drop_count.get(), 5);
        assert!(!trace.setup_after_measure.get());
        assert!(!trace.drop_during_measure.get());
    }

    #[test]
    fn destructive_restore_pattern_clones_seed_in_setup_only() {
        // Models Criterion LargeInput: immutable seed lives outside; each
        // iteration restores a working copy in setup, mutates in measure.
        #[derive(Clone)]
        struct Seed(String);

        let seed = Seed("xxxx".into());
        let mut results = Vec::new();
        for _ in 0..3 {
            let out = run_batched_iteration(
                || seed.clone(),
                |doc| {
                    doc.0.insert(2, 'y');
                    doc.0.clone()
                },
            );
            results.push(out);
        }
        assert_eq!(seed.0, "xxxx");
        assert!(results.iter().all(|r| r == "xxyxx"));
    }
}

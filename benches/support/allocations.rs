//! System-allocation counters shared by isolated benchmark executables.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

struct ProbeAllocator;
static ENABLED: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_DELTA: AtomicIsize = AtomicIsize::new(0);
static PEAK_DELTA: AtomicIsize = AtomicIsize::new(0);
fn live_change(bytes: isize) {
    if ENABLED.load(Ordering::Relaxed) {
        let live = LIVE_DELTA.fetch_add(bytes, Ordering::Relaxed) + bytes;
        PEAK_DELTA.fetch_max(live, Ordering::Relaxed);
    }
}
fn record(bytes: usize) {
    if ENABLED.load(Ordering::Relaxed) {
        CALLS.fetch_add(1, Ordering::Relaxed);
        live_change(bytes as isize);
        BYTES.fetch_add(bytes, Ordering::Relaxed);
    }
}
// SAFETY: all operations delegate unchanged pointers/layouts to System; counters
// observe requested bytes and never participate in allocator ownership.
unsafe impl GlobalAlloc for ProbeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: forward the caller's valid allocation layout unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        // SAFETY: forward the caller's valid allocation layout unchanged.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        live_change(-(layout.size() as isize));
        // SAFETY: allocation and layout are owned by the delegated System allocator.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        live_change(-(layout.size() as isize));
        record(size);
        // SAFETY: forward the original allocation/layout and requested size unchanged.
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: ProbeAllocator = ProbeAllocator;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct AllocationStats {
    pub calls: usize,
    pub requested_bytes: usize,
    pub peak_live_bytes_delta: isize,
    pub retained_bytes_delta: isize,
}

/// Isolated-process observation only; prepare inputs before entering this scope.
/// Requested traffic counts every realloc's full requested size, not net growth.
pub fn capture<T>(operation: impl FnOnce() -> T) -> (T, AllocationStats) {
    let (result, captured) = capture_with_input(|| (), |()| operation());
    let stats = AllocationStats {
        calls: captured.operation_calls,
        requested_bytes: captured.operation_requested_bytes,
        peak_live_bytes_delta: captured
            .peak_tracked_live_bytes
            .saturating_sub(captured.input_live_bytes),
        retained_bytes_delta: captured
            .retained_tracked_live_bytes
            .saturating_sub(captured.input_live_bytes),
    };
    (result, stats)
}

/// Count requested allocation traffic and the live-byte delta during one operation.
/// Inputs must be prepared before calling; returned values are dropped after counting.
/// The live delta can undercount retained memory if the operation frees preexisting
/// allocations. This is not an absolute heap/RSS measurement or a realloc RSS peak.
pub fn allocations<T>(name: &str, operation: impl FnOnce() -> T) {
    let (result, stats) = capture(operation);
    println!(
        "allocation_probe name={name} calls={} requested_bytes={} peak_live_bytes_delta={}",
        stats.calls, stats.requested_bytes, stats.peak_live_bytes_delta
    );
    black_box(result);
}

/// Tracked owned-input plus operation lifetime, not whole-process heap or RSS.
#[derive(Debug)]
pub struct InputMemoryStats {
    pub input_live_bytes: isize,
    pub operation_calls: usize,
    pub operation_requested_bytes: usize,
    pub peak_tracked_live_bytes: isize,
    pub retained_tracked_live_bytes: isize,
}

pub fn capture_with_input<I, T>(
    prepare: impl FnOnce() -> I,
    operation: impl FnOnce(I) -> T,
) -> (T, InputMemoryStats) {
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    LIVE_DELTA.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(0, Ordering::Relaxed);
    ENABLED.store(true, Ordering::Relaxed);
    let input = prepare();
    let input_live_bytes = LIVE_DELTA.load(Ordering::Relaxed);
    CALLS.store(0, Ordering::Relaxed);
    BYTES.store(0, Ordering::Relaxed);
    PEAK_DELTA.store(input_live_bytes, Ordering::Relaxed);
    let result = operation(input);
    ENABLED.store(false, Ordering::Relaxed);
    let stats = InputMemoryStats {
        input_live_bytes,
        operation_calls: CALLS.load(Ordering::Relaxed),
        operation_requested_bytes: BYTES.load(Ordering::Relaxed),
        peak_tracked_live_bytes: PEAK_DELTA.load(Ordering::Relaxed),
        retained_tracked_live_bytes: LIVE_DELTA.load(Ordering::Relaxed),
    };
    (result, stats)
}

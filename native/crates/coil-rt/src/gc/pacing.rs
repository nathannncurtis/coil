//! Pacing (section 9.5): a full collection starts when the bytes allocated
//! on the GC heap since the last one exceed `max(8 MiB, live_bytes)` —
//! Go's GOGC=100 rule — with the private-bytes backstop for memory held
//! only through compiled-code objects: every 64 MiB of GC-heap allocation
//! the runtime samples the process's private bytes and collects when they
//! have grown by more than the live GC heap since the last collection.
//! `gc.disable()` stops the automatic pacing alone.

use std::sync::Mutex;

/// The floor of the growth threshold: collections never wait for more
/// than 8 MiB of allocation.
pub const MIN_THRESHOLD: u64 = 8 * 1024 * 1024;
/// How often the private-bytes backstop samples, in GC-heap bytes
/// allocated since the last sample.
pub const BACKSTOP_SAMPLE_BYTES: u64 = 64 * 1024 * 1024;

struct Pacing {
    /// Total GC-heap object bytes handed out since the process started.
    allocated_total: u64,
    /// The same, at the last collection: the growth the threshold reads.
    allocated_at_last: u64,
    /// The live GC-heap bytes the last collection measured.
    live_bytes: u64,
    /// `allocated_total` when the backstop last sampled.
    backstop_at: u64,
    /// Private bytes at the last collection, when a sample succeeded.
    private_at_last: Option<u64>,
    /// Collections run so far, pacing-driven and explicit.
    collections: u64,
}

fn pacing() -> &'static Mutex<Pacing> {
    static PACING: std::sync::OnceLock<Mutex<Pacing>> = std::sync::OnceLock::new();
    PACING.get_or_init(|| {
        Mutex::new(Pacing {
            allocated_total: 0,
            allocated_at_last: 0,
            live_bytes: 0,
            backstop_at: 0,
            private_at_last: None,
            collections: 0,
        })
    })
}

/// Record `bytes` of GC-heap allocation and run a collection when pacing
/// calls for one. Called at the entry of the object allocation paths,
/// before the block exists, so a collection it starts cannot sweep the
/// object being allocated; the collector's own allocations never
/// re-enter (the collecting flag stops the trigger before it starts).
pub(crate) fn on_gc_heap_request(bytes: usize) {
    let mut pacing = pacing().lock().expect("pacing state poisoned");
    pacing.allocated_total = pacing.allocated_total.saturating_add(bytes as u64);
    if !crate::gc::is_enabled() || crate::gc::is_collecting() {
        return;
    }
    let growth = pacing.allocated_total - pacing.allocated_at_last;
    if growth > MIN_THRESHOLD.max(pacing.live_bytes) {
        drop(pacing);
        crate::gc::collect();
        return;
    }
    if pacing.allocated_total - pacing.backstop_at >= BACKSTOP_SAMPLE_BYTES {
        pacing.backstop_at = pacing.allocated_total;
        let live = pacing.live_bytes;
        if let (Some(now), Some(last)) = (private_bytes(), pacing.private_at_last)
            && now > last
            && now - last > live
        {
            drop(pacing);
            crate::gc::collect();
        }
    }
}

/// The pacing decision, split out pure: collect when `growth` exceeds the
/// threshold `max(8 MiB, live_bytes)`.
pub fn threshold_exceeded(growth: u64, live_bytes: u64) -> bool {
    growth > MIN_THRESHOLD.max(live_bytes)
}

/// The backstop's decision: collect when private bytes grew by more than
/// the live heap since the last collection.
pub fn backstop_exceeded(grown: u64, live_bytes: u64) -> bool {
    grown > live_bytes
}

/// The last collection's outcome, for the next threshold and the backstop.
pub(crate) fn record_collection(live_bytes: u64) {
    let mut pacing = pacing().lock().expect("pacing state poisoned");
    pacing.allocated_at_last = pacing.allocated_total;
    pacing.live_bytes = live_bytes;
    pacing.private_at_last = private_bytes().or(pacing.private_at_last);
    pacing.collections += 1;
}

/// Collections run so far.
pub fn collections() -> u64 {
    pacing().lock().expect("pacing state poisoned").collections
}

/// The live GC-heap bytes the last collection measured.
pub fn live_bytes() -> u64 {
    pacing().lock().expect("pacing state poisoned").live_bytes
}

/// The process's private bytes: `PROCESS_MEMORY_COUNTERS_EX`'s
/// `PrivateUsage` on Windows; the resident set on the Unix hosts, the
/// closest portable stand-in. `None` when the host cannot report it.
pub fn private_bytes() -> Option<u64> {
    imp::private_bytes()
}

#[cfg(target_os = "windows")]
mod imp {
    use core::ffi::c_void;

    #[repr(C)]
    struct ProcessMemoryCountersEx {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut c_void,
            counters: *mut ProcessMemoryCountersEx,
            cb: u32,
        ) -> i32;
    }

    pub fn private_bytes() -> Option<u64> {
        let mut counters = ProcessMemoryCountersEx {
            cb: core::mem::size_of::<ProcessMemoryCountersEx>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_non_paged_pool_usage: 0,
            quota_non_paged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
            private_usage: 0,
        };
        // SAFETY: the struct is this function's own and sized for the
        // call; `GetCurrentProcess` returns a constant pseudo-handle.
        let ok = unsafe {
            K32GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                core::mem::size_of::<ProcessMemoryCountersEx>() as u32,
            )
        };
        (ok != 0).then_some(counters.private_usage as u64)
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    pub fn private_bytes() -> Option<u64> {
        // statm's second field is the resident set in pages.
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        Some(pages * page_size())
    }

    fn page_size() -> u64 {
        // 4 KiB on every host this runtime builds for today; `sysconf`
        // needs libc, which the runtime does not link.
        4 * 1024
    }
}

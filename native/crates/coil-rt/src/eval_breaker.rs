//! The per-thread eval-breaker word: safepoint polls and GIL drop
//! requests (spec sections 9.4 and 10.3).
//!
//! The word lives in `ThreadState` where compiled code reaches it through
//! the TLS slot; the GIL sets the drop-request bit in the *holder's* word
//! and clears it once a waiter acquired, exactly as CPython 3.12's
//! `ceval_gil.c` does. The remaining 3.12 bits (GC scheduled, signals
//! pending, calls to do, async exception) arrive with the subsystems that
//! set them.

use core::sync::atomic::Ordering;

use crate::tstate::ThreadState;

/// CPython 3.12's `_PY_GIL_DROP_REQUEST_BIT` (pycore_ceval.h): the GIL asks
/// the attached thread to drop it at the next breaker check.
pub const DROP_REQUEST_BIT: i64 = 1;

/// Set the drop-request bit in `tstate`'s eval-breaker word.
///
/// # Safety
///
/// `tstate` must be a live thread state; the atomic write is the only
/// cross-thread access to the word.
pub unsafe fn set_drop_request(tstate: *mut ThreadState) {
    // SAFETY: the caller's contract; the GIL passes its holder pointer.
    unsafe {
        (*tstate)
            .eval_breaker
            .fetch_or(DROP_REQUEST_BIT, Ordering::SeqCst)
    };
}

/// Clear the drop-request bit in `tstate`'s eval-breaker word.
///
/// # Safety
///
/// As `set_drop_request`.
pub unsafe fn clear_drop_request(tstate: *mut ThreadState) {
    // SAFETY: see `set_drop_request`.
    unsafe {
        (*tstate)
            .eval_breaker
            .fetch_and(!DROP_REQUEST_BIT, Ordering::SeqCst)
    };
}

/// Whether any bit of `tstate`'s eval-breaker word is set: compiled code's
/// poll at back-edges.
///
/// # Safety
///
/// `tstate` must be a live thread state.
pub unsafe fn pending(tstate: *mut ThreadState) -> bool {
    // SAFETY: an atomic read of the caller's thread state.
    unsafe { (*tstate).eval_breaker.load(Ordering::SeqCst) != 0 }
}

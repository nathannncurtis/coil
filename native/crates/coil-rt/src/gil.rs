//! The GIL: one attached thread, the switch interval, and the attach and
//! detach of threads and extensions; the free-threaded build stays on the
//! roadmap (spec section 10.3).
//!
//! The semantics are CPython 3.12's `ceval_gil.c`: a mutex and condition
//! variable with `locked`, `switch_number` and `last_holder` state, a switch
//! interval of 5 ms by default (`sys.setswitchinterval`, once `sys` exists),
//! and the fairness rule — a waiter that timed out without a GIL switch sets
//! the drop-request bit in the holder's eval-breaker word (section 9.4).
//! `take_gil` resets the request on every acquire, as `ceval_gil.c` does,
//! so no drop request survives a GIL switch.

use core::ffi::{c_double, c_int, c_ulong};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::eval_breaker;
use crate::tstate::{self, ThreadState};

/// The default switch interval in microseconds: 5 ms.
pub const DEFAULT_SWITCH_INTERVAL_US: u64 = 5_000;

/// CPython's `PyGILState_LOCKED` (gilstate.h): `Ensure` found the GIL held.
pub const PyGILState_LOCKED: c_int = 0;
/// CPython's `PyGILState_UNLOCKED`: `Ensure` had to attach the thread.
pub const PyGILState_UNLOCKED: c_int = 1;

struct GilInner {
    locked: bool,
    switch_number: u64,
}

struct GilRuntime {
    interval_us: AtomicU64,
    inner: Mutex<GilInner>,
    cond: Condvar,
    last_holder: AtomicPtr<ThreadState>,
}

static GIL: GilRuntime = GilRuntime {
    interval_us: AtomicU64::new(DEFAULT_SWITCH_INTERVAL_US),
    inner: Mutex::new(GilInner {
        locked: false,
        switch_number: 0,
    }),
    cond: Condvar::new(),
    last_holder: AtomicPtr::new(ptr::null_mut()),
};

/// CPython 3.12's `take_gil`: wait for the GIL; after a full switch interval
/// without a switch, set the drop-request bit in the holder's eval-breaker
/// word (the fairness mechanism that forces the switch). Every acquire
/// resets the request — `ceval_gil.c` clears `gil_drop_request` whenever it
/// is set on entry to the holder's section — so no request survives a
/// switch.
///
/// # Safety
///
/// `tstate` must be the current thread's own state, and any state the GIL
/// hands over must stay live (thread states are never freed).
pub unsafe fn take_gil(tstate: *mut ThreadState) {
    let mut guard = GIL.inner.lock().expect("GIL mutex poisoned");
    while guard.locked {
        let saved_switch_number = guard.switch_number;
        // Re-read the interval every wait so `sys.setswitchinterval`
        // applies immediately, as in `take_gil`.
        let interval_us = GIL.interval_us.load(Ordering::Relaxed).max(1);
        let interval = Duration::from_micros(interval_us);
        let start = Instant::now();
        let (released, _result) = GIL
            .cond
            .wait_timeout(guard, interval)
            .expect("GIL mutex poisoned");
        guard = released;
        // `Condvar::wait_timeout` can return early on a spurious wakeup or a
        // racing signal, so measure the timeout like `take_gil` does on
        // Windows rather than trusting `WaitTimeoutResult`.
        if start.elapsed() >= interval && guard.locked && guard.switch_number == saved_switch_number
        {
            let holder = GIL.last_holder.load(Ordering::Relaxed);
            if holder.is_null() {
                continue;
            }
            // SAFETY: the holder pointer is a live thread state.
            unsafe { eval_breaker::set_drop_request(holder) };
        }
    }
    guard.locked = true;
    let previous = GIL.last_holder.swap(tstate, Ordering::Relaxed);
    if !ptr::eq(previous, tstate) {
        guard.switch_number += 1;
    }
    // The request lives in the holder's breaker word here, not in one global
    // word as in CPython, so the reset covers both places a pending bit can
    // sit: this state's own word (a waiter set it before this thread last
    // detached) and the word of the state the GIL is taken over from.
    // SAFETY: live thread states; states are never freed, so the previous
    // holder's pointer stays valid after its thread detaches.
    unsafe { eval_breaker::clear_drop_request(tstate) };
    if !previous.is_null() && !ptr::eq(previous, tstate) {
        unsafe { eval_breaker::clear_drop_request(previous) };
    }
}

/// CPython 3.12's `drop_gil`: release the GIL, if this thread holds it, and
/// wake one waiter.
///
/// # Safety
///
/// `tstate` must be the current thread's own state.
pub unsafe fn drop_gil(tstate: *mut ThreadState) {
    let mut guard = GIL.inner.lock().expect("GIL mutex poisoned");
    debug_assert!(guard.locked, "dropping a GIL this thread does not hold");
    if ptr::eq(GIL.last_holder.load(Ordering::Relaxed), tstate) {
        guard.locked = false;
    }
    drop(guard);
    GIL.cond.notify_one();
}

/// Attach `tstate`: take the GIL and mark the state attached — "attached" is
/// PEP 703's term for what holding the GIL means in v1 (section 10.3).
///
/// # Safety
///
/// `tstate` must be the current thread's own state.
pub unsafe fn attach(tstate: *mut ThreadState) {
    // SAFETY: the caller's contract.
    unsafe { take_gil(tstate) };
    // SAFETY: the caller's contract.
    unsafe { (*tstate).attached = true };
}

/// Detach `tstate`: mark it detached and drop the GIL.
///
/// # Safety
///
/// As `attach`.
pub unsafe fn detach(tstate: *mut ThreadState) {
    // SAFETY: the caller's contract.
    unsafe { (*tstate).attached = false };
    // SAFETY: the caller's contract.
    unsafe { drop_gil(tstate) };
}

/// See the CPython 3.12 contract for this entry point. What
/// `Py_BEGIN_ALLOW_THREADS` expands to: release the GIL and return the
/// thread state `Py_END_ALLOW_THREADS` passes to `PyEval_RestoreThread`.
#[unsafe(no_mangle)]
pub extern "C" fn PyEval_SaveThread() -> *mut ThreadState {
    let tstate = tstate::current();
    // SAFETY: `tstate` is the current thread's own state.
    unsafe {
        if (*tstate).attached {
            detach(tstate);
        }
    }
    tstate
}

/// See the CPython 3.12 contract for this entry point. What
/// `Py_END_ALLOW_THREADS` expands to: re-attach the thread state
/// `PyEval_SaveThread` returned.
///
/// # Safety
///
/// `tstate` must be null or a thread state this runtime created for the
/// calling thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyEval_RestoreThread(tstate: *mut ThreadState) {
    if tstate.is_null() {
        return;
    }
    // SAFETY: the caller's contract; a state already attached is left alone.
    unsafe {
        if !(*tstate).attached {
            attach(tstate);
        }
    }
}

/// See the CPython 3.12 contract for this entry point: acquire the GIL for
/// the current thread, creating and attaching a thread state on first use
/// from a foreign thread.
#[unsafe(no_mangle)]
pub extern "C" fn PyGILState_Ensure() -> c_int {
    let tstate = tstate::current();
    // SAFETY: `tstate` is the current thread's own state.
    unsafe {
        let already_attached = (*tstate).attached;
        (*tstate).gilstate_counter += 1;
        if already_attached {
            return PyGILState_LOCKED;
        }
        attach(tstate);
    }
    PyGILState_UNLOCKED
}

/// See the CPython 3.12 contract for this entry point: drop one `Ensure`
/// reference. Every `UNLOCKED` release detaches the thread, as
/// `pystate.c`'s `PyGILState_Release` does — its `Ensure` counterpart
/// attached, so the GIL goes back (3.12 deletes the state outright when
/// the counter reaches zero; ours keeps it bound to the thread for the
/// next `Ensure`, so the detach covers that case too).
#[unsafe(no_mangle)]
pub extern "C" fn PyGILState_Release(oldstate: c_int) {
    let tstate = tstate::try_current();
    if tstate.is_null() {
        return;
    }
    // SAFETY: `tstate` is the current thread's own state.
    unsafe {
        if (*tstate).gilstate_counter > 0 {
            (*tstate).gilstate_counter -= 1;
        }
        if oldstate == PyGILState_UNLOCKED && (*tstate).attached {
            detach(tstate);
        }
    }
}

/// See the CPython 3.12 contract for this entry point: whether the current
/// thread holds the GIL.
#[unsafe(no_mangle)]
pub extern "C" fn PyGILState_Check() -> c_int {
    let tstate = tstate::try_current();
    // SAFETY: a flag read of the current thread's state, or a null check.
    (!tstate.is_null() && unsafe { (*tstate).attached }) as c_int
}

/// The switch interval in microseconds, `sys.getswitchinterval`'s value.
pub fn switch_interval_us() -> u64 {
    GIL.interval_us.load(Ordering::Relaxed)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyEval_SetSwitchInterval(seconds: c_double) {
    // `take_gil` floors anything below one microsecond, as CPython does.
    let us = if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1e6) as u64
    } else {
        1
    };
    GIL.interval_us.store(us.max(1), Ordering::Relaxed);
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyEval_GetSwitchInterval() -> c_double {
    switch_interval_us() as c_double / 1e6
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _PyEval_SetSwitchInterval(us: c_ulong) {
    // `c_ulong` is u32 on the Windows ABI this runtime also builds for.
    #[allow(clippy::unnecessary_cast)]
    let us = us as u64;
    GIL.interval_us.store(us.max(1), Ordering::Relaxed);
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _PyEval_GetSwitchInterval() -> c_ulong {
    switch_interval_us() as c_ulong
}

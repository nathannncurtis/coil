//! Thread states and their TLS slot (spec section 10.2).
//!
//! One `ThreadState` per thread that has run Python: the attached/detached
//! state (PEP 703's terms; "holds the GIL" in v1, section 10.3), the pending
//! exception and its traceback, the `exc_info` stack, the recursion depth,
//! the eval-breaker word, the saved frame pointer of a detached thread, the
//! `PyGILState` counter, the context, the allocator-cache pointer and the
//! zero-count-table pointer of section 9.
//!
//! The slot: on Windows a `TlsAlloc` index below 64, read through the TEB
//! (`gs:[0x1480 + 8*index]`, `TEB.TlsSlots`) on x64 so compiled code can do
//! the same in one instruction; on Unix a `thread_local!`. States are
//! created on first use and live until the process ends: `PyGILState_Release`
//! detaches (3.12 keeps the thread state bound for reuse) and destruction
//! belongs to the runtime-shutdown and collector tasks that own the thread
//! registry. The raw message of the last `PyErr_SetString`-style set rides
//! along until str objects exist to carry it in `args` (the value itself is
//! a real exception instance with the exact layout of section 8.2).

use core::ffi::c_void;

use coil_capi_sys::PyObject;

/// CPython 3.12's `_PyErr_StackItem` (pycore_error.h): the `sys.exc_info()`
/// visible inside one `except` block, pushed by compiled code entering a
/// handler.
#[repr(C)]
pub struct PyErr_StackItem {
    /// The handler's exception type, or null.
    pub exc_type: *mut PyObject,
    /// The handler's exception instance, or null.
    pub exc_value: *mut PyObject,
    /// The handler's traceback, or null.
    pub exc_traceback: *mut PyObject,
}

/// The per-thread state of spec section 10.2.
pub struct ThreadState {
    /// Whether this thread holds the GIL: attached in PEP 703's terms.
    pub attached: bool,
    /// 3.12's `curexc_value`: the pending exception, an owned instance of a
    /// `BaseException` subclass (null when none is set).
    pub current_exception: *mut PyObject,
    /// 3.12's `curexc_traceback`, owned; `PyErr_GetRaisedException` stitches
    /// it into the instance's own field.
    pub current_traceback: *mut PyObject,
    /// The `exc_info` stack, innermost last.
    pub exc_info: Vec<PyErr_StackItem>,
    /// The recursion depth of compiled frames on this thread.
    pub recursion_depth: i32,
    /// The eval-breaker word (section 9.4): bit 0 is the GIL drop request;
    /// compiled code polls it at back-edges through the TLS slot.
    pub eval_breaker: core::sync::atomic::AtomicI64,
    /// The top compiled frame while detached, for the stack walk of 9.4.
    pub frame: *mut PyObject,
    /// `PyGILState_Ensure`'s nesting counter.
    pub gilstate_counter: u32,
    /// The current context object (the contextvars machinery is a later task).
    pub context: *mut PyObject,
    /// This thread's allocator cache, reached by the collector's
    /// stop-the-world walk; never dereferenced after thread death.
    pub alloc_cache: *mut c_void,
    /// This thread's zero-count table (section 9.2), until the collector
    /// task owns it.
    pub zct: *mut c_void,
    /// The raw bytes of the pending exception's message, the payload the
    /// str type will carry in `args`; empty when the exception was set by
    /// other means.
    pub message: Vec<u8>,
}

impl ThreadState {
    fn new() -> ThreadState {
        ThreadState {
            attached: false,
            current_exception: core::ptr::null_mut(),
            current_traceback: core::ptr::null_mut(),
            exc_info: Vec::new(),
            recursion_depth: 0,
            eval_breaker: core::sync::atomic::AtomicI64::new(0),
            frame: core::ptr::null_mut(),
            gilstate_counter: 0,
            context: core::ptr::null_mut(),
            alloc_cache: crate::alloc::thread_cache::cache_ptr().cast(),
            zct: core::ptr::null_mut(),
            message: Vec::new(),
        }
    }
}

// --------------------------------------------------------------- the TLS slot

#[cfg(target_os = "windows")]
mod slot {
    use super::ThreadState;
    use core::ffi::c_void;
    use std::sync::OnceLock;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn TlsAlloc() -> u32;
        fn TlsGetValue(dw_tls_index: u32) -> *mut c_void;
        fn TlsSetValue(dw_tls_index: u32, lp_tls_value: *mut c_void) -> i32;
    }

    /// `TLS_OUT_OF_INDEXES`.
    const NO_INDEX: u32 = 0xFFFF_FFFF;
    /// Slots 0..64 are `TEB.TlsSlots` at `gs:[0x1480 + 8*index]` on x64;
    /// `TlsAlloc` hands them out first, and only they are one TEB read away.
    const MAX_TEB_SLOT: u32 = 64;

    /// This runtime's TLS index, allocated once at first use.
    fn index() -> u32 {
        static INDEX: OnceLock<u32> = OnceLock::new();
        *INDEX.get_or_init(|| {
            // SAFETY: thread- and process-safe kernel32 entry point.
            let index = unsafe { TlsAlloc() };
            assert!(
                index != NO_INDEX && index < MAX_TEB_SLOT,
                "coil: TlsAlloc did not return a TEB slot below 64"
            );
            index
        })
    }

    /// Read the slot through the TEB on x64, matching the address compiled
    /// code computes; the other Windows architectures read it through
    /// `TlsGetValue` until their TEB layouts are pinned (section 17).
    #[inline]
    unsafe fn read(index: u32) -> *mut c_void {
        #[cfg(target_arch = "x86_64")]
        {
            let address = 0x1480 + 8 * index as u64;
            let value: *mut c_void;
            // SAFETY: a plain load from this thread's own TEB, at an address
            // inside TlsSlots for an index `TlsAlloc` returned below 64.
            unsafe {
                core::arch::asm!(
                    "mov {value}, gs:[{address}]",
                    address = in(reg) address,
                    value = out(reg) value,
                    options(nostack, readonly, preserves_flags),
                );
            }
            value
        }
        #[cfg(not(target_arch = "x86_64"))]
        // SAFETY: the index is a valid `TlsAlloc` slot for this thread.
        {
            unsafe { TlsGetValue(index) }
        }
    }

    pub(super) fn get() -> *mut ThreadState {
        // SAFETY: the index is valid and the slot holds a `ThreadState`
        // pointer or null.
        unsafe { read(index()) as *mut ThreadState }
    }

    pub(super) fn set(state: *mut ThreadState) {
        // SAFETY: the index is valid and the caller passes this thread's own
        // state pointer.
        let stored = unsafe { TlsSetValue(index(), state.cast()) };
        assert_eq!(stored, 1, "coil: TlsSetValue failed");
        // Cross-check the TEB arithmetic against the API on this thread.
        // SAFETY: same index, freshly written.
        let observed = unsafe { read(index()) };
        assert!(core::ptr::eq(observed as *mut ThreadState, state));
    }
}

#[cfg(not(target_os = "windows"))]
mod slot {
    use super::ThreadState;
    use std::cell::Cell;

    thread_local! {
        static SLOT: Cell<*mut ThreadState> = const { Cell::new(core::ptr::null_mut()) };
    }

    pub(super) fn get() -> *mut ThreadState {
        SLOT.with(|slot| slot.get())
    }

    pub(super) fn set(state: *mut ThreadState) {
        SLOT.with(|slot| slot.set(state));
    }
}

/// The current thread's state, creating it (detached) on first use.
pub fn current() -> *mut ThreadState {
    let state = slot::get();
    if !state.is_null() {
        return state;
    }
    let state = Box::into_raw(Box::new(ThreadState::new()));
    slot::set(state);
    state
}

/// The current thread's state, or null before first use; for paths that must
/// not create one (`PyGILState_Check`).
pub fn try_current() -> *mut ThreadState {
    slot::get()
}

// ------------------------------------------------- the r05 error-state surface

/// Record `exc`'s instance with `message` as the pending exception. This is
/// `PyErr_SetString`'s body and the error path every runtime helper uses; it
/// chains any exception already pending the way `_PyErr_SetObject` does.
pub fn set_string(exc: *mut PyObject, message: &[u8]) {
    crate::types::exc::set_string(exc, message);
}

/// The pending exception's type, or null when none is set (CPython's
/// `PyErr_Occurred`).
pub fn occurred() -> *mut PyObject {
    crate::types::exc::occurred()
}

/// The raw message recorded by the pending exception, empty when none or
/// when it was set by other means.
pub fn message() -> Vec<u8> {
    crate::types::exc::message()
}

/// Clear the pending exception (`PyErr_Clear`).
pub fn clear() {
    crate::types::exc::clear();
}

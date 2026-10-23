//! Thread states and their TLS slot in the TEB (spec section 10.2).
//!
//! INTERIM: only the per-thread exception state behind `PyErr_Occurred` /
//! `PyErr_SetString` lives here, so the object and number code of r05 can
//! report C API errors the way section 10.5 describes (a function returns
//! NULL or -1 with the exception set). r08 replaces this file with the real
//! `ThreadState` — attach/detach, the `exc_info` stack, recursion depth, the
//! eval-breaker word and the TEB TLS slot — and turns the raw message bytes
//! below into a real exception instance. Nothing outside `PyErr_*` accessors
//! should be built on this state.

use std::cell::RefCell;

use coil_capi_sys::PyObject;

thread_local! {
    /// The pending exception: its type object and the message bytes of the
    /// last `PyErr_SetString`-style call, kept raw until str objects exist.
    static CURRENT_EXCEPTION: RefCell<(*mut PyObject, Vec<u8>)> =
        const { RefCell::new((core::ptr::null_mut(), Vec::new())) };
}

/// Record `exc` as the pending exception with `message` as its raw payload.
pub fn set_string(exc: *mut PyObject, message: &[u8]) {
    CURRENT_EXCEPTION.with_borrow_mut(|state| {
        *state = (exc, message.to_vec());
    });
}

/// The pending exception's type, or null when none is set (CPython's
/// `PyErr_Occurred`).
pub fn occurred() -> *mut PyObject {
    CURRENT_EXCEPTION.with_borrow(|state| state.0)
}

/// The raw message recorded by the pending exception, empty when none or
/// when it was set by other means.
pub fn message() -> Vec<u8> {
    CURRENT_EXCEPTION.with_borrow(|state| state.1.clone())
}

/// Clear the pending exception (`PyErr_Clear`).
pub fn clear() {
    CURRENT_EXCEPTION.with_borrow_mut(|state| {
        *state = (core::ptr::null_mut(), Vec::new());
    });
}

/// See the CPython 3.12 contract for this entry point. Sets the pending
/// exception from a null-terminated message.
///
/// # Safety
///
/// `exc` must be null or a valid `PyObject`-shaped exception type; `message`
/// must be null or a valid null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyErr_SetString(exc: *mut PyObject, message: *const core::ffi::c_char) {
    if exc.is_null() || message.is_null() {
        return;
    }
    // SAFETY: the caller guarantees a null-terminated C string at `message`.
    let len = unsafe { core::ffi::CStr::from_ptr(message).to_bytes().len() };
    // SAFETY: `len` bytes are readable per the C string contract.
    let bytes = unsafe { core::slice::from_raw_parts(message.cast::<u8>(), len) };
    set_string(exc, bytes);
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_Occurred() -> *mut PyObject {
    occurred()
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyErr_Clear() {
    clear();
}

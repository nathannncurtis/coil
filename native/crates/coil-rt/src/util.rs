//! Layout-only reads of argument containers the calling machinery always
//! passes in CPython shape (a tuple for args, a dict or null for kwds), so
//! slots like `tp_new` can check for excess arguments before the container
//! types exist. The container tasks own these layouts; only the leading
//! field each C API macro reads is touched.

use coil_capi_sys::PyObject;

/// `PyTuple_GET_SIZE` by layout: `ob_size` at the var-object offset.
///
/// # Safety
///
/// `args` must be null or a tuple object.
pub unsafe fn var_size(object: *const PyObject) -> isize {
    if object.is_null() {
        return 0;
    }
    // SAFETY: the var-object header is the first 24 bytes.
    unsafe {
        core::ptr::read_unaligned(
            (object as *const u8).add(coil_rt_abi::cp312::varobject::OB_SIZE) as *const isize,
        )
    }
}

/// `PyDict_GET_SIZE` by layout: `ma_used` at the dict prefix offset.
///
/// # Safety
///
/// `dict` must be null or a dict object.
pub unsafe fn dict_used(object: *const PyObject) -> isize {
    if object.is_null() {
        return 0;
    }
    // SAFETY: `ma_used` is the first field of the dict prefix.
    unsafe {
        core::ptr::read_unaligned(
            (object as *const u8).add(coil_rt_abi::cp312::dict::MA_USED) as *const isize
        )
    }
}

/// A type's `tp_name` as a UTF-8 slice, or `"?"` when unreadable.
///
/// # Safety
///
/// `name` must be null or a valid null-terminated C string.
pub unsafe fn cstr(name: *const core::ffi::c_char) -> &'static str {
    if name.is_null() {
        return "?";
    }
    // SAFETY: the caller guarantees a null-terminated C string.
    let bytes = unsafe { core::ffi::CStr::from_ptr(name).to_bytes() };
    core::str::from_utf8(bytes).unwrap_or("?")
}

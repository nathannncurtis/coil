//! Attribute access by C-string name (spec section 10.1): the
//! `tp_getattro` and `tp_setattro` dispatch behind `PyObject_GetAttrString`
//! and `PyObject_SetAttrString`, which the module machinery and the import
//! system call with interned names.

use core::ffi::{c_char, c_int};

use coil_capi_sys::PyObject;

use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::unicode;

/// `get_attr_string`: intern `name` and dispatch through the object's
/// `tp_getattro`. A new reference, or null with the error set.
///
/// # Safety
///
/// `obj` must be null or a valid object; `name` a NUL-terminated string.
pub(crate) unsafe fn get_attr_string(obj: *mut PyObject, name: *const c_char) -> *mut PyObject {
    // SAFETY: the C string is NUL-terminated per contract.
    let name = unsafe { unicode::PyUnicode_InternFromString(name) };
    if name.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the object and interned-name contracts.
    let value = unsafe { getattr(obj, name) };
    // SAFETY: our own interned reference.
    unsafe { crate::gc::counted::Py_DecRef(name) };
    value
}

/// `set_attr_string`: intern `name` and dispatch through the object's
/// `tp_setattro`; a null `value` deletes the attribute.
///
/// # Safety
///
/// As `get_attr_string`; `value` a valid object or null.
pub(crate) unsafe fn set_attr_string(
    obj: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the C string is NUL-terminated per contract.
    let name = unsafe { unicode::PyUnicode_InternFromString(name) };
    if name.is_null() {
        return -1;
    }
    // SAFETY: the object, interned-name and value contracts.
    let res = unsafe { setattr(obj, name, value) };
    // SAFETY: our own interned reference.
    unsafe { crate::gc::counted::Py_DecRef(name) };
    res
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// As `get_attr_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetAttrString(
    obj: *mut PyObject,
    name: *const c_char,
) -> *mut PyObject {
    // SAFETY: the contracts above.
    unsafe { get_attr_string(obj, name) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// As `set_attr_string`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetAttrString(
    obj: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the contracts above.
    unsafe { set_attr_string(obj, name, value) }
}

/// `tp_getattro` dispatch; the generic attribute protocol (descriptors and
/// type attributes) is a later task, so a type without the slot names that
/// gap rather than guessing.
///
/// # Safety
///
/// `obj` must be a valid object; `name` an exact string.
unsafe fn getattr(obj: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the header read is within the object.
    let getattro = unsafe { (*crate::object::header::type_of(obj)).tp_getattro };
    match getattro {
        Some(getattro) => {
            // SAFETY: the slot contract; `name` is a valid string.
            unsafe { getattro(obj, name) }
        }
        None => {
            tstate::set_string(
                exceptions::system_error(),
                b"attribute protocol not implemented for this type",
            );
            core::ptr::null_mut()
        }
    }
}

/// `tp_setattro` dispatch; the generic gap named as in `getattr`.
///
/// # Safety
///
/// As `getattr`; `value` a valid object or null.
unsafe fn setattr(obj: *mut PyObject, name: *mut PyObject, value: *mut PyObject) -> c_int {
    // SAFETY: the header read is within the object.
    let setattro = unsafe { (*crate::object::header::type_of(obj)).tp_setattro };
    match setattro {
        Some(setattro) => {
            // SAFETY: the slot contract.
            unsafe { setattro(obj, name, value) }
        }
        None => {
            tstate::set_string(
                exceptions::system_error(),
                b"attribute protocol not implemented for this type",
            );
            -1
        }
    }
}

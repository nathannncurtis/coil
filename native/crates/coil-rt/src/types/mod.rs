//! The built-in types — long, unicode, bytes, tuple, list, dict, set,
//! float, complex, function, code, frame, generators, exceptions — over
//! the exact and prefix layouts (spec sections 8.2, 8.4 and 8.5).

pub mod bool_;
pub mod complex;
pub mod dict;
pub mod exc;
pub mod float_;
pub mod list;
pub mod long;
pub mod set;
pub mod tuple;

use core::ffi::c_int;

use coil_capi_sys::PyObject;

use crate::object::{header, singletons, typeobject};
use crate::types::exc as exceptions;

/// The outcome of a rich comparison through a `tp_richcompare` slot pair:
/// a decision, a double `NotImplemented`, or a raised exception.
pub(crate) enum Cmp {
    Bool(bool),
    NotImplemented,
    Error,
}

/// `PyObject_RichCompareBool`'s dispatch for the container types: the
/// identity shortcuts for `==`/`!=` (CPython's, and those alone), then
/// the left operand's slot, then the reflected right operand's when the
/// left declined (`do_richcompare`'s pairing). No subclass-priority
/// rule yet: static types have no metaclass-overridden comparisons to
/// order.
///
/// # Safety
///
/// `a` and `b` must point to valid `PyObject` headers of readied types.
pub(crate) unsafe fn rich_compare(a: *mut PyObject, b: *mut PyObject, op: c_int) -> Cmp {
    use typeobject::{Py_EQ, Py_GE, Py_GT, Py_LE, Py_LT, Py_NE};
    // SAFETY: header reads within the objects.
    unsafe {
        // `PyObject_RichCompareBool`'s shortcut: only == and != may
        // resolve on identity; the ordering operators must reach the
        // slots, which may legitimately raise (complex < complex).
        if core::ptr::eq(a, b) {
            if op == Py_EQ {
                return Cmp::Bool(true);
            }
            if op == Py_NE {
                return Cmp::Bool(false);
            }
        }
        let mut result = compare_via(header::type_of(a), a, b, op);
        if matches!(result, Cmp::NotImplemented) {
            let reflected = match op {
                Py_LT => Py_GT,
                Py_GT => Py_LT,
                Py_LE => Py_GE,
                Py_GE => Py_LE,
                other => other,
            };
            result = compare_via(header::type_of(b), b, a, reflected);
        }
        result
    }
}

/// One `tp_richcompare` call, mapped onto [`Cmp`].
///
/// # Safety
///
/// `ty` must be `owner`'s readied type; `a`/`b` are the call's operands.
unsafe fn compare_via(
    ty: *mut coil_capi_sys::PyTypeObject,
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> Cmp {
    // SAFETY: the slot read is within the type object.
    match unsafe { (*ty).tp_richcompare } {
        None => Cmp::NotImplemented,
        Some(compare) => {
            // SAFETY: the slot takes any two objects of the type's family.
            let result = unsafe { compare(a, b, op) };
            if result.is_null() {
                Cmp::Error
            } else if core::ptr::eq(result, singletons::not_implemented()) {
                Cmp::NotImplemented
            } else if core::ptr::eq(result, singletons::true_()) {
                Cmp::Bool(true)
            } else if core::ptr::eq(result, singletons::false_()) {
                Cmp::Bool(false)
            } else {
                // A type returning a non-bool violates its contract; the
                // object protocol task adds coercion. Treat as no answer.
                Cmp::NotImplemented
            }
        }
    }
}

/// Whether `a` and `b` are indistinguishable as a dict key or set element:
/// the identity fast path CPython's lookups take, then `==` through the
/// slots. `Err` carries a raised comparison error.
///
/// # Safety
///
/// `a` and `b` must point to valid `PyObject` headers of readied types.
pub(crate) unsafe fn objects_equal(a: *mut PyObject, b: *mut PyObject) -> Result<bool, ()> {
    // SAFETY: both are valid objects.
    match unsafe { rich_compare(a, b, typeobject::Py_EQ) } {
        Cmp::Bool(equal) => Ok(equal),
        // Both sides declined: only identity (already checked) equates them.
        Cmp::NotImplemented => Ok(false),
        Cmp::Error => Err(()),
    }
}

/// `PyObject_Hash` for the container types: `tp_hash` dispatch, `-1` on
/// error with the exception set (CPython's reserved value).
///
/// # Safety
///
/// `o` must point to a valid `PyObject` header of a readied type.
pub(crate) unsafe fn object_hash(o: *mut PyObject) -> isize {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(o) };
    // SAFETY: the slot read is within the type object.
    let Some(hash) = (unsafe { (*ty).tp_hash }) else {
        // Readying installs PyObject_HashNotImplemented on unhashable
        // types, so a missing slot means the type was never readied.
        let name = if ty.is_null() {
            "?".to_string()
        } else {
            // SAFETY: `tp_name` of a static type is a valid C string.
            unsafe { crate::util::cstr((*ty).tp_name) }.to_string()
        };
        crate::tstate::set_string(
            exceptions::type_error(),
            format!("unhashable type: '{name}'").as_bytes(),
        );
        return -1;
    };
    // SAFETY: the slot takes an object of the type's family.
    unsafe { hash(o) }
}

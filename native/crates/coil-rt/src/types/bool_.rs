//! `bool`: CPython 3.12's `PyBool_Type`, an `int` subtype whose only
//! instances are the immortal `True` and `False` (spec sections 8.1 and
//! 8.2).
//!
//! The arithmetic redefines only what `boolobject.c` redefines — `~`, `&`,
//! `|`, `^` — so that boolean operands keep producing bools; everything
//! else inherits `int`'s slots through `PyLong_Type`. The bitwise
//! inversion delegates to `int`'s without CPython's `DeprecationWarning`,
//! which needs the warnings machinery of a later task.

use core::ffi::c_int;

use coil_capi_sys::{Py_TPFLAGS_DEFAULT, PyNumberMethods, PyObject, PyTypeObject};

use crate::object::singletons;
use crate::object::typeobject::zero_type;

/// See the CPython 3.12 contract for this entry point: `True` or `False`
/// themselves, never a fresh object.
#[unsafe(no_mangle)]
pub extern "C" fn PyBool_FromLong(ok: c_int) -> *mut PyObject {
    if ok != 0 {
        singletons::true_()
    } else {
        singletons::false_()
    }
}

/// `PyBool_Check`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_bool(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { crate::object::header::type_of(op) };
    !ty.is_null() && core::ptr::eq(ty, &raw mut PyBool_Type)
}

unsafe extern "C" fn bool_dealloc(boolean: *mut PyObject) {
    // Bool instances are immortal; a stray decref only restores the count.
    // SAFETY: the header write is within the object.
    unsafe { (*boolean).ob_refcnt = crate::object::header::IMMORTAL_REFCNT };
}

unsafe extern "C" fn bool_invert(v: *mut PyObject) -> *mut PyObject {
    // `int`'s inversion; the deprecation warning needs the warnings task.
    // SAFETY: `PyLong_Type` is readied and `nb_invert` set on its table.
    unsafe {
        let table = crate::types::long::PyLong_Type.tp_as_number;
        (*table).nb_invert.unwrap_unchecked()(v)
    }
}

unsafe extern "C" fn bool_and(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the type reads follow the check; `int`'s slot is set.
    unsafe {
        if !is_bool(a) || !is_bool(b) {
            let table = crate::types::long::PyLong_Type.tp_as_number;
            return (*table).nb_and.unwrap_unchecked()(a, b);
        }
        PyBool_FromLong(
            (core::ptr::eq(a, singletons::true_()) & core::ptr::eq(b, singletons::true_()))
                as c_int,
        )
    }
}

unsafe extern "C" fn bool_xor(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the type reads follow the check; `int`'s slot is set.
    unsafe {
        if !is_bool(a) || !is_bool(b) {
            let table = crate::types::long::PyLong_Type.tp_as_number;
            return (*table).nb_xor.unwrap_unchecked()(a, b);
        }
        PyBool_FromLong(
            (core::ptr::eq(a, singletons::true_()) ^ core::ptr::eq(b, singletons::true_()))
                as c_int,
        )
    }
}

unsafe extern "C" fn bool_or(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the type reads follow the check; `int`'s slot is set.
    unsafe {
        if !is_bool(a) || !is_bool(b) {
            let table = crate::types::long::PyLong_Type.tp_as_number;
            return (*table).nb_or.unwrap_unchecked()(a, b);
        }
        PyBool_FromLong(
            (core::ptr::eq(a, singletons::true_()) | core::ptr::eq(b, singletons::true_()))
                as c_int,
        )
    }
}

const fn bool_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"bool".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::long::OB_DIGIT as isize;
    t.tp_itemsize = core::mem::size_of::<u32>() as isize;
    t.tp_dealloc = Some(bool_dealloc);
    t.tp_as_number = &raw mut BOOL_AS_NUMBER;
    t.tp_flags = Py_TPFLAGS_DEFAULT;
    t.tp_base = &raw mut crate::types::long::PyLong_Type;
    t
}

/// `bool`'s number slots: the three operations `boolobject.c` overrides to
/// keep bool results for bool operands, plus the inversion it forwards to
/// `int`. Everything else inherits through `PyLong_Type`.
static mut BOOL_AS_NUMBER: PyNumberMethods = PyNumberMethods {
    nb_invert: Some(bool_invert),
    nb_and: Some(bool_and),
    nb_xor: Some(bool_xor),
    nb_or: Some(bool_or),
    nb_add: None,
    nb_subtract: None,
    nb_multiply: None,
    nb_remainder: None,
    nb_divmod: None,
    nb_power: None,
    nb_negative: None,
    nb_positive: None,
    nb_absolute: None,
    nb_bool: None,
    nb_lshift: None,
    nb_rshift: None,
    nb_int: None,
    nb_reserved: core::ptr::null_mut(),
    nb_float: None,
    nb_inplace_add: None,
    nb_inplace_subtract: None,
    nb_inplace_multiply: None,
    nb_inplace_remainder: None,
    nb_inplace_power: None,
    nb_inplace_lshift: None,
    nb_inplace_rshift: None,
    nb_inplace_and: None,
    nb_inplace_xor: None,
    nb_inplace_or: None,
    nb_floor_divide: None,
    nb_true_divide: None,
    nb_inplace_floor_divide: None,
    nb_inplace_true_divide: None,
    nb_index: None,
    nb_matrix_multiply: None,
    nb_inplace_matrix_multiply: None,
};

/// The `bool` type object (CPython's `PyBool_Type`). `tp_repr` and
/// `tp_new` stay unset until `str` objects and argument parsing exist;
/// `tp_richcompare` and `tp_hash` inherit `int`'s, as in CPython.
#[unsafe(no_mangle)]
pub static mut PyBool_Type: PyTypeObject = bool_type();

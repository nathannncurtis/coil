//! The immortal singletons — `None`, `True`, `False`, `NotImplemented`,
//! `Ellipsis` — and CPython's small integers -5..256, every one at
//! `ob_refcnt = 0xFFFF_FFFF` (PEP 683, spec section 8.1), so `is` behaves
//! exactly as in CPython 3.12.

use coil_capi_sys::{PyObject, PyTypeObject};

use super::header;
use super::typeobject::zero_type;
use crate::types::bool_::PyBool_Type;
use crate::types::long::{PyLongObject, tag_from_sign_and_size};

/// CPython's `_PY_NSMALLNEGINTS`.
pub const NSMALLNEGINTS: i64 = 5;
/// CPython's `_PY_NSMALLPOSINTS` (257: 0 through 256 inclusive).
pub const NSMALLPOSINTS: i64 = 257;
/// The small-int range, inclusive: -5 ..= 256.
pub const SMALL_INT_MIN: i64 = -NSMALLNEGINTS;
pub const SMALL_INT_MAX: i64 = NSMALLPOSINTS - 1;

/// `True` and `False` are one-digit `PyLongObject`s of `bool`
/// (boolobject.c): `lv_tag` per `_PyLong_TRUE_TAG`/`_PyLong_FALSE_TAG`.
const fn bool_struct(tag: usize, digit: u32) -> PyLongObject {
    PyLongObject {
        ob_base: PyObject {
            ob_refcnt: header::IMMORTAL_REFCNT,
            ob_type: &raw mut PyBool_Type,
        },
        lv_tag: tag,
        ob_digit: [digit],
    }
}

/// CPython's `_Py_FalseStruct`: zero-valued, so its tag is `SIGN_ZERO`.
#[unsafe(no_mangle)]
pub static mut _Py_FalseStruct: PyLongObject = bool_struct(tag_from_sign_and_size(0, 0), 0);

/// CPython's `_Py_TrueStruct`.
#[unsafe(no_mangle)]
pub static mut _Py_TrueStruct: PyLongObject = bool_struct(tag_from_sign_and_size(1, 1), 1);

/// CPython's `_Py_NoneStruct`; the type is `_PyNone_Type` ("NoneType").
#[unsafe(no_mangle)]
pub static mut _Py_NoneStruct: PyObject = PyObject {
    ob_refcnt: header::IMMORTAL_REFCNT,
    ob_type: &raw mut _PyNone_Type,
};

/// CPython's `_Py_NotImplementedStruct`.
#[unsafe(no_mangle)]
pub static mut _Py_NotImplementedStruct: PyObject = PyObject {
    ob_refcnt: header::IMMORTAL_REFCNT,
    ob_type: &raw mut _PyNotImplemented_Type,
};

/// CPython's `_Py_EllipsisObject`.
#[unsafe(no_mangle)]
pub static mut _Py_EllipsisObject: PyObject = PyObject {
    ob_refcnt: header::IMMORTAL_REFCNT,
    ob_type: &raw mut PyEllipsis_Type,
};

unsafe extern "C" fn none_dealloc(object: *mut PyObject) {
    // A stray decref on an immortal singleton resets the count instead of
    // freeing, as in CPython's none_dealloc.
    // SAFETY: the header write is within the object.
    unsafe { (*object).ob_refcnt = header::IMMORTAL_REFCNT };
}

unsafe extern "C" fn none_hash(_object: *mut PyObject) -> isize {
    0xFCA86420
}

unsafe extern "C" fn none_new(
    _type_: *mut PyTypeObject,
    args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: layout read of the argument tuple.
    if unsafe { crate::util::var_size(args) } > 0 {
        crate::tstate::set_string(
            super::exceptions::type_error(),
            b"NoneType takes no arguments",
        );
        return core::ptr::null_mut();
    }
    none()
}

const fn none_type_object() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"NoneType".as_ptr();
    t.tp_dealloc = Some(none_dealloc);
    t.tp_hash = Some(none_hash);
    t.tp_richcompare = Some(super::typeobject::object_identity_richcompare);
    t.tp_new = Some(none_new);
    t
}

/// `NoneType` (CPython's `_PyNone_Type`).
#[unsafe(no_mangle)]
pub static mut _PyNone_Type: PyTypeObject = none_type_object();

unsafe extern "C" fn not_implemented_dealloc(object: *mut PyObject) {
    // SAFETY: the header write is within the object.
    unsafe { (*object).ob_refcnt = header::IMMORTAL_REFCNT };
}

const fn not_implemented_type_object() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"NotImplementedType".as_ptr();
    t.tp_dealloc = Some(not_implemented_dealloc);
    t.tp_richcompare = Some(super::typeobject::object_identity_richcompare);
    t
}

/// CPython's `_PyNotImplemented_Type`.
#[unsafe(no_mangle)]
pub static mut _PyNotImplemented_Type: PyTypeObject = not_implemented_type_object();

unsafe extern "C" fn ellipsis_dealloc(object: *mut PyObject) {
    // SAFETY: the header write is within the object.
    unsafe { (*object).ob_refcnt = header::IMMORTAL_REFCNT };
}

const fn ellipsis_type_object() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"ellipsis".as_ptr();
    t.tp_dealloc = Some(ellipsis_dealloc);
    t.tp_richcompare = Some(super::typeobject::object_identity_richcompare);
    t
}

/// CPython's `PyEllipsis_Type`.
#[unsafe(no_mangle)]
pub static mut PyEllipsis_Type: PyTypeObject = ellipsis_type_object();

/// The small integers, immortal and exact `PyLongObject`s, in CPython's
/// order: index `value + NSMALLNEGINTS`.
static mut SMALL_INTS: [PyLongObject; (NSMALLNEGINTS + NSMALLPOSINTS) as usize] = {
    const COUNT: usize = (NSMALLNEGINTS + NSMALLPOSINTS) as usize;
    const fn singleton(value: i64) -> PyLongObject {
        let sign: i32 = if value == 0 {
            0
        } else if value < 0 {
            -1
        } else {
            1
        };
        let size = if value == 0 { 0 } else { 1 };
        PyLongObject {
            ob_base: PyObject {
                ob_refcnt: header::IMMORTAL_REFCNT,
                ob_type: &raw mut crate::types::long::PyLong_Type,
            },
            lv_tag: tag_from_sign_and_size(sign, size),
            ob_digit: [value.unsigned_abs() as u32],
        }
    }
    let mut ints = [const { singleton(0) }; COUNT];
    let mut i = 0;
    while i < COUNT {
        ints[i] = singleton(i as i64 - NSMALLNEGINTS);
        i += 1;
    }
    ints
};

/// The address of the small-int singleton for `value`, or null when `value`
/// is outside -5..=256.
pub fn get_small_int(value: i64) -> *mut PyObject {
    if !(SMALL_INT_MIN..=SMALL_INT_MAX).contains(&value) {
        return core::ptr::null_mut();
    }
    // SAFETY: the index is in range and the singleton is only addressed,
    // never mutated after init.
    unsafe { (&raw mut SMALL_INTS[(value + NSMALLNEGINTS) as usize]).cast() }
}

/// `Py_None` — a borrowed reference to the immortal `None`.
pub fn none() -> *mut PyObject {
    &raw mut _Py_NoneStruct
}

/// `Py_True`.
pub fn true_() -> *mut PyObject {
    (&raw mut _Py_TrueStruct).cast()
}

/// `Py_False`.
pub fn false_() -> *mut PyObject {
    (&raw mut _Py_FalseStruct).cast()
}

/// `Py_NotImplemented`.
pub fn not_implemented() -> *mut PyObject {
    &raw mut _Py_NotImplementedStruct
}

/// `Py_Ellipsis`.
pub fn ellipsis() -> *mut PyObject {
    &raw mut _Py_EllipsisObject
}

/// The `NoneType` type object.
pub fn none_type() -> *mut PyTypeObject {
    &raw mut _PyNone_Type
}

/// The `NotImplementedType` type object.
pub fn not_implemented_type() -> *mut PyTypeObject {
    &raw mut _PyNotImplemented_Type
}

/// The `ellipsis` type object.
pub fn ellipsis_type() -> *mut PyTypeObject {
    &raw mut PyEllipsis_Type
}

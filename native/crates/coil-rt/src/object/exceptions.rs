//! The exception type objects the object and number code raises through.
//! The instance layout — `PyBaseExceptionObject` and subclasses — is exact
//! (spec section 8.2) and this module defines only what r05 needs: the type
//! objects with `tp_basicsize` over that layout, their traverse, and the
//! exported `PyExc_*` pointer slots CPython declares. The exception
//! machinery — instances, `__init__`, raise/render — is the exceptions
//! task's; these types never go away, so they are static, not heap types.

use core::ffi::{c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASE_EXC_SUBCLASS, Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC,
    PyObject, PyTypeObject, visitproc,
};

use super::typeobject::zero_type;

unsafe extern "C" fn base_exception_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // The managed fields of the exact BaseException layout (8.2): dict,
    // args, notes, traceback, context, cause.
    // SAFETY: every field read is within the instance layout.
    unsafe {
        let base = object as *const u8;
        for offset in [
            coil_rt_abi::cp312::base_exception::DICT,
            coil_rt_abi::cp312::base_exception::ARGS,
            coil_rt_abi::cp312::base_exception::NOTES,
            coil_rt_abi::cp312::base_exception::TRACEBACK,
            coil_rt_abi::cp312::base_exception::CONTEXT,
            coil_rt_abi::cp312::base_exception::CAUSE,
        ] {
            let referent = core::ptr::read_unaligned(base.add(offset) as *const *mut PyObject);
            if !referent.is_null() && visit(referent, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

unsafe extern "C" fn base_exception_clear(_object: *mut PyObject) -> c_int {
    // Instances do not exist until the exceptions task; the slot must be
    // non-null for `Py_TPFLAGS_HAVE_GC` and the fields it would clear are
    // the six traverse visits above.
    0
}

unsafe extern "C" fn exception_dealloc(object: *mut PyObject) {
    // Exception instances this runtime allocates live on the GC heap.
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

const fn base_exception_type_object() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"BaseException".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::base_exception::SIZEOF as isize;
    t.tp_dealloc = Some(exception_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_BASE_EXC_SUBCLASS
        | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(base_exception_traverse);
    t.tp_clear = Some(base_exception_clear);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `BaseException` type object itself.
static mut _PyExc_BaseException_Type: PyTypeObject = base_exception_type_object();

macro_rules! exception_type {
    ($(#[$doc:meta])* $static_name:ident, $pointer_name:ident, $name:literal) => {
        $(#[$doc])*
        static mut $static_name: PyTypeObject = {
            let mut t = zero_type();
            t.tp_name = concat!($name, "\0").as_ptr() as *const core::ffi::c_char;
            t.tp_dealloc = Some(exception_dealloc);
            t.tp_flags = Py_TPFLAGS_DEFAULT
                | Py_TPFLAGS_BASETYPE
                | Py_TPFLAGS_BASE_EXC_SUBCLASS;
            t.tp_base = &raw mut _PyExc_BaseException_Type;
            t
        };

        /// CPython's `PyExc_<name>` pointer slot; the C API addresses
        /// exception types through these.
        #[unsafe(no_mangle)]
        pub static mut $pointer_name: *mut PyObject =
            &raw mut $static_name as *mut PyObject;
    };
}

exception_type!(
    /// `TypeError`.
    _PyExc_TypeError_Type,
    PyExc_TypeError,
    "TypeError"
);
exception_type!(
    /// `ValueError`.
    _PyExc_ValueError_Type,
    PyExc_ValueError,
    "ValueError"
);
exception_type!(
    /// `OverflowError`.
    _PyExc_OverflowError_Type,
    PyExc_OverflowError,
    "OverflowError"
);
exception_type!(
    /// `ZeroDivisionError`.
    _PyExc_ZeroDivisionError_Type,
    PyExc_ZeroDivisionError,
    "ZeroDivisionError"
);
exception_type!(
    /// `MemoryError`.
    _PyExc_MemoryError_Type,
    PyExc_MemoryError,
    "MemoryError"
);
exception_type!(
    /// `SystemError`.
    _PyExc_SystemError_Type,
    PyExc_SystemError,
    "SystemError"
);
exception_type!(
    /// `NotImplementedError`.
    _PyExc_NotImplementedError_Type,
    PyExc_NotImplementedError,
    "NotImplementedError"
);

/// The `BaseException` type object for `PyExc_BaseException` and subtype
/// checks.
pub fn base_exception_type() -> *mut PyTypeObject {
    &raw mut _PyExc_BaseException_Type
}

/// CPython's `PyExc_BaseException`.
#[unsafe(no_mangle)]
pub static mut PyExc_BaseException: *mut PyObject =
    &raw mut _PyExc_BaseException_Type as *mut PyObject;

macro_rules! accessor {
    ($(#[$doc:meta])* $fn_name:ident, $static_name:ident) => {
        $(#[$doc])*
        pub fn $fn_name() -> *mut PyObject {
            // SAFETY: reading the exported pointer slot's initial value;
            // nothing reassigns these.
            unsafe { $static_name }
        }
    };
}

accessor!(
    /// The `TypeError` type.
    type_error,
    PyExc_TypeError
);
accessor!(
    /// The `ValueError` type.
    value_error,
    PyExc_ValueError
);
accessor!(
    /// The `OverflowError` type.
    overflow_error,
    PyExc_OverflowError
);
accessor!(
    /// The `ZeroDivisionError` type.
    zero_division_error,
    PyExc_ZeroDivisionError
);
accessor!(
    /// The `MemoryError` type.
    memory_error,
    PyExc_MemoryError
);
accessor!(
    /// The `SystemError` type.
    system_error,
    PyExc_SystemError
);
accessor!(
    /// The `NotImplementedError` type.
    not_implemented_error,
    PyExc_NotImplementedError
);

// Internal helpers with `PyTypeObject` returns for init and subtype checks.
macro_rules! type_accessor {
    ($fn_name:ident, $static_name:ident) => {
        pub(crate) fn $fn_name() -> *mut PyTypeObject {
            &raw mut $static_name
        }
    };
}

type_accessor!(type_error_type, _PyExc_TypeError_Type);
type_accessor!(value_error_type, _PyExc_ValueError_Type);
type_accessor!(overflow_error_type, _PyExc_OverflowError_Type);
type_accessor!(zero_division_error_type, _PyExc_ZeroDivisionError_Type);
type_accessor!(memory_error_type, _PyExc_MemoryError_Type);
type_accessor!(system_error_type, _PyExc_SystemError_Type);
type_accessor!(not_implemented_error_type, _PyExc_NotImplementedError_Type);

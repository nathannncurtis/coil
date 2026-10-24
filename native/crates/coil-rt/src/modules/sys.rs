//! The `sys` core: the attributes the C API and `_testcapi` reach for
//! (spec sections 10.6 and 11.2). Not the full module.

use coil_capi_sys::{METH_NOARGS, METH_O, PyMethodDef, PyObject};

use crate::gc::counted;
use crate::modules;
use crate::object::header;
use crate::tstate;
use crate::types::exc::{self as exceptions};
use crate::types::{float_, long, tuple, unicode};

/// The impersonated version (spec 11.2).
const VERSION: &str = "3.12.10 (coil, Oct 24 2026, 00:00:00)";

/// `sys.getrefcount`: the pin count plus one (spec 11.2).
unsafe extern "C" fn getrefcount(_module: *mut PyObject, op: *mut PyObject) -> *mut PyObject {
    if op.is_null() {
        tstate::set_string(
            exceptions::type_error(),
            b"getrefcount: argument should be a PyObject",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `op` is a valid object.
    let count = unsafe { header::refcnt(op) };
    long::PyLong_FromSsize_t(count + 1)
}

/// `sys.setswitchinterval`: validate and set the interval.
unsafe extern "C" fn setswitchinterval(
    _module: *mut PyObject,
    seconds: *mut PyObject,
) -> *mut PyObject {
    if seconds.is_null() {
        tstate::set_string(
            exceptions::type_error(),
            b"setswitchinterval: argument should be a number",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `seconds` is a valid object.
    let value = unsafe { float_::PyFloat_AsDouble(seconds) };
    if value == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    if value < 0.0 {
        tstate::set_string(exceptions::value_error(), b"seconds must be positive");
        return core::ptr::null_mut();
    }
    crate::gil::PyEval_SetSwitchInterval(value);
    crate::object::singletons::none()
}

/// `sys.getswitchinterval`: the current interval as a float.
unsafe extern "C" fn getswitchinterval(
    _module: *mut PyObject,
    _args: *mut PyObject,
) -> *mut PyObject {
    float_::PyFloat_FromDouble(crate::gil::PyEval_GetSwitchInterval())
}

/// The `sys.version_info` tuple: `(3, 12, 10, "final", 0)`.
fn version_info() -> *mut PyObject {
    let tuple = tuple::PyTuple_New(5);
    if tuple.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the tuple is fresh and the values valid objects.
    unsafe {
        for (i, value) in [
            long::PyLong_FromLong(3),
            long::PyLong_FromLong(12),
            long::PyLong_FromLong(10),
            unicode::from_rust("final"),
            long::PyLong_FromLong(0),
        ]
        .into_iter()
        .enumerate()
        {
            if tuple::PyTuple_SetItem(tuple, i as isize, value) < 0 {
                counted::Py_DecRef(tuple);
                return core::ptr::null_mut();
            }
        }
    }
    tuple
}

/// The `sys.implementation` namespace: `name`, `cache_tag`, `version`,
/// `hexversion`.
fn implementation(version_info: *mut PyObject) -> *mut PyObject {
    // SAFETY: the name is a NUL-terminated string.
    let implementation = unsafe { modules::PyModule_New(c"implementation".as_ptr()) };
    if implementation.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the operands are valid.
    unsafe {
        if modules::PyModule_AddStringConstant(implementation, c"name".as_ptr(), c"coil".as_ptr())
            < 0
        {
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
        // `cache_tag` is None (spec 11.2).
        if modules::PyModule_AddObjectRef(
            implementation,
            c"cache_tag".as_ptr(),
            crate::object::singletons::none(),
        ) < 0
        {
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
        let version = unicode::from_rust(VERSION);
        if version.is_null() {
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
        if modules::PyModule_AddObjectRef(implementation, c"version".as_ptr(), version) < 0 {
            counted::Py_DecRef(version);
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
        counted::Py_DecRef(version);
        let hexversion = long::PyLong_FromLongLong(0x030C0AF0);
        if hexversion.is_null() {
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
        if modules::PyModule_AddObjectRef(implementation, c"hexversion".as_ptr(), hexversion) < 0 {
            counted::Py_DecRef(hexversion);
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
        counted::Py_DecRef(hexversion);
        // `version_info` is shared with the module.
        // SAFETY: `version_info` is a valid object.
        counted::Py_IncRef(version_info);
        if modules::PyModule_AddObjectRef(implementation, c"version_info".as_ptr(), version_info)
            < 0
        {
            counted::Py_DecRef(implementation);
            return core::ptr::null_mut();
        }
    }
    implementation
}

/// The `sys` method table; `const` so the `PyCFunctionObject`s that
/// `PyModule_AddFunctions` creates keep a stable pointer.
const METHODS: [PyMethodDef; 4] = [
    PyMethodDef {
        ml_name: c"getrefcount".as_ptr(),
        ml_meth: Some(getrefcount),
        ml_flags: METH_O,
        ml_doc: c"getrefcount(object) -> integer\n\nReturn the reference count of object. The result is meaningless except for debugging and testing the Python runtime.".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"setswitchinterval".as_ptr(),
        ml_meth: Some(setswitchinterval),
        ml_flags: METH_O,
        ml_doc: c"setswitchinterval(seconds)\n\nSet the ideal time in seconds between checkings for the ability to switch threads.".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"getswitchinterval".as_ptr(),
        ml_meth: Some(getswitchinterval),
        ml_flags: METH_NOARGS,
        ml_doc: c"getswitchinterval() -> float\n\nReturn the current ideal time in seconds between checkings for the ability to switch threads.".as_ptr(),
    },
    PyMethodDef {
        ml_name: core::ptr::null(),
        ml_meth: None,
        ml_flags: 0,
        ml_doc: core::ptr::null(),
    },
];

/// Create the `sys` module with the `sys.modules` mapping; a new reference,
/// or null with the error set.
///
/// # Safety
///
/// `modules` must be the `sys.modules` mapping.
pub unsafe fn create(modules: *mut PyObject) -> *mut PyObject {
    // SAFETY: the name is a NUL-terminated string.
    let module = unsafe { modules::PyModule_New(c"sys".as_ptr()) };
    if module.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the operands are valid.
    unsafe {
        // `sys.modules` owns the mapping.
        if modules::PyModule_AddObjectRef(module, c"modules".as_ptr(), modules) < 0 {
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        let version = unicode::from_rust(VERSION);
        if version.is_null() {
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        if modules::PyModule_AddObjectRef(module, c"version".as_ptr(), version) < 0 {
            counted::Py_DecRef(version);
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        counted::Py_DecRef(version);
        let version_info = version_info();
        if version_info.is_null() {
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        if modules::PyModule_AddObjectRef(module, c"version_info".as_ptr(), version_info) < 0 {
            counted::Py_DecRef(version_info);
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        let hexversion = long::PyLong_FromLongLong(0x030C0AF0);
        if hexversion.is_null() {
            counted::Py_DecRef(version_info);
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        if modules::PyModule_AddObjectRef(module, c"hexversion".as_ptr(), hexversion) < 0 {
            counted::Py_DecRef(hexversion);
            counted::Py_DecRef(version_info);
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        counted::Py_DecRef(hexversion);
        let implementation = implementation(version_info);
        if implementation.is_null() {
            counted::Py_DecRef(version_info);
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        if modules::PyModule_AddObjectRef(module, c"implementation".as_ptr(), implementation) < 0 {
            counted::Py_DecRef(implementation);
            counted::Py_DecRef(version_info);
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        counted::Py_DecRef(implementation);
        counted::Py_DecRef(version_info);
        // The functions.
        if modules::PyModule_AddFunctions(module, METHODS.as_ptr() as *mut PyMethodDef) < 0 {
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
    }
    module
}

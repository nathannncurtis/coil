//! The `_imp` core: the implementation-specific import API (spec section
//! 10.6). Not the full module.

use coil_capi_sys::{METH_O, METH_VARARGS, PyMethodDef, PyObject};

use crate::gc::counted;
use crate::modules;
use crate::tstate;
use crate::types::exc::{self as exceptions};
use crate::types::long;

/// `_imp.create_dynamic(spec)`: load or find the extension for `spec`.
unsafe extern "C" fn create_dynamic(_module: *mut PyObject, spec: *mut PyObject) -> *mut PyObject {
    if spec.is_null() {
        tstate::set_string(
            exceptions::type_error(),
            b"create_dynamic: spec is required",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `spec` is a valid object.
    unsafe { crate::import::create_dynamic(spec) }
}

/// `_imp.exec_dynamic(module)`: run the module's `Py_mod_exec` slot.
unsafe extern "C" fn exec_dynamic(_module: *mut PyObject, module: *mut PyObject) -> *mut PyObject {
    if module.is_null() {
        tstate::set_string(
            exceptions::type_error(),
            b"exec_dynamic: module is required",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `module` is a valid object.
    let result = unsafe { crate::import::exec_dynamic(module) };
    if result < 0 {
        return core::ptr::null_mut();
    }
    long::PyLong_FromLong(result as i32)
}

/// `_imp.is_builtin(name)`: 1 for builtins, -1 for sys/builtins, 0 otherwise.
unsafe extern "C" fn is_builtin(_module: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    if name.is_null() {
        tstate::set_string(exceptions::type_error(), b"is_builtin: name is required");
        return core::ptr::null_mut();
    }
    // SAFETY: `name` is a valid object.
    let result = unsafe { crate::import::is_builtin(name) };
    long::PyLong_FromLong(result as i32)
}

/// The `_imp` method table; `const` so the `PyCFunctionObject`s that
/// `PyModule_AddFunctions` creates keep a stable pointer.
const METHODS: [PyMethodDef; 4] = [
    PyMethodDef {
        ml_name: c"create_dynamic".as_ptr(),
        ml_meth: Some(create_dynamic),
        ml_flags: METH_VARARGS,
        ml_doc: c"create_dynamic(spec)\n\nCreate the module for the given spec.".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"exec_dynamic".as_ptr(),
        ml_meth: Some(exec_dynamic),
        ml_flags: METH_O,
        ml_doc: c"exec_dynamic(module)\n\nExecute the module's Py_mod_exec slot.".as_ptr(),
    },
    PyMethodDef {
        ml_name: c"is_builtin".as_ptr(),
        ml_meth: Some(is_builtin),
        ml_flags: METH_O,
        ml_doc: c"is_builtin(name)\n\nReturn 1 if name is a builtin module, -1 for sys/builtins, 0 otherwise.".as_ptr(),
    },
    PyMethodDef {
        ml_name: core::ptr::null(),
        ml_meth: None,
        ml_flags: 0,
        ml_doc: core::ptr::null(),
    },
];

/// Create the `_imp` module; a new reference, or null with the error set.
pub fn create() -> *mut PyObject {
    // SAFETY: the name is a NUL-terminated string.
    let module = unsafe { modules::PyModule_New(c"_imp".as_ptr()) };
    if module.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the module and methods are valid.
    unsafe {
        if modules::PyModule_AddFunctions(module, METHODS.as_ptr() as *mut PyMethodDef) < 0 {
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
    }
    module
}

//! The `builtins` core: the singletons and the exception types (spec
//! section 10.6). Not the full module — just what the C API and
//! `_testcapi` reach for.

use coil_capi_sys::PyObject;

use crate::gc::counted;
use crate::modules;
use crate::types::exc;

/// Create the `builtins` module: the singletons and the exception types; a
/// new reference, or null with the error set.
pub fn create() -> *mut PyObject {
    // SAFETY: the name is a NUL-terminated string.
    let module = unsafe { modules::PyModule_New(c"builtins".as_ptr()) };
    if module.is_null() {
        return core::ptr::null_mut();
    }
    // The singletons are immortal; the dict's reference is harmless.
    // SAFETY: `module` is a module and each value a valid object.
    unsafe {
        for (name, value) in [
            (c"None".as_ptr(), crate::object::singletons::none()),
            (c"True".as_ptr(), crate::object::singletons::true_()),
            (c"False".as_ptr(), crate::object::singletons::false_()),
            (
                c"NotImplemented".as_ptr(),
                crate::object::singletons::not_implemented(),
            ),
            (c"Ellipsis".as_ptr(), crate::object::singletons::ellipsis()),
        ] {
            if modules::PyModule_AddObjectRef(module, name, value) < 0 {
                counted::Py_DecRef(module);
                return core::ptr::null_mut();
            }
        }
        // The exception hierarchy: `BaseException` first, then every
        // subtype in dependency order.
        if modules::PyModule_AddType(module, exc::base_exception_type()) < 0 {
            counted::Py_DecRef(module);
            return core::ptr::null_mut();
        }
        for type_ in exc::SUBTYPE_OBJECTS.0 {
            if modules::PyModule_AddType(module, *type_) < 0 {
                counted::Py_DecRef(module);
                return core::ptr::null_mut();
            }
        }
    }
    module
}

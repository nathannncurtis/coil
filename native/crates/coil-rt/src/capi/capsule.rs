//! `capsule.h`: the `PyCapsule` type and its C API — opaque C pointers
//! wrapped in a Python object (spec section 10.1).

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{PyObject, PyTypeObject};

use crate::object::header;
use crate::object::typeobject::{PyType_GenericAlloc, zero_type};
use crate::tstate;

/// CPython's `PyCapsule`: the header followed by the four C fields.
#[repr(C)]
struct PyCapsuleObject {
    ob_base: PyObject,
    pointer: *mut c_void,
    name: *const c_char,
    context: *mut c_void,
    destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
}

/// CPython's `capsule_dealloc`: run the destructor, then the type's free.
unsafe extern "C" fn capsule_dealloc(object: *mut PyObject) {
    // SAFETY: the object is a capsule this runtime allocated.
    unsafe {
        let capsule = &mut *object.cast::<PyCapsuleObject>();
        if let Some(destructor) = capsule.destructor {
            destructor(object);
        }
    }
    // SAFETY: the block came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

/// CPython's `capsule_repr`.
unsafe extern "C" fn capsule_repr(object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the field reads are within the capsule.
    unsafe {
        let capsule = &*object.cast::<PyCapsuleObject>();
        let (quote, name) = if capsule.name.is_null() {
            ("", "NULL")
        } else {
            ("\"", crate::util::cstr(capsule.name))
        };
        let text = format!("<capsule object {quote}{name}{quote} at {object:p}>");
        crate::types::unicode::str_from_utf8(&text)
    }
}

const fn capsule_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"PyCapsule".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyCapsuleObject>() as isize;
    t.tp_dealloc = Some(capsule_dealloc);
    t.tp_repr = Some(capsule_repr);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `PyCapsule` type object (CPython's `PyCapsule_Type`).
#[unsafe(no_mangle)]
pub static mut PyCapsule_Type: PyTypeObject = capsule_type();

/// `PyCapsule_CheckExact`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
unsafe fn is_capsule_exact(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    core::ptr::eq(unsafe { header::type_of(op) }, &raw mut PyCapsule_Type)
}

/// CPython's `_is_legal_capsule`: a real capsule with a non-null pointer,
/// else `ValueError` naming the caller.
unsafe fn is_legal_capsule(capsule: *mut PyObject, caller: &str) -> bool {
    unsafe {
        if is_capsule_exact(capsule) && !(*capsule.cast::<PyCapsuleObject>()).pointer.is_null() {
            return true;
        }
        tstate::set_string(
            crate::types::exc::value_error(),
            format!("{caller} called with invalid PyCapsule object").as_bytes(),
        );
        false
    }
}

/// CPython's `name_matches`: two names are the same when both are null or
/// both spell the same string.
fn name_matches(name1: *const c_char, name2: *const c_char) -> bool {
    if name1.is_null() || name2.is_null() {
        return name1 == name2;
    }
    // SAFETY: both point to NUL-terminated strings by contract.
    unsafe { core::ffi::CStr::from_ptr(name1) == core::ffi::CStr::from_ptr(name2) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name`, when not null, must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_New(
    pointer: *mut c_void,
    name: *const c_char,
    destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
) -> *mut PyObject {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if pointer.is_null() {
            tstate::set_string(
                crate::types::exc::value_error(),
                b"PyCapsule_New called with null pointer",
            );
            return core::ptr::null_mut();
        }
        let object = PyType_GenericAlloc(&raw mut PyCapsule_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let capsule = &mut *object.cast::<PyCapsuleObject>();
        capsule.pointer = pointer;
        capsule.name = name;
        capsule.context = core::ptr::null_mut();
        capsule.destructor = destructor;
        object
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name`, when not null, must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_IsValid(o: *mut PyObject, name: *const c_char) -> c_int {
    // SAFETY: the field read is within the capsule the caller vouches for.
    unsafe {
        (is_capsule_exact(o)
            && !(*o.cast::<PyCapsuleObject>()).pointer.is_null()
            && name_matches((*o.cast::<PyCapsuleObject>()).name, name)) as c_int
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name`, when not null, must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetPointer(
    o: *mut PyObject,
    name: *const c_char,
) -> *mut c_void {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_GetPointer") {
            return core::ptr::null_mut();
        }
        let capsule = &*o.cast::<PyCapsuleObject>();
        if !name_matches(name, capsule.name) {
            tstate::set_string(
                crate::types::exc::value_error(),
                b"PyCapsule_GetPointer called with incorrect name",
            );
            return core::ptr::null_mut();
        }
        capsule.pointer
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetName(o: *mut PyObject) -> *const c_char {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_GetName") {
            return core::ptr::null();
        }
        (*o.cast::<PyCapsuleObject>()).name
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetDestructor(
    o: *mut PyObject,
) -> Option<unsafe extern "C" fn(*mut PyObject)> {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_GetDestructor") {
            return None;
        }
        (*o.cast::<PyCapsuleObject>()).destructor
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_GetContext(o: *mut PyObject) -> *mut c_void {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_GetContext") {
            return core::ptr::null_mut();
        }
        (*o.cast::<PyCapsuleObject>()).context
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetPointer(o: *mut PyObject, pointer: *mut c_void) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if pointer.is_null() {
            tstate::set_string(
                crate::types::exc::value_error(),
                b"PyCapsule_SetPointer called with null pointer",
            );
            return -1;
        }
        if !is_legal_capsule(o, "PyCapsule_SetPointer") {
            return -1;
        }
        (*o.cast::<PyCapsuleObject>()).pointer = pointer;
        0
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name`, when not null, must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetName(o: *mut PyObject, name: *const c_char) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_SetName") {
            return -1;
        }
        (*o.cast::<PyCapsuleObject>()).name = name;
        0
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetDestructor(
    o: *mut PyObject,
    destructor: Option<unsafe extern "C" fn(*mut PyObject)>,
) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_SetDestructor") {
            return -1;
        }
        (*o.cast::<PyCapsuleObject>()).destructor = destructor;
        0
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_SetContext(o: *mut PyObject, context: *mut c_void) -> c_int {
    // SAFETY: the arguments follow the C contract.
    unsafe {
        if !is_legal_capsule(o, "PyCapsule_SetContext") {
            return -1;
        }
        (*o.cast::<PyCapsuleObject>()).context = context;
        0
    }
}

/// See the CPython 3.12 contract for this entry point. The import system
/// this runtime fronts does not resolve module names yet, so every name
/// takes CPython's failed-import path with its exact message.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCapsule_Import(name: *const c_char, no_block: c_int) -> *mut c_void {
    let _ = no_block;
    // SAFETY: `name` is a NUL-terminated string by contract.
    let name = unsafe { crate::util::cstr(name) };
    let first = name.split('.').next().unwrap_or(name);
    tstate::set_string(
        crate::types::exc::import_error(),
        format!("PyCapsule_Import could not import module \"{first}\"").as_bytes(),
    );
    core::ptr::null_mut()
}

/// Ready the capsule type; `init_static_types` calls this once.
pub fn init() -> c_int {
    // SAFETY: init runs before any concurrent access can exist.
    unsafe {
        if crate::object::typeobject::PyType_Ready(&raw mut PyCapsule_Type) < 0 {
            return -1;
        }
        crate::gc::counted::note_coil_type(&raw mut PyCapsule_Type);
    }
    0
}

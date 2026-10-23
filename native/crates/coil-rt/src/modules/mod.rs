//! The native built-in modules of the Rust column (spec section 10.6):
//! `builtins`, `sys`, `_imp`, `_struct`, `math`, `cmath`, `_collections`,
//! `itertools`, `array`, `binascii`, `zlib`, `audioop` and `_codecs`,
//! each a C-API-shaped surface over the exact layouts and the shared
//! helpers below.
//!
//! The module object machinery every one of them — and every `.pyd` —
//! initialises through (spec section 10.4): `PyModuleDef_Init`, PEP 489
//! multi-phase init, single-phase init with `m_copy`, and the
//! `PyModule_*` exports.
//!
//! Keyword arguments travel the same seam: a non-empty keyword dict
//! raises exactly CPython's "keywords must be strings", the answer it
//! gives non-str keys.

pub mod _codecs;
pub mod binascii;
pub mod builtins;
pub mod cmath;
pub mod imp;
pub mod math;
pub mod sys;

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{
    METH_CLASS, METH_STATIC, Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC,
    Py_TPFLAGS_READY, PyMethodDef, PyModuleDef, PyObject, PyTypeObject, c_long,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::zero_type;
use crate::tstate;
use crate::types::exc::{self as exceptions, PyErr_Format};
use crate::types::{dict, long, unicode};

/// A module's function table. The raw pointers inside `PyMethodDef`
/// make it non-`Sync` by construction, but every entry points at static
/// strings and functions, so the tables can live in statics.
pub struct MethodTable(pub &'static [coil_capi_sys::PyMethodDef]);
// SAFETY: the name strings and function pointers are process-static.
unsafe impl Sync for MethodTable {}

/// CPython 3.12's `PyModuleObject` (pycore_moduleobject.h): a header, the
/// attribute dict, the defining `PyModuleDef` for extension modules, the
/// per-module state `m_size` bytes of it, the weakref list and the name
/// kept for logging after the dict is cleared.
#[repr(C)]
pub struct PyModuleObject {
    pub ob_base: PyObject,
    pub md_dict: *mut PyObject,
    pub md_def: *mut PyModuleDef,
    pub md_state: *mut c_void,
    pub md_weaklist: *mut PyObject,
    pub md_name: *mut PyObject,
}

const fn module_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"module".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyModuleObject>() as isize;
    t.tp_dealloc = Some(module_dealloc);
    t.tp_repr = Some(module_repr);
    t.tp_getattro = Some(module_getattro);
    t.tp_setattro = Some(module_setattro);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
    t.tp_traverse = Some(module_traverse);
    t.tp_clear = Some(module_clear);
    t.tp_weaklistoffset = core::mem::offset_of!(PyModuleObject, md_weaklist) as isize;
    t.tp_dictoffset = core::mem::offset_of!(PyModuleObject, md_dict) as isize;
    t.tp_alloc = Some(crate::object::typeobject::PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

const fn moduledef_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"moduledef".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyModuleDef>() as isize;
    t
}

/// The `module` type object (CPython's `PyModule_Type`).
#[unsafe(no_mangle)]
pub static mut PyModule_Type: PyTypeObject = module_type();

/// The `moduledef` type object (CPython's `PyModuleDef_Type`).
#[unsafe(no_mangle)]
pub static mut PyModuleDef_Type: PyTypeObject = moduledef_type();

/// The module types in dependency order for `init_static_types`.
pub fn static_types() -> [*mut PyTypeObject; 2] {
    [&raw mut PyModule_Type, &raw mut PyModuleDef_Type]
}

/// The `moduledef` type, for the multi-phase init result check.
pub fn module_def_type() -> *mut PyTypeObject {
    &raw mut PyModuleDef_Type
}

/// `PyModule_Check`: exact type or subtype of `module`.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
pub unsafe fn is_module(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object; the MRO walk is over
    // readied types.
    unsafe {
        crate::object::typeobject::PyType_IsSubtype(header::type_of(op), &raw mut PyModule_Type)
            != 0
    }
}

/// `PyUnicode_CheckExact` by address, the way the container types check.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
unsafe fn is_str_exact(op: *const PyObject) -> bool {
    // SAFETY: a header read within the object.
    !op.is_null()
        && core::ptr::eq(
            unsafe { header::type_of(op) },
            &raw mut crate::types::unicode::PyUnicode_Type,
        )
}

/// `_PyModule_IsExtension` (pycore_moduleobject.h): a module whose def
/// carries methods.
///
/// # Safety
///
/// `obj` must be null or a valid `PyObject`.
pub unsafe fn is_extension(obj: *const PyObject) -> bool {
    // SAFETY: the def read follows the module type check.
    unsafe { is_module(obj) && !(*obj.cast::<PyModuleObject>()).md_def.is_null() }
}

// ------------------------------------------------------------- the C API

/// See the CPython 3.12 contract for this entry point; the def becomes an
/// object of `PyModuleDef_Type` and receives its index in the module
/// table, and joins the collector's roots for its `m_copy`.
///
/// # Safety
///
/// `def` must point to a statically allocated `PyModuleDef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModuleDef_Init(def: *mut PyModuleDef) -> *mut PyObject {
    if def.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the caller guarantees a static def; `PyModuleDef_Type` is
    // readied by init.
    unsafe {
        if (*def).m_base.m_index == 0 {
            (*def).m_base.ob_base.ob_refcnt = 1;
            (*def).m_base.ob_base.ob_type = &raw mut PyModuleDef_Type;
            (*def).m_base.m_index = crate::import::next_module_index();
        }
    }
    crate::gc::roots::register_module_def(def);
    def.cast()
}

/// `module_init_dict` (moduleobject.c): the attributes every fresh module
/// starts with. False with the error set.
///
/// # Safety
///
/// `md` must be a module whose dict exists; `name`/`doc` valid objects or
/// null.
unsafe fn init_dict(md: *mut PyModuleObject, name: *mut PyObject, doc: *mut PyObject) -> bool {
    // SAFETY: `md` is a module whose dict exists.
    let dict = unsafe { (*md).md_dict };
    let doc = if doc.is_null() {
        crate::object::singletons::none()
    } else {
        doc
    };
    // SAFETY: the dict is a fresh dict and the values valid objects.
    unsafe {
        for (key, value) in [
            (c"__name__".as_ptr(), name),
            (c"__doc__".as_ptr(), doc),
            (c"__package__".as_ptr(), crate::object::singletons::none()),
            (c"__loader__".as_ptr(), crate::object::singletons::none()),
            (c"__spec__".as_ptr(), crate::object::singletons::none()),
        ] {
            if dict::PyDict_SetItemString(dict, key, value) < 0 {
                return false;
            }
        }
        if is_str_exact(name) && (*md).md_name.is_null() {
            (*md).md_name = name;
            crate::gc::counted::Py_IncRef(name);
        }
    }
    true
}

/// A zeroed module with a fresh dict, or null with `MemoryError` set.
fn new_module_notrack() -> *mut PyModuleObject {
    // SAFETY: `PyModule_Type` is readied by init and its instances are
    // tracing-regime objects.
    let module = unsafe { crate::gc::counted::_PyObject_New(&raw mut PyModule_Type) };
    if module.is_null() {
        return core::ptr::null_mut();
    }
    let md = module.cast::<PyModuleObject>();
    // SAFETY: the fresh block is zeroed and exclusively ours.
    unsafe {
        (*md).md_dict = dict::PyDict_New();
        if (*md).md_dict.is_null() {
            crate::alloc::pymem::PyObject_Free(module.cast());
            return core::ptr::null_mut();
        }
    }
    md
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name` must be a valid object or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_NewObject(name: *mut PyObject) -> *mut PyObject {
    if name.is_null() {
        tstate::set_string(exceptions::system_error(), b"empty module name");
        return core::ptr::null_mut();
    }
    let md = new_module_notrack();
    if md.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh module owns its dict; `name` is a valid object.
    unsafe {
        if !init_dict(md, name, core::ptr::null_mut()) {
            module_dealloc(md.cast());
            return core::ptr::null_mut();
        }
    }
    md.cast()
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_New(name: *const c_char) -> *mut PyObject {
    // SAFETY: `name` is a NUL-terminated string by contract.
    let name = unsafe { unicode::PyUnicode_FromString(name) };
    if name.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `name` is a valid object.
    let module = unsafe { PyModule_NewObject(name) };
    // SAFETY: a new reference we own.
    unsafe { crate::gc::counted::Py_DecRef(name) };
    module
}

/// `check_api_version` (moduleobject.c): CPython warns on mismatch and
/// loads anyway; the warning machinery is a later task, so every version
/// is accepted the way a heeded warning would be.
fn check_api_version(_name: &str, _module_api_version: c_int) -> bool {
    true
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must point to a statically allocated `PyModuleDef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_Create2(
    module: *mut PyModuleDef,
    module_api_version: c_int,
) -> *mut PyObject {
    if !crate::import::is_initialized() {
        tstate::set_string(
            exceptions::system_error(),
            b"Python import machinery not initialized",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the caller's contract.
    unsafe { _PyModule_CreateInitialized(module, module_api_version) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must point to a statically allocated `PyModuleDef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyModule_CreateInitialized(
    module: *mut PyModuleDef,
    module_api_version: c_int,
) -> *mut PyObject {
    // SAFETY: the caller's contract; a null return needs an error this
    // path cannot leave unset.
    unsafe {
        if PyModuleDef_Init(module).is_null() {
            return core::ptr::null_mut();
        }
        let name = crate::util::cstr((*module).m_name);
        if !check_api_version(name, module_api_version) {
            return core::ptr::null_mut();
        }
        if !(*module).m_slots.is_null() {
            PyErr_Format(
                exceptions::system_error(),
                c"module %s: PyModule_Create is incompatible with m_slots".as_ptr(),
                (*module).m_name as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            return core::ptr::null_mut();
        }
        let m = PyModule_New((*module).m_name);
        if m.is_null() {
            return core::ptr::null_mut();
        }
        let md = m.cast::<PyModuleObject>();
        if (*module).m_size > 0 {
            (*md).md_state = crate::alloc::pymem::PyMem_Malloc((*module).m_size as usize);
            if (*md).md_state.is_null() {
                tstate::set_string(exceptions::memory_error(), b"");
                module_dealloc(m);
                return core::ptr::null_mut();
            }
            core::ptr::write_bytes((*md).md_state.cast::<u8>(), 0, (*module).m_size as usize);
        }
        if !(*module).m_methods.is_null() && PyModule_AddFunctions(m, (*module).m_methods) < 0 {
            module_dealloc(m);
            return core::ptr::null_mut();
        }
        if !(*module).m_doc.is_null() && PyModule_SetDocString(m, (*module).m_doc) < 0 {
            module_dealloc(m);
            return core::ptr::null_mut();
        }
        (*md).md_def = module;
        m
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `def` must point to a statically allocated `PyModuleDef`; `spec` must
/// be a valid object whose `name` attribute is a string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_FromDefAndSpec2(
    def: *mut PyModuleDef,
    spec: *mut PyObject,
    module_api_version: c_int,
) -> *mut PyObject {
    // SAFETY: a static def.
    unsafe { PyModuleDef_Init(def) };
    // SAFETY: `spec` is a valid object.
    let nameobj = unsafe { crate::object::attr::get_attr_string(spec, c"name".as_ptr()) };
    if nameobj.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the whole body guards `def` and `nameobj`.
    unsafe {
        if !is_str_exact(nameobj) {
            tstate::set_string(exceptions::type_error(), b"spec.name must be a string");
            crate::gc::counted::Py_DecRef(nameobj);
            return core::ptr::null_mut();
        }
        let m = from_def_and_spec(def, spec, nameobj, module_api_version);
        crate::gc::counted::Py_DecRef(nameobj);
        m
    }
}

/// `PyModule_FromDefAndSpec2`'s body after `spec.name`: slot validation,
/// `Py_mod_create`, the def install and the method/doc fill.
///
/// # Safety
///
/// `def` static, `spec` a valid object, `nameobj` an exact string.
unsafe fn from_def_and_spec(
    def: *mut PyModuleDef,
    spec: *mut PyObject,
    nameobj: *mut PyObject,
    module_api_version: c_int,
) -> *mut PyObject {
    type Create = unsafe extern "C" fn(*mut PyObject, *mut PyModuleDef) -> *mut PyObject;
    let mut create: Option<Create> = None;
    let mut has_execution_slots = false;
    // SAFETY: the def is static and the name a valid string.
    unsafe {
        let name = crate::util::cstr(unicode::PyUnicode_AsUTF8(nameobj));
        if !check_api_version(name, module_api_version) {
            return core::ptr::null_mut();
        }
        if (*def).m_size < 0 {
            PyErr_Format(
                exceptions::system_error(),
                c"module %s: m_size may not be negative for multi-phase initialization".as_ptr(),
                (*def).m_name as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            return core::ptr::null_mut();
        }
        let mut cur_slot = (*def).m_slots;
        while !cur_slot.is_null() && (*cur_slot).slot != 0 {
            match (*cur_slot).slot {
                coil_capi_sys::Py_mod_create => {
                    if create.is_some() {
                        PyErr_Format(
                            exceptions::system_error(),
                            c"module %s has multiple create slots".as_ptr(),
                            (*def).m_name as u64,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                        );
                        return core::ptr::null_mut();
                    }
                    create = Some(core::mem::transmute::<*mut c_void, Create>(
                        (*cur_slot).value,
                    ));
                }
                coil_capi_sys::Py_mod_exec => has_execution_slots = true,
                coil_capi_sys::Py_mod_multiple_interpreters => {}
                _ => {
                    PyErr_Format(
                        exceptions::system_error(),
                        c"module %s uses unknown slot ID %i".as_ptr(),
                        (*def).m_name as u64,
                        (*cur_slot).slot as u64,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                    );
                    return core::ptr::null_mut();
                }
            }
            cur_slot = cur_slot.add(1);
        }

        let m = match create {
            Some(create) => create(spec, def),
            None => PyModule_NewObject(nameobj),
        };
        if m.is_null() {
            if tstate::occurred().is_null() {
                PyErr_Format(
                    exceptions::system_error(),
                    c"creation of module %s failed without setting an exception".as_ptr(),
                    (*def).m_name as u64,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                );
            }
            return core::ptr::null_mut();
        }
        if !tstate::occurred().is_null() {
            PyErr_Format(
                exceptions::system_error(),
                c"creation of module %s raised unreported exception".as_ptr(),
                (*def).m_name as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            crate::gc::counted::Py_DecRef(m);
            return core::ptr::null_mut();
        }

        if is_module(m) {
            let md = m.cast::<PyModuleObject>();
            (*md).md_state = core::ptr::null_mut();
            (*md).md_def = def;
        } else if (*def).m_size > 0
            || (*def).m_traverse.is_some()
            || (*def).m_clear.is_some()
            || (*def).m_free.is_some()
        {
            PyErr_Format(
                exceptions::system_error(),
                c"module %s is not a module object, but requests module state".as_ptr(),
                (*def).m_name as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            crate::gc::counted::Py_DecRef(m);
            return core::ptr::null_mut();
        } else if has_execution_slots {
            PyErr_Format(
                exceptions::system_error(),
                c"module %s specifies execution slots, but did not create a ModuleType instance"
                    .as_ptr(),
                (*def).m_name as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            crate::gc::counted::Py_DecRef(m);
            return core::ptr::null_mut();
        }

        if !(*def).m_methods.is_null() && add_methods_to_object(m, nameobj, (*def).m_methods) < 0 {
            crate::gc::counted::Py_DecRef(m);
            return core::ptr::null_mut();
        }
        if !(*def).m_doc.is_null() && PyModule_SetDocString(m, (*def).m_doc) < 0 {
            crate::gc::counted::Py_DecRef(m);
            return core::ptr::null_mut();
        }
        m
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must be a module whose def carries it; `def` static.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_ExecDef(module: *mut PyObject, def: *mut PyModuleDef) -> c_int {
    // SAFETY: the module contract.
    let name = unsafe { PyModule_GetName(module) };
    if name.is_null() {
        return -1;
    }
    // SAFETY: the whole body guards `def` and the module fields.
    unsafe {
        if (*def).m_size >= 0 {
            let md = module.cast::<PyModuleObject>();
            if (*md).md_state.is_null() {
                // Always set a state pointer; this serves as a marker to
                // skip multiple initialization (moduleobject.c).
                (*md).md_state = crate::alloc::pymem::PyMem_Malloc((*def).m_size as usize);
                if (*md).md_state.is_null() {
                    tstate::set_string(exceptions::memory_error(), b"");
                    return -1;
                }
                core::ptr::write_bytes((*md).md_state.cast::<u8>(), 0, (*def).m_size as usize);
            }
        }
        if (*def).m_slots.is_null() {
            return 0;
        }
        type Exec = unsafe extern "C" fn(*mut PyObject) -> c_int;
        let mut cur_slot = (*def).m_slots;
        while !cur_slot.is_null() && (*cur_slot).slot != 0 {
            match (*cur_slot).slot {
                coil_capi_sys::Py_mod_create | coil_capi_sys::Py_mod_multiple_interpreters => {}
                coil_capi_sys::Py_mod_exec => {
                    let exec = core::mem::transmute::<*mut c_void, Exec>((*cur_slot).value);
                    let ret = exec(module);
                    if ret != 0 {
                        if tstate::occurred().is_null() {
                            PyErr_Format(
                                exceptions::system_error(),
                                c"execution of module %s failed without setting an exception"
                                    .as_ptr(),
                                (*def).m_name as u64,
                                0,
                                0,
                                0,
                                0,
                                0,
                                0,
                                0,
                            );
                        }
                        return -1;
                    }
                    if !tstate::occurred().is_null() {
                        PyErr_Format(
                            exceptions::system_error(),
                            c"execution of module %s raised unreported exception".as_ptr(),
                            (*def).m_name as u64,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                            0,
                        );
                        return -1;
                    }
                }
                _ => {
                    PyErr_Format(
                        exceptions::system_error(),
                        c"module %s initialized with unknown slot %i".as_ptr(),
                        (*def).m_name as u64,
                        (*cur_slot).slot as u64,
                        0,
                        0,
                        0,
                        0,
                        0,
                        0,
                    );
                    return -1;
                }
            }
            cur_slot = cur_slot.add(1);
        }
    }
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module; `functions` a static, null-name-terminated
/// `PyMethodDef` array.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddFunctions(
    m: *mut PyObject,
    functions: *mut PyMethodDef,
) -> c_int {
    // SAFETY: the module contract.
    let name = unsafe { PyModule_GetNameObject(m) };
    if name.is_null() {
        return -1;
    }
    // SAFETY: the array contract.
    let res = unsafe { add_methods_to_object(m, name, functions) };
    // SAFETY: our own new reference.
    unsafe { crate::gc::counted::Py_DecRef(name) };
    res
}

/// `_add_methods_to_object` (moduleobject.c): wrap each def in a
/// `builtin_function_or_method` bound to the module and publish it as an
/// attribute.
///
/// # Safety
///
/// `module` must be a valid object accepting attributes, `name` a string
/// and `functions` a static terminated array.
unsafe fn add_methods_to_object(
    module: *mut PyObject,
    name: *mut PyObject,
    functions: *mut PyMethodDef,
) -> c_int {
    unsafe {
        let mut fdef = functions;
        while !(*fdef).ml_name.is_null() {
            if (*fdef).ml_flags & (METH_CLASS | METH_STATIC) != 0 {
                tstate::set_string(
                    exceptions::value_error(),
                    b"module functions cannot set METH_CLASS or METH_STATIC",
                );
                return -1;
            }
            let func = crate::types::pycfunction::PyCFunction_NewEx(fdef, module, name);
            if func.is_null() {
                return -1;
            }
            if crate::object::attr::set_attr_string(module, (*fdef).ml_name, func) < 0 {
                crate::gc::counted::Py_DecRef(func);
                return -1;
            }
            crate::gc::counted::Py_DecRef(func);
            fdef = fdef.add(1);
        }
    }
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module and `value` a valid non-null object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddObjectRef(
    m: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the contracts above.
    unsafe {
        if !is_module(m) {
            tstate::set_string(
                exceptions::type_error(),
                b"PyModule_AddObjectRef() first argument must be a module",
            );
            return -1;
        }
        if value.is_null() {
            if tstate::occurred().is_null() {
                tstate::set_string(
                    exceptions::system_error(),
                    b"PyModule_AddObjectRef() must be called with an exception raised if value is NULL",
                );
            }
            return -1;
        }
        let dict = PyModule_GetDict(m);
        if dict.is_null() {
            PyErr_Format(
                exceptions::system_error(),
                c"module '%s' has no __dict__".as_ptr(),
                PyModule_GetName(m) as u64,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
            );
            return -1;
        }
        dict::PyDict_SetItemString(dict, name, value)
    }
}

/// See the CPython 3.12 contract for this entry point; steals `value` on
/// success.
///
/// # Safety
///
/// As `PyModule_AddObjectRef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddObject(
    m: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the contracts above.
    unsafe {
        let res = PyModule_AddObjectRef(m, name, value);
        if res == 0 {
            crate::gc::counted::Py_DecRef(value);
        }
        res
    }
}

/// `_PyModule_Add` (pycore_modsupport.h): decrefs `value` either way.
///
/// # Safety
///
/// As `PyModule_AddObjectRef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyModule_Add(
    m: *mut PyObject,
    name: *const c_char,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the contracts above.
    unsafe {
        let res = PyModule_AddObjectRef(m, name, value);
        if !value.is_null() {
            crate::gc::counted::Py_DecRef(value);
        }
        res
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddIntConstant(
    m: *mut PyObject,
    name: *const c_char,
    value: c_long,
) -> c_int {
    // SAFETY: the module contract; `PyLong_FromLong` returns a new
    // reference or null with the error set.
    unsafe { _PyModule_Add(m, name, long::PyLong_FromLong(value)) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module; `value` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddStringConstant(
    m: *mut PyObject,
    name: *const c_char,
    value: *const c_char,
) -> c_int {
    // SAFETY: the contracts above.
    unsafe { _PyModule_Add(m, name, unicode::PyUnicode_FromString(value)) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must be a module; `type_` a type object (readied or not).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_AddType(
    module: *mut PyObject,
    type_: *mut PyTypeObject,
) -> c_int {
    // SAFETY: the type contract.
    unsafe {
        if (*type_).tp_flags & Py_TPFLAGS_READY == 0
            && crate::object::typeobject::PyType_Ready(type_) < 0
        {
            return -1;
        }
        // `_PyType_Name`: the last component of `tp_name`.
        let full = crate::util::cstr((*type_).tp_name);
        let short = full.rsplit('.').next().unwrap_or(full);
        let mut buffer = [0u8; 256];
        if short.len() >= buffer.len() {
            tstate::set_string(
                exceptions::system_error(),
                b"PyModule_AddType: type name too long",
            );
            return -1;
        }
        buffer[..short.len()].copy_from_slice(short.as_bytes());
        PyModule_AddObjectRef(module, buffer.as_ptr().cast(), type_.cast())
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module; `doc` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_SetDocString(m: *mut PyObject, doc: *const c_char) -> c_int {
    // SAFETY: the contracts above.
    unsafe {
        let v = unicode::PyUnicode_FromString(doc);
        if v.is_null() {
            return -1;
        }
        let res = crate::object::attr::set_attr_string(m, c"__doc__".as_ptr(), v);
        crate::gc::counted::Py_DecRef(v);
        res
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetDict(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: the module contract.
    if unsafe { !is_module(m) } {
        tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the dict read follows the type check.
    unsafe { (*m.cast::<PyModuleObject>()).md_dict }
}

/// `PyDict_GetItemWithError` on the module dict for a C string key; null
/// when the module has no dict or the key is absent, the error left in
/// place.
///
/// # Safety
///
/// `m` must be a module; `key` a NUL-terminated string.
unsafe fn module_dict_entry(m: *mut PyObject, key: *const c_char) -> *mut PyObject {
    // SAFETY: the C string contract.
    let key = unsafe { unicode::PyUnicode_InternFromString(key) };
    if key.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the module contract.
    let dict = unsafe { PyModule_GetDict(m) };
    let value = if dict.is_null() {
        core::ptr::null_mut()
    } else {
        // SAFETY: the dict and key contracts.
        unsafe { dict::PyDict_GetItemWithError(dict, key) }
    };
    // SAFETY: our own interned reference.
    unsafe { crate::gc::counted::Py_DecRef(key) };
    value
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetNameObject(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: the contracts above.
    unsafe {
        if !is_module(m) {
            set_bad_argument();
            return core::ptr::null_mut();
        }
        let name = module_dict_entry(m, c"__name__".as_ptr());
        if name.is_null() || !is_str_exact(name) {
            if tstate::occurred().is_null() {
                tstate::set_string(exceptions::system_error(), b"nameless module");
            }
            return core::ptr::null_mut();
        }
        crate::gc::counted::Py_IncRef(name);
        name
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetName(m: *mut PyObject) -> *const c_char {
    // SAFETY: the module contract.
    let name = unsafe { PyModule_GetNameObject(m) };
    if name.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the module dict still holds the name, so the UTF-8 form
    // outlives the decref, exactly CPython's borrowing argument.
    let utf8 = unsafe { unicode::PyUnicode_AsUTF8(name) };
    // SAFETY: our own reference.
    unsafe { crate::gc::counted::Py_DecRef(name) };
    utf8
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetFilenameObject(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: the contracts above.
    unsafe {
        if !is_module(m) {
            set_bad_argument();
            return core::ptr::null_mut();
        }
        let fileobj = module_dict_entry(m, c"__file__".as_ptr());
        if fileobj.is_null() || !is_str_exact(fileobj) {
            if tstate::occurred().is_null() {
                tstate::set_string(exceptions::system_error(), b"module filename missing");
            }
            return core::ptr::null_mut();
        }
        crate::gc::counted::Py_IncRef(fileobj);
        fileobj
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetFilename(m: *mut PyObject) -> *const c_char {
    // SAFETY: the module contract.
    let fileobj = unsafe { PyModule_GetFilenameObject(m) };
    if fileobj.is_null() {
        return core::ptr::null();
    }
    // SAFETY: the module dict still holds the file name.
    let utf8 = unsafe { unicode::PyUnicode_AsUTF8(fileobj) };
    // SAFETY: our own reference.
    unsafe { crate::gc::counted::Py_DecRef(fileobj) };
    utf8
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetDef(m: *mut PyObject) -> *mut PyModuleDef {
    // SAFETY: the module contract.
    if unsafe { !is_module(m) } {
        set_bad_argument();
        return core::ptr::null_mut();
    }
    // SAFETY: the def read follows the type check.
    unsafe { (*m.cast::<PyModuleObject>()).md_def }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyModule_GetState(m: *mut PyObject) -> *mut c_void {
    // SAFETY: the module contract.
    if unsafe { !is_module(m) } {
        set_bad_argument();
        return core::ptr::null_mut();
    }
    // SAFETY: the state read follows the type check.
    unsafe { (*m.cast::<PyModuleObject>()).md_state }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `m` must be a module.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyModule_Clear(m: *mut PyObject) {
    // SAFETY: the module contract.
    let d = unsafe { (*m.cast::<PyModuleObject>()).md_dict };
    if !d.is_null() {
        // SAFETY: the dict contract.
        unsafe { _PyModule_ClearDict(d) };
    }
}

/// See the CPython 3.12 contract for this entry point: names with a
/// single leading underscore are zapped first, then everything but
/// `__builtins__`, both by overwriting with `None` rather than deleting,
/// so destructors run in a predictable order.
///
/// # Safety
///
/// `d` must be a dict.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyModule_ClearDict(d: *mut PyObject) {
    // SAFETY: the dict iteration and stores follow the dict contracts.
    unsafe {
        let none = crate::object::singletons::none();
        let mut pos: isize = 0;
        let mut key: *mut PyObject = core::ptr::null_mut();
        let mut value: *mut PyObject = core::ptr::null_mut();
        while dict::PyDict_Next(d, &mut pos, &mut key, &mut value) != 0 {
            if !core::ptr::eq(value, none)
                && is_str_exact(key)
                && starts_single_underscore(key)
                && dict::PyDict_SetItem(d, key, none) < 0
            {
                crate::types::exc::PyErr_WriteUnraisable(core::ptr::null_mut());
            }
        }
        let mut pos: isize = 0;
        while dict::PyDict_Next(d, &mut pos, &mut key, &mut value) != 0 {
            if !core::ptr::eq(value, none)
                && is_str_exact(key)
                && !starts_double_underscore(key, b"__builtins__")
                && dict::PyDict_SetItem(d, key, none) < 0
            {
                crate::types::exc::PyErr_WriteUnraisable(core::ptr::null_mut());
            }
        }
    }
}

/// Whether the string's first character is `_` and its second is not `_`
/// (`PyUnicode_READ_CHAR(key, 0) == '_' && PyUnicode_READ_CHAR(key, 1) != '_'`).
///
/// # Safety
///
/// `key` must be an exact string.
unsafe fn starts_single_underscore(key: *mut PyObject) -> bool {
    // SAFETY: the reads stay within the string's data.
    unsafe {
        let length = unicode::get_length(key);
        if length < 1 {
            return false;
        }
        let chars = unicode::data_u8(key);
        *chars == b'_' && (length < 2 || *chars.add(1) != b'_')
    }
}

/// Whether the string does not equal `name`: the second `_PyModule_ClearDict`
/// pass clears every name except `__builtins__`.
///
/// # Safety
///
/// `key` must be an exact string.
unsafe fn starts_double_underscore(key: *mut PyObject, name: &[u8]) -> bool {
    // SAFETY: the slice stays within the string's data.
    unsafe {
        let length = unicode::get_length(key);
        if length != name.len() as isize {
            return true;
        }
        core::slice::from_raw_parts(unicode::data_u8(key), name.len()) != name
    }
}

/// `_PyModuleSpec_IsInitializing`: `spec._initializing` true or false;
/// absent, null spec or unreadable being false.
///
/// # Safety
///
/// `spec` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyModuleSpec_IsInitializing(spec: *mut PyObject) -> c_int {
    if spec.is_null() {
        return 0;
    }
    // SAFETY: `spec` is a valid object.
    unsafe {
        let value = crate::object::attr::get_attr_string(spec, c"_initializing".as_ptr());
        if value.is_null() {
            tstate::clear();
            return 0;
        }
        let initializing = bool_value(value);
        crate::gc::counted::Py_DecRef(value);
        initializing
    }
}

// ------------------------------------------------- per-interpreter state

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must be a module carrying `def`; `def` must have been through
/// `PyModuleDef_Init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyState_AddModule(module: *mut PyObject, def: *mut PyModuleDef) -> c_int {
    // SAFETY: the contracts above.
    unsafe {
        if def.is_null() {
            fatal("PyState_AddModule: module definition is NULL");
        }
        if !(*def).m_slots.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"PyState_AddModule called on module with slots",
            );
            return -1;
        }
        crate::import::modules_by_index_set(def, module)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// As `PyState_AddModule`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyState_RemoveModule(def: *mut PyModuleDef) -> c_int {
    // SAFETY: the contracts above.
    unsafe {
        if !(*def).m_slots.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"PyState_RemoveModule called on module with slots",
            );
            return -1;
        }
        crate::import::modules_by_index_clear(def)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must have been through `PyModuleDef_Init`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyState_FindModule(module: *mut PyModuleDef) -> *mut PyObject {
    // SAFETY: the contract above.
    unsafe {
        if !(*module).m_slots.is_null() {
            return core::ptr::null_mut();
        }
        crate::import::modules_by_index_get(module)
    }
}

/// `_PyState_AddModule`, the pre-3.7 signature kept for old stable-ABI
/// extensions; the thread state it used to take is this runtime's only
/// one.
///
/// # Safety
///
/// As `PyState_AddModule`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyState_AddModule(
    _tstate: *mut core::ffi::c_void,
    module: *mut PyObject,
    def: *mut PyModuleDef,
) -> c_int {
    // SAFETY: as `PyState_AddModule`.
    unsafe { PyState_AddModule(module, def) }
}

// ------------------------------------------------------- slots and helpers

/// `module_dealloc` (moduleobject.c).
///
/// # Safety
///
/// `m` must be a module whose last reference just dropped.
unsafe extern "C" fn module_dealloc(m: *mut PyObject) {
    // SAFETY: the whole body reads only the dying module.
    unsafe {
        let md = m.cast::<PyModuleObject>();
        let def = (*md).md_def;
        if !def.is_null()
            && (*def).m_free.is_some()
            && ((*def).m_size <= 0 || !(*md).md_state.is_null())
        {
            // bpo-39824: don't call m_free() if m_size > 0 and
            // md_state=NULL.
            (*def).m_free.unwrap()(md.cast());
        }
        if !(*md).md_dict.is_null() {
            crate::gc::counted::Py_DecRef((*md).md_dict);
        }
        if !(*md).md_name.is_null() {
            crate::gc::counted::Py_DecRef((*md).md_name);
        }
        if !(*md).md_state.is_null() {
            crate::alloc::pymem::PyMem_Free((*md).md_state);
        }
        crate::alloc::pymem::PyObject_Free(m as *mut core::ffi::c_void);
    }
}

/// `module_traverse` (moduleobject.c): the def's `m_traverse` when the
/// module has state, then the dict.
///
/// # Safety
///
/// `m` must be a module; `visit`/`arg` the collector's pair.
unsafe extern "C" fn module_traverse(
    m: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the whole body reads only a live module.
    unsafe {
        let md = m.cast::<PyModuleObject>();
        let def = (*md).md_def;
        if !def.is_null()
            && let Some(traverse) = (*def).m_traverse
            && ((*def).m_size <= 0 || !(*md).md_state.is_null())
        {
            let res = traverse(m, visit, arg);
            if res != 0 {
                return res;
            }
        }
        if !(*md).md_dict.is_null() {
            let res = visit((*md).md_dict, arg);
            if res != 0 {
                return res;
            }
        }
    }
    0
}

/// `module_clear` (moduleobject.c).
///
/// # Safety
///
/// `m` must be a live module.
unsafe extern "C" fn module_clear(m: *mut PyObject) -> c_int {
    // SAFETY: the whole body reads and writes only a live module.
    unsafe {
        let md = m.cast::<PyModuleObject>();
        let def = (*md).md_def;
        if !def.is_null()
            && let Some(clear) = (*def).m_clear
            && ((*def).m_size <= 0 || !(*md).md_state.is_null())
        {
            let res = clear(m);
            if res != 0 {
                return res;
            }
        }
        if !(*md).md_dict.is_null() {
            let dict = (*md).md_dict;
            (*md).md_dict = core::ptr::null_mut();
            crate::gc::counted::Py_DecRef(dict);
        }
    }
    0
}

/// `module_repr`: CPython asks importlib for the repr; importlib is not
/// compiled yet, so this falls back to the shape a fully-initialised
/// importlib gives a built-in module.
///
/// # Safety
///
/// `m` must be a module.
unsafe extern "C" fn module_repr(m: *mut PyObject) -> *mut PyObject {
    // SAFETY: the name read follows the module type check.
    unsafe {
        let name = PyModule_GetNameObject(m);
        if name.is_null() {
            return core::ptr::null_mut();
        }
        let text = format!(
            "<module '{}'>",
            crate::util::cstr(unicode::PyUnicode_AsUTF8(name))
        );
        crate::gc::counted::Py_DecRef(name);
        let mut bytes = text.into_bytes();
        bytes.push(0);
        unicode::PyUnicode_FromString(bytes.as_ptr().cast())
    }
}

/// `module_getattro`: the instance dict lookup with the `__getattr__`
/// hook and CPython's module-scoped `AttributeError`.
///
/// # Safety
///
/// `m` must be a module; `name` a string.
unsafe extern "C" fn module_getattro(m: *mut PyObject, name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the whole body reads the module and its dict.
    unsafe {
        let md = m.cast::<PyModuleObject>();
        if unicode::_PyUnicode_EqualToASCIIString(name, c"__dict__".as_ptr()) != 0 {
            if (*md).md_dict.is_null() {
                tstate::set_string(exceptions::attribute_error(), b"__dict__");
                return core::ptr::null_mut();
            }
            crate::gc::counted::Py_IncRef((*md).md_dict);
            return (*md).md_dict;
        }
        let attr = if (*md).md_dict.is_null() {
            core::ptr::null_mut()
        } else {
            dict::PyDict_GetItemWithError((*md).md_dict, name)
        };
        if !attr.is_null() {
            crate::gc::counted::Py_IncRef(attr);
            return attr;
        }
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let getattr = if (*md).md_dict.is_null() {
            core::ptr::null_mut()
        } else {
            module_dict_entry(m, c"__getattr__".as_ptr())
        };
        if !getattr.is_null() {
            return call_one_arg(getattr, name);
        }
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let mod_name = if (*md).md_dict.is_null() {
            core::ptr::null()
        } else {
            unicode::PyUnicode_AsUTF8(module_dict_entry(m, c"__name__".as_ptr()))
        };
        PyErr_Format(
            exceptions::attribute_error(),
            c"module '%s' has no attribute '%s'".as_ptr(),
            mod_name as u64,
            unicode::PyUnicode_AsUTF8(name) as u64,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        core::ptr::null_mut()
    }
}

/// `module_setattro`: `PyObject_GenericSetAttr` on a module resolves to
/// the instance dict at `tp_dictoffset`; `__dict__` itself is the one
/// readonly member.
///
/// # Safety
///
/// `m` must be a module; `name` a string; `value` a valid object or null.
unsafe extern "C" fn module_setattro(
    m: *mut PyObject,
    name: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the dict store follows the module type check.
    unsafe {
        let md = m.cast::<PyModuleObject>();
        if unicode::_PyUnicode_EqualToASCIIString(name, c"__dict__".as_ptr()) != 0 {
            tstate::set_string(exceptions::type_error(), b"readonly attribute");
            return -1;
        }
        if (*md).md_dict.is_null() {
            tstate::set_string(exceptions::system_error(), b"nameless module");
            return -1;
        }
        if value.is_null() {
            if dict::PyDict_DelItem((*md).md_dict, name) < 0
                && crate::types::exc::PyErr_ExceptionMatches(exceptions::key_error()) != 0
            {
                tstate::clear();
                tstate::set_string(exceptions::attribute_error(), b"attribute");
            }
            return -1;
        }
        dict::PyDict_SetItem((*md).md_dict, name, value)
    }
}

/// The `PyErr_BadArgument` TypeError (errors.c).
fn set_bad_argument() {
    tstate::set_string(
        exceptions::type_error(),
        b"bad argument type for built-in operation",
    );
}

/// `Py_FatalError`: the message and the abort CPython pairs it with.
fn fatal(message: &str) -> ! {
    eprintln!("Fatal Python error: {message}");
    std::process::abort()
}

/// `PyObject_IsTrue` restricted to what `spec._initializing` can be: the
/// two singletons. The full truth protocol arrives with the object
/// protocol layer; a `_initializing` that is not a bool is a type error
/// either way.
///
/// # Safety
///
/// `value` must be a valid object.
unsafe fn bool_value(value: *mut PyObject) -> c_int {
    if core::ptr::eq(value, crate::object::singletons::true_()) {
        return 1;
    }
    if core::ptr::eq(value, crate::object::singletons::false_()) {
        return 0;
    }
    tstate::set_string(exceptions::type_error(), b"expected a bool value");
    -1
}

// ------------------------------------------------------------ call helper

/// Call `callable` with `args` positionally and null keywords, through
/// the C function dispatch or the type's `tp_call`.
///
/// # Safety
///
/// `callable` must be a valid object; `args` valid objects.
pub unsafe fn call(callable: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    // SAFETY: the header read is within the object.
    if unsafe { crate::types::pycfunction::is_cfunction(callable) } {
        // SAFETY: the vectorcall contract holds on the arguments.
        return unsafe {
            crate::types::pycfunction::vectorcall(
                callable,
                args.as_ptr(),
                args.len() as isize,
                core::ptr::null_mut(),
            )
        };
    }
    let tuple = crate::types::tuple::from_array(args);
    // SAFETY: the slot read is within the type object.
    let call_slot = unsafe { (*header::type_of(callable)).tp_call };
    let Some(call_slot) = call_slot else {
        // SAFETY: the name read is within the type object.
        let name = unsafe { crate::util::cstr((*header::type_of(callable)).tp_name) };
        tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not callable").as_bytes(),
        );
        return core::ptr::null_mut();
    };
    // SAFETY: the slot takes the callable and the argument tuple.
    let result = unsafe { call_slot(callable, tuple, core::ptr::null_mut()) };
    // The tuple was built from borrowed arguments, so the block drops
    // without touching the items.
    // SAFETY: the block was allocated above.
    unsafe { crate::alloc::free_block(tuple.cast()) };
    result
}

/// `PyObject_CallNoArgs` over the shared path.
///
/// # Safety
///
/// `callable` must be a valid object.
pub unsafe fn call_no_args(callable: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `call`.
    unsafe { call(callable, &[]) }
}

/// `PyObject_CallOneArg` over the shared path.
///
/// # Safety
///
/// `callable` and `arg` must be valid objects.
pub unsafe fn call_one_arg(callable: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `call`.
    unsafe { call(callable, &[arg]) }
}

// --------------------------------------------------------------- coercion

/// `PyLong_CheckExact`: int without subtypes (bool is not exact).
///
/// # Safety
///
/// `op` must be a valid object.
pub(crate) unsafe fn is_exact_long(op: *const PyObject) -> bool {
    // SAFETY: the header read is within the object.
    unsafe { header::type_of(op) == &raw mut crate::types::long::PyLong_Type }
}

/// `PyFloat_CheckExact`.
///
/// # Safety
///
/// `op` must be a valid object.
pub(crate) unsafe fn is_exact_float(op: *const PyObject) -> bool {
    // SAFETY: the header read is within the object.
    unsafe { header::type_of(op) == &raw mut crate::types::float_::PyFloat_Type }
}

/// `PyObject_IsTrue`'s protocol: `nb_bool`, then `mp_length`, then
/// `sq_length`, otherwise true. Returns -1 only when a slot raised.
///
/// # Safety
///
/// `obj` must be a valid object.
pub(crate) unsafe fn object_is_true(obj: *mut PyObject) -> core::ffi::c_int {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(obj) };
    // SAFETY: the slots, when present, take the object.
    unsafe {
        if let Some(table) = (*ty).tp_as_number.as_ref()
            && let Some(nb_bool) = table.nb_bool
        {
            let res = nb_bool(obj);
            return if res > 0 { 1 } else { res as core::ffi::c_int };
        }
        if let Some(table) = (*ty).tp_as_mapping.as_ref()
            && let Some(mp_length) = table.mp_length
        {
            let res = mp_length(obj);
            return if res > 0 { 1 } else { res as core::ffi::c_int };
        }
        if let Some(table) = (*ty).tp_as_sequence.as_ref()
            && let Some(sq_length) = table.sq_length
        {
            let res = sq_length(obj);
            return if res > 0 { 1 } else { res as core::ffi::c_int };
        }
    }
    1
}

/// `PyTuple_Check`.
///
/// # Safety
///
/// `op` must be a valid object.
pub(crate) unsafe fn number_index(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: the layout check reads the header.
    if unsafe { long::is_long(obj) } {
        // SAFETY: retaining the int keeps the caller's reference intact.
        unsafe { exceptions::retain_object(obj) };
        return obj;
    }
    // SAFETY: the slot read is within the type object.
    let ty = unsafe { header::type_of(obj) };
    let nb = unsafe { (*ty).tp_as_number };
    let index_slot = if nb.is_null() {
        None
    } else {
        unsafe { (*nb).nb_index }
    };
    if let Some(index_slot) = index_slot {
        // SAFETY: the slot takes the object.
        let converted = unsafe { index_slot(obj) };
        if converted.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: the layout check reads the header.
        if !unsafe { long::is_long(converted) } {
            let name = unsafe { crate::util::cstr((*ty).tp_name) };
            tstate::set_string(
                exceptions::type_error(),
                format!("__index__ returned non-int (type {name})").as_bytes(),
            );
            return core::ptr::null_mut();
        }
        return converted;
    }
    let name = unsafe { crate::util::cstr((*ty).tp_name) };
    tstate::set_string(
        exceptions::type_error(),
        format!("'{name}' object cannot be interpreted as an integer").as_bytes(),
    );
    core::ptr::null_mut()
}

/// A number binary operation over the number slots
/// (`binary_op`): the left type's slot, then the right's when it
/// answers NotImplemented, else CPython's unsupported-operand message
/// with `symbol` ("+", "*", ...) between the type names.
///
/// # Safety
///
/// `a` and `b` must be valid objects of readied types.
pub(crate) unsafe fn number_binary(
    a: *mut PyObject,
    b: *mut PyObject,
    slot: unsafe fn(*mut coil_capi_sys::PyNumberMethods) -> Option<coil_capi_sys::binaryfunc>,
    symbol: &str,
) -> *mut PyObject {
    // SAFETY: the slot reads are within the type objects.
    unsafe {
        let slot_a = slot_of(a, slot);
        if let Some(f) = slot_a {
            let result = f(a, b);
            if result.is_null() || !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
            // NotImplemented from a slot is the borrowed immortal singleton;
            // the other side gets its chance.
        }
        let slot_b = slot_of(b, slot);
        if let Some(f) = slot_b {
            let result = f(a, b);
            if result.is_null() || !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
        }
        let a_name = crate::util::cstr((*header::type_of(a)).tp_name);
        let b_name = crate::util::cstr((*header::type_of(b)).tp_name);
        tstate::set_string(
            exceptions::type_error(),
            format!("unsupported operand type(s) for {symbol}: '{a_name}' and '{b_name}'")
                .as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// Read a number slot from an object's type.
unsafe fn slot_of(
    obj: *mut PyObject,
    slot: unsafe fn(*mut coil_capi_sys::PyNumberMethods) -> Option<coil_capi_sys::binaryfunc>,
) -> Option<coil_capi_sys::binaryfunc> {
    // SAFETY: the table read follows the header.
    let nb = unsafe { (*header::type_of(obj)).tp_as_number };
    if nb.is_null() {
        None
    } else {
        // SAFETY: the table belongs to a readied type.
        unsafe { slot(nb) }
    }
}

unsafe fn nb_add(nb: *mut coil_capi_sys::PyNumberMethods) -> Option<coil_capi_sys::binaryfunc> {
    // SAFETY: the caller guarantees the table is valid.
    unsafe { (*nb).nb_add }
}

unsafe fn nb_multiply(
    nb: *mut coil_capi_sys::PyNumberMethods,
) -> Option<coil_capi_sys::binaryfunc> {
    // SAFETY: as `nb_add`.
    unsafe { (*nb).nb_multiply }
}

/// `PyNumber_Add` over the number slots.
///
/// # Safety
///
/// `a` and `b` must be valid objects of readied types.
pub(crate) unsafe fn number_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the arguments are valid objects.
    unsafe { number_binary(a, b, nb_add, "+") }
}

/// `PyNumber_Multiply` over the number slots.
///
/// # Safety
///
/// `a` and `b` must be valid objects of readied types.
pub(crate) unsafe fn number_multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the arguments are valid objects.
    unsafe { number_binary(a, b, nb_multiply, "*") }
}

/// The `__index__` protocol as CPython's argument conversion applies it
/// (`PyNumber_Index` reduced to this runtime's types): int and bool pass
/// through, a type with `nb_index` goes through the slot once, anything
/// else is "'X' object cannot be interpreted as an integer".
///
/// # Safety
///
/// `obj` must be a valid object.
pub(crate) unsafe fn reject_keywords(kwds: *mut PyObject) -> Result<(), ()> {
    // SAFETY: the dict-layout read follows the calling machinery's
    // contract on `kwds`.
    if !kwds.is_null() && unsafe { crate::util::dict_used(kwds) } > 0 {
        tstate::set_string(exceptions::type_error(), b"keywords must be strings");
        return Err(());
    }
    Ok(())
}

/// The argument-count error of CPython's positional parsing
/// (`_PyArg_CheckPositional`), with the function's name.
pub(crate) fn positional_error(name: &str, min: usize, max: usize, given: usize) {
    if min == max {
        let plural = if min == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name} expected {min} argument{plural}, got {given}").as_bytes(),
        );
    } else if given < min {
        let plural = if min == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name} expected at least {min} argument{plural}, got {given}").as_bytes(),
        );
    } else {
        let plural = if max == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name} expected at most {max} argument{plural}, got {given}").as_bytes(),
        );
    }
}

/// The count error of the keyword-capable parse path (`_PyArg_Parser`)
/// for too many positionals: required arguments can also arrive by
/// keyword there — impossible in this runtime — so the shortfall side
/// is handled by `missing_argument_error` instead.
pub(crate) fn positional_error_kw(name: &str, min: usize, max: usize, given: usize) {
    if max == 0 {
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes no positional arguments").as_bytes(),
        );
    } else {
        let qualifier = if min < max { "at most" } else { "exactly" };
        let plural = if max == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes {qualifier} {max} positional argument{plural} ({given} given)")
                .as_bytes(),
        );
    }
}

/// `_PyArg_UnpackKeywords`'s two-layer count error for too many
/// positionals: positional overflow past every parameter (keyword-only
/// ones included) reports the total, overflow past the
/// positional-or-keyword parameters reports those.
pub(crate) fn keywords_arity_error(
    name: &str,
    minpos: usize,
    maxpos: usize,
    maxargs: usize,
    given: usize,
) {
    if given > maxargs {
        let plural = if maxargs == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes at most {maxargs} argument{plural} ({given} given)").as_bytes(),
        );
    } else {
        positional_error_kw(name, minpos, maxpos, given);
    }
}

/// `_PyArg_Parser`'s message for a missing positional-or-keyword
/// parameter (it could still arrive by name in CPython).
pub(crate) fn missing_argument_error(name: &str, param: &str, pos: usize) {
    tstate::set_string(
        exceptions::type_error(),
        format!("{name}() missing required argument '{param}' (pos {pos})").as_bytes(),
    );
}

// -------------------------------------------------------------- iteration

/// `PyObject_GetIter` for the types with `tp_iter`.
///
/// # Safety
///
/// `obj` must be a valid object of a readied type.
pub(crate) unsafe fn get_iter(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot read is within the type object.
    let iter = unsafe { (*header::type_of(obj)).tp_iter };
    let Some(iter) = iter else {
        let name = unsafe { crate::util::cstr((*header::type_of(obj)).tp_name) };
        tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not iterable").as_bytes(),
        );
        return core::ptr::null_mut();
    };
    // SAFETY: the slot takes the object.
    unsafe { iter(obj) }
}

/// `PyIter_Next`: the next item, None-shaped null at exhaustion, null
/// with the exception set on error. The caller distinguishes by
/// `tstate::occurred()`.
///
/// # Safety
///
/// `iter` must be an iterator object.
pub(crate) unsafe fn iter_next(iter: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot read is within the type object.
    let next = unsafe { (*header::type_of(iter)).tp_iternext };
    let Some(next) = next else {
        let name = unsafe { crate::util::cstr((*header::type_of(iter)).tp_name) };
        tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not an iterator").as_bytes(),
        );
        return core::ptr::null_mut();
    };
    // SAFETY: the slot takes the iterator.
    unsafe { next(iter) }
}

/// Collect an arbitrary iterable into a vector (`PySequence_List`'s
/// role); Err with the exception set.
///
/// # Safety
///
/// `obj` must be a valid object of a readied type.
pub(crate) unsafe fn collect_iterable(obj: *mut PyObject) -> Result<Vec<*mut PyObject>, ()> {
    // SAFETY: as `get_iter`.
    let iter = unsafe { get_iter(obj) };
    if iter.is_null() {
        return Err(());
    }
    let mut items = Vec::new();
    loop {
        // SAFETY: the loop hands `get_iter`'s result to `iter_next`.
        let item = unsafe { iter_next(iter) };
        if item.is_null() {
            if !tstate::occurred().is_null() {
                return Err(());
            }
            return Ok(items);
        }
        items.push(item);
    }
}

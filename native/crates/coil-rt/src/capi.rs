//! The CPython C API surface exported from the runtime DLL, one module
//! per CPython header (spec section 10.1).

use std::collections::HashSet;
use std::ffi::CStr;
use std::ffi::c_char;
use std::sync::OnceLock;

use coil_capi_sys::PYTHON312_EXPORTS;

// C API exports implemented natively rather than by a raising stub. The
// export layer lands incrementally (spec section 10.1 orders the work by
// tools/capi_usage.py), so it starts empty and every entry of the export
// list is still a stub; names move here as they are implemented.
const IMPLEMENTED: &[&str] = &[
    // modules/mod.rs
    "PyModule_AddFunctions",
    "PyModule_AddIntConstant",
    "PyModule_AddObject",
    "PyModule_AddObjectRef",
    "PyModule_AddStringConstant",
    "PyModule_AddType",
    "PyModule_Create2",
    "PyModule_ExecDef",
    "PyModule_FromDefAndSpec2",
    "PyModule_GetDef",
    "PyModule_GetDict",
    "PyModule_GetFilename",
    "PyModule_GetFilenameObject",
    "PyModule_GetName",
    "PyModule_GetNameObject",
    "PyModule_GetState",
    "PyModule_New",
    "PyModule_NewObject",
    "PyModule_SetDocString",
    "PyModule_Type",
    "PyModuleDef_Init",
    "PyModuleDef_Type",
    "PyState_AddModule",
    "PyState_FindModule",
    "PyState_RemoveModule",
    "_PyModule_Add",
    "_PyModule_Clear",
    "_PyModule_ClearDict",
    "_PyModule_CreateInitialized",
    "_PyModuleSpec_IsInitializing",
    "_PyState_AddModule",
    // modules/cfunc.rs
    "PyCFunction_New",
    "PyCFunction_NewEx",
    "PyCFunction_GetFunction",
    "PyCFunction_GetSelf",
    "PyCFunction_GetFlags",
    "PyCFunction_Type",
    // object/attr.rs
    "PyObject_GetAttrString",
    "PyObject_SetAttrString",
    // types/dict.rs
    "PyDict_SetItemString",
    "PyDict_GetItemString",
    // import.rs
    "PyImport_GetModuleDict",
    "PyImport_AddModuleObject",
    "PyImport_AddModule",
    "_PyImport_SetModuleString",
    "_PyImport_FixupExtensionObject",
    "_PyImport_FixupBuiltin",
    "_PyImport_IsInitialized",
];

/// How many C API exports are implemented natively (not stubs).
pub fn implemented_count() -> usize {
    IMPLEMENTED.len()
}

/// Every C API name this runtime still answers with a stub.
pub fn stub_names() -> &'static HashSet<&'static str> {
    static NAMES: OnceLock<HashSet<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        PYTHON312_EXPORTS
            .lines()
            .map(|line| {
                line.split(' ')
                    .next()
                    .expect("export lines are '<name> <kind>'")
            })
            .filter(|name| !IMPLEMENTED.contains(name))
            .collect()
    })
}

/// Whether `name` is still a stub: `Some(true)` for a stub, `Some(false)`
/// for an implemented export, `None` for a name this runtime does not
/// export. tools/capi_usage.py --check reaches the same answer across the
/// DLL boundary through `coil_rt_is_stub`.
pub fn is_stub(name: &str) -> Option<bool> {
    if IMPLEMENTED.contains(&name) {
        return Some(false);
    }
    if stub_names().contains(name) {
        return Some(true);
    }
    None
}

/// `coil_rt_stub_count`: how many C API exports are still stubs.
#[unsafe(no_mangle)]
pub extern "C" fn coil_rt_stub_count() -> usize {
    stub_names().len()
}

/// `coil_rt_is_stub(name)`: 1 when the name is still a stub, 0 when it is
/// implemented, -1 when it is not an export of this runtime.
///
/// # Safety
/// `name` must point to a NUL-terminated string that stays valid for the
/// duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_is_stub(name: *const c_char) -> i32 {
    // SAFETY: the caller hands us a valid NUL-terminated string per the
    // contract above; nothing outlives the call.
    let name = unsafe { CStr::from_ptr(name) };
    match name.to_str().ok().and_then(is_stub) {
        Some(stub) => i32::from(stub),
        None => -1,
    }
}

//! The CPython C API surface exported from the runtime DLL, one module
//! per CPython header (spec section 10.1).

pub mod r#abstract;
pub mod call;
pub mod capsule;
pub mod gc;
pub mod iterobject;
pub mod memoryobject;
pub mod object;
pub mod recursion;

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
    // capi/abstract.rs: number, sequence, mapping, iterator, buffer protocols
    "PyAIter_Check",
    "PyBuffer_FillContiguousStrides",
    "PyBuffer_FromContiguous",
    "PyBuffer_GetPointer",
    "PyBuffer_IsContiguous",
    "PyBuffer_SizeFromFormat",
    "PyCallable_Check",
    "PyIter_Check",
    "PyIter_Next",
    "PyIter_Send",
    "PyMapping_Check",
    "PyMapping_GetItemString",
    "PyMapping_HasKey",
    "PyMapping_HasKeyString",
    "PyMapping_Items",
    "PyMapping_Keys",
    "PyMapping_Length",
    "PyMapping_SetItemString",
    "PyMapping_Size",
    "PyMapping_Values",
    "PyNumber_Absolute",
    "PyNumber_Add",
    "PyNumber_And",
    "PyNumber_AsSsize_t",
    "PyNumber_Check",
    "PyNumber_Divmod",
    "PyNumber_Float",
    "PyNumber_FloorDivide",
    "PyNumber_InPlaceAdd",
    "PyNumber_InPlaceAnd",
    "PyNumber_InPlaceFloorDivide",
    "PyNumber_InPlaceLshift",
    "PyNumber_InPlaceMatrixMultiply",
    "PyNumber_InPlaceMultiply",
    "PyNumber_InPlaceOr",
    "PyNumber_InPlacePower",
    "PyNumber_InPlaceRemainder",
    "PyNumber_InPlaceRshift",
    "PyNumber_InPlaceSubtract",
    "PyNumber_InPlaceTrueDivide",
    "PyNumber_InPlaceXor",
    "PyNumber_Index",
    "PyNumber_Invert",
    "PyNumber_Long",
    "PyNumber_Lshift",
    "PyNumber_MatrixMultiply",
    "PyNumber_Multiply",
    "PyNumber_Negative",
    "PyNumber_Or",
    "PyNumber_Positive",
    "PyNumber_Power",
    "PyNumber_Remainder",
    "PyNumber_Rshift",
    "PyNumber_Subtract",
    "PyNumber_ToBase",
    "PyNumber_TrueDivide",
    "PyNumber_Xor",
    "PyObject_CheckBuffer",
    "PyObject_DelItem",
    "PyObject_DelItemString",
    "PyObject_GetItem",
    "PyObject_SetItem",
    "PySequence_Check",
    "PySequence_Concat",
    "PySequence_Contains",
    "PySequence_Count",
    "PySequence_DelItem",
    "PySequence_DelSlice",
    "PySequence_Fast",
    "PySequence_GetItem",
    "PySequence_GetSlice",
    "PySequence_In",
    "PySequence_InPlaceConcat",
    "PySequence_InPlaceRepeat",
    "PySequence_Index",
    "PySequence_Length",
    "PySequence_List",
    "PySequence_Repeat",
    "PySequence_SetItem",
    "PySequence_SetSlice",
    "PySequence_Size",
    "PySequence_Tuple",
    "_PyNumber_Index",
    "_PySequence_IterSearch",
    // capi/call.rs: the call and vectorcall families
    "PyObject_Call",
    "PyObject_CallFunction",
    "PyObject_CallFunctionObjArgs",
    "PyObject_CallMethod",
    "PyObject_CallMethodObjArgs",
    "PyObject_CallNoArgs",
    "PyObject_CallObject",
    "PyObject_CallOneArg",
    "PyObject_Vectorcall",
    "PyObject_VectorcallDict",
    "PyObject_VectorcallMethod",
    "PyVectorcall_Call",
    "PyVectorcall_Function",
    "PyVectorcall_NARGS",
    "_PyObject_Call",
    "_PyObject_CallFunction_SizeT",
    "_PyObject_CallMethod",
    "_PyObject_CallMethod_SizeT",
    "_PyObject_Call_Prepend",
    "_PyObject_FastCall",
    "_PyObject_MakeTpCall",
    // capi/capsule.rs
    "PyCapsule_GetContext",
    "PyCapsule_GetDestructor",
    "PyCapsule_GetName",
    "PyCapsule_GetPointer",
    "PyCapsule_Import",
    "PyCapsule_IsValid",
    "PyCapsule_New",
    "PyCapsule_SetContext",
    "PyCapsule_SetDestructor",
    "PyCapsule_SetName",
    "PyCapsule_SetPointer",
    "PyCapsule_Type",
    // capi/gc.rs
    "PyObject_GC_Del",
    "PyObject_GC_IsFinalized",
    "PyObject_GC_IsTracked",
    "PyObject_GC_Track",
    "PyObject_GC_UnTrack",
    "PyObject_IS_GC",
    "PyObject_InitVar",
    "_PyObject_GC_New",
    "_PyObject_GC_NewVar",
    "_PyObject_NewVar",
    // capi/iterobject.rs
    "PyCallIter_New",
    "PySeqIter_New",
    // capi/memoryobject.rs
    "PyBuffer_ToContiguous",
    "PyMemoryView_FromBuffer",
    "PyMemoryView_FromMemory",
    "PyMemoryView_FromObject",
    "PyMemoryView_GetContiguous",
    "PyMemoryView_Type",
    // capi/object.rs: the object protocol
    "PyObject_AsFileDescriptor",
    "PyObject_CallFinalizer",
    "PyObject_CallFinalizerFromDealloc",
    "PyObject_ClearWeakRefs",
    "PyObject_Dir",
    "PyObject_Format",
    "PyObject_GenericGetAttr",
    "PyObject_GenericGetDict",
    "PyObject_GenericSetAttr",
    "PyObject_GenericSetDict",
    "PyObject_GetAIter",
    "PyObject_GetAttr",
    "PyObject_GetAttrString",
    "PyObject_GetIter",
    "PyObject_HasAttr",
    "PyObject_HasAttrString",
    "PyObject_Hash",
    "PyObject_IsInstance",
    "PyObject_IsSubclass",
    "PyObject_IsTrue",
    "PyObject_Length",
    "PyObject_LengthHint",
    "PyObject_Not",
    "PyObject_Print",
    "PyObject_RichCompare",
    "PyObject_RichCompareBool",
    "PyObject_SelfIter",
    "PyObject_SetAttr",
    "PyObject_SetAttrString",
    "PyObject_Size",
    "PyObject_Type",
    "_PyObject_ClearManagedDict",
    "_PyObject_GenericGetAttrWithDict",
    "_PyObject_GenericSetAttrWithDict",
    "_PyObject_GetDictPtr",
    "_PyObject_GetMethod",
    "_PyObject_LookupAttr",
    "_PyObject_LookupSpecial",
    "_PyObject_NextNotImplemented",
    "_PyObject_RealIsInstance",
    "_PyObject_RealIsSubclass",
    "_PyObject_VisitManagedDict",
    // types/unicode.rs, types/bytes.rs: earlier export layers
    "PyBuffer_FillInfo",
    "PyBuffer_Release",
    "PyObject_ASCII",
    "PyObject_Bytes",
    "PyObject_GetBuffer",
    "PyObject_Repr",
    "PyObject_Str",
    // object/typeobject.rs, gc/counted.rs: earlier export layers
    "PyObject_HashNotImplemented",
    "PyObject_Init",
    "_PyObject_New",
    // alloc/pymem.rs: spec section 9.1
    "PyMem_Calloc",
    "PyMem_Free",
    "PyMem_GetAllocator",
    "PyMem_Malloc",
    "PyMem_RawCalloc",
    "PyMem_RawFree",
    "PyMem_RawMalloc",
    "PyMem_RawRealloc",
    "PyMem_Realloc",
    "PyMem_SetAllocator",
    "PyObject_Calloc",
    "PyObject_Free",
    "PyObject_Malloc",
    "PyObject_Realloc",
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

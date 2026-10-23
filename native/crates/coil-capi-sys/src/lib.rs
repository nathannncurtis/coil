//! CPython 3.12.10 C API declarations for the Windows x64 ABI.
//!
//! Field order mirrors the pinned headers (`Include/` of CPython 3.12.10) as
//! built with MSVC for x86-64, where C `long` is 32 bits. The fixed-width
//! aliases below keep that layout valid whatever the building host is, so the
//! tests check it on every platform.
//!
//! The checked-in files beside this crate, `exports.txt` and
//! `python3-exports.txt`, are produced by `tools/gen_capi.py` from the real
//! DLLs and name the surface the runtime must expose.

// Names keep the exact C spelling of the pinned headers.
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use core::ffi::{c_char, c_int, c_uchar, c_uint, c_void};

/// C `long` in the Windows x64 ABI: 32 bits, unlike LP64 hosts.
pub type c_long = i32;
/// C `unsigned long` in the Windows x64 ABI.
pub type c_ulong = u32;

pub type Py_ssize_t = isize;
pub type Py_hash_t = isize;

/// Return type of `sendfunc` (`Py_am_send`).
pub type PySendResult = c_int;

pub const PYGEN_RETURN: PySendResult = 0;
pub const PYGEN_ERROR: PySendResult = -1;
pub const PYGEN_NEXT: PySendResult = 1;

pub type unaryfunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
pub type binaryfunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
pub type ternaryfunc =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
pub type inquiry = unsafe extern "C" fn(*mut PyObject) -> c_int;
pub type lenfunc = unsafe extern "C" fn(*mut PyObject) -> Py_ssize_t;
pub type ssizeargfunc = unsafe extern "C" fn(*mut PyObject, Py_ssize_t) -> *mut PyObject;
pub type ssizessizeargfunc =
    unsafe extern "C" fn(*mut PyObject, Py_ssize_t, Py_ssize_t) -> *mut PyObject;
pub type ssizeobjargproc = unsafe extern "C" fn(*mut PyObject, Py_ssize_t, *mut PyObject) -> c_int;
pub type ssizessizeobjargproc =
    unsafe extern "C" fn(*mut PyObject, Py_ssize_t, Py_ssize_t, *mut PyObject) -> c_int;
pub type objobjargproc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
pub type objobjproc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> c_int;
pub type visitproc = unsafe extern "C" fn(*mut PyObject, *mut c_void) -> c_int;
pub type traverseproc = unsafe extern "C" fn(*mut PyObject, visitproc, *mut c_void) -> c_int;
pub type freefunc = unsafe extern "C" fn(*mut c_void);
pub type destructor = unsafe extern "C" fn(*mut PyObject);
pub type getattrfunc = unsafe extern "C" fn(*mut PyObject, *mut c_char) -> *mut PyObject;
pub type getattrofunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
pub type setattrfunc = unsafe extern "C" fn(*mut PyObject, *mut c_char, *mut PyObject) -> c_int;
pub type setattrofunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
pub type reprfunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
pub type hashfunc = unsafe extern "C" fn(*mut PyObject) -> Py_hash_t;
pub type richcmpfunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, c_int) -> *mut PyObject;
pub type getiterfunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
pub type iternextfunc = unsafe extern "C" fn(*mut PyObject) -> *mut PyObject;
pub type descrgetfunc =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
pub type descrsetfunc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
pub type initproc = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> c_int;
pub type newfunc =
    unsafe extern "C" fn(*mut PyTypeObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
pub type allocfunc = unsafe extern "C" fn(*mut PyTypeObject, Py_ssize_t) -> *mut PyObject;
pub type getter = unsafe extern "C" fn(*mut PyObject, *mut c_void) -> *mut PyObject;
pub type setter = unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut c_void) -> c_int;
pub type getbufferproc = unsafe extern "C" fn(*mut PyObject, *mut Py_buffer, c_int) -> c_int;
pub type releasebufferproc = unsafe extern "C" fn(*mut PyObject, *mut Py_buffer);
pub type sendfunc =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut *mut PyObject) -> PySendResult;
pub type vectorcallfunc = unsafe extern "C" fn(
    *mut PyObject,
    *const *mut PyObject,
    usize,
    *mut PyObject,
) -> *mut PyObject;
pub type PyCFunction = unsafe extern "C" fn(*mut PyObject, *mut PyObject) -> *mut PyObject;
pub type _PyCFunctionFast =
    unsafe extern "C" fn(*mut PyObject, *const *mut PyObject, Py_ssize_t) -> *mut PyObject;
pub type PyCFunctionWithKeywords =
    unsafe extern "C" fn(*mut PyObject, *mut PyObject, *mut PyObject) -> *mut PyObject;
pub type _PyCFunctionFastWithKeywords = unsafe extern "C" fn(
    *mut PyObject,
    *const *mut PyObject,
    Py_ssize_t,
    *mut PyObject,
) -> *mut PyObject;
pub type PyCMethod = unsafe extern "C" fn(
    *mut PyObject,
    *mut PyTypeObject,
    *const *mut PyObject,
    usize,
    *mut PyObject,
) -> *mut PyObject;

#[repr(C)]
pub struct PyObject {
    pub ob_refcnt: Py_ssize_t,
    pub ob_type: *mut PyTypeObject,
}

#[repr(C)]
pub struct PyVarObject {
    pub ob_base: PyObject,
    pub ob_size: Py_ssize_t,
}

#[repr(C)]
pub struct PyNumberMethods {
    pub nb_add: Option<binaryfunc>,
    pub nb_subtract: Option<binaryfunc>,
    pub nb_multiply: Option<binaryfunc>,
    pub nb_remainder: Option<binaryfunc>,
    pub nb_divmod: Option<binaryfunc>,
    pub nb_power: Option<ternaryfunc>,
    pub nb_negative: Option<unaryfunc>,
    pub nb_positive: Option<unaryfunc>,
    pub nb_absolute: Option<unaryfunc>,
    pub nb_bool: Option<inquiry>,
    pub nb_invert: Option<unaryfunc>,
    pub nb_lshift: Option<binaryfunc>,
    pub nb_rshift: Option<binaryfunc>,
    pub nb_and: Option<binaryfunc>,
    pub nb_xor: Option<binaryfunc>,
    pub nb_or: Option<binaryfunc>,
    pub nb_int: Option<unaryfunc>,
    pub nb_reserved: *mut c_void,
    pub nb_float: Option<unaryfunc>,
    pub nb_inplace_add: Option<binaryfunc>,
    pub nb_inplace_subtract: Option<binaryfunc>,
    pub nb_inplace_multiply: Option<binaryfunc>,
    pub nb_inplace_remainder: Option<binaryfunc>,
    pub nb_inplace_power: Option<ternaryfunc>,
    pub nb_inplace_lshift: Option<binaryfunc>,
    pub nb_inplace_rshift: Option<binaryfunc>,
    pub nb_inplace_and: Option<binaryfunc>,
    pub nb_inplace_xor: Option<binaryfunc>,
    pub nb_inplace_or: Option<binaryfunc>,
    pub nb_floor_divide: Option<binaryfunc>,
    pub nb_true_divide: Option<binaryfunc>,
    pub nb_inplace_floor_divide: Option<binaryfunc>,
    pub nb_inplace_true_divide: Option<binaryfunc>,
    pub nb_index: Option<unaryfunc>,
    pub nb_matrix_multiply: Option<binaryfunc>,
    pub nb_inplace_matrix_multiply: Option<binaryfunc>,
}

#[repr(C)]
pub struct PySequenceMethods {
    pub sq_length: Option<lenfunc>,
    pub sq_concat: Option<binaryfunc>,
    pub sq_repeat: Option<ssizeargfunc>,
    pub sq_item: Option<ssizeargfunc>,
    pub was_sq_slice: *mut c_void,
    pub sq_ass_item: Option<ssizeobjargproc>,
    pub was_sq_ass_slice: *mut c_void,
    pub sq_contains: Option<objobjproc>,
    pub sq_inplace_concat: Option<binaryfunc>,
    pub sq_inplace_repeat: Option<ssizeargfunc>,
}

#[repr(C)]
pub struct PyMappingMethods {
    pub mp_length: Option<lenfunc>,
    pub mp_subscript: Option<binaryfunc>,
    pub mp_ass_subscript: Option<objobjargproc>,
}

#[repr(C)]
pub struct PyAsyncMethods {
    pub am_await: Option<unaryfunc>,
    pub am_aiter: Option<unaryfunc>,
    pub am_anext: Option<unaryfunc>,
    pub am_send: Option<sendfunc>,
}

#[repr(C)]
pub struct PyBufferProcs {
    pub bf_getbuffer: Option<getbufferproc>,
    pub bf_releasebuffer: Option<releasebufferproc>,
}

/// The full (not limited-API) 3.12 type object. `ob_refcnt`, `ob_type` and
/// `ob_size` spell out the PyObject_VAR_HEAD prefix.
#[repr(C)]
pub struct PyTypeObject {
    pub ob_refcnt: Py_ssize_t,
    pub ob_type: *mut PyTypeObject,
    pub ob_size: Py_ssize_t,
    pub tp_name: *const c_char,
    pub tp_basicsize: Py_ssize_t,
    pub tp_itemsize: Py_ssize_t,
    pub tp_dealloc: Option<destructor>,
    pub tp_vectorcall_offset: Py_ssize_t,
    pub tp_getattr: Option<getattrfunc>,
    pub tp_setattr: Option<setattrfunc>,
    pub tp_as_async: *mut PyAsyncMethods,
    pub tp_repr: Option<reprfunc>,
    pub tp_as_number: *mut PyNumberMethods,
    pub tp_as_sequence: *mut PySequenceMethods,
    pub tp_as_mapping: *mut PyMappingMethods,
    pub tp_hash: Option<hashfunc>,
    pub tp_call: Option<ternaryfunc>,
    pub tp_str: Option<reprfunc>,
    pub tp_getattro: Option<getattrofunc>,
    pub tp_setattro: Option<setattrofunc>,
    pub tp_as_buffer: *mut PyBufferProcs,
    pub tp_flags: c_ulong,
    pub tp_doc: *const c_char,
    pub tp_traverse: Option<traverseproc>,
    pub tp_clear: Option<inquiry>,
    pub tp_richcompare: Option<richcmpfunc>,
    pub tp_weaklistoffset: Py_ssize_t,
    pub tp_iter: Option<getiterfunc>,
    pub tp_iternext: Option<iternextfunc>,
    pub tp_methods: *mut PyMethodDef,
    pub tp_members: *mut PyMemberDef,
    pub tp_getset: *mut PyGetSetDef,
    pub tp_base: *mut PyTypeObject,
    pub tp_dict: *mut PyObject,
    pub tp_descr_get: Option<descrgetfunc>,
    pub tp_descr_set: Option<descrsetfunc>,
    pub tp_dictoffset: Py_ssize_t,
    pub tp_init: Option<initproc>,
    pub tp_alloc: Option<allocfunc>,
    pub tp_new: Option<newfunc>,
    pub tp_free: Option<freefunc>,
    pub tp_is_gc: Option<inquiry>,
    pub tp_bases: *mut PyObject,
    pub tp_mro: *mut PyObject,
    pub tp_cache: *mut PyObject,
    pub tp_subclasses: *mut c_void,
    pub tp_weaklist: *mut PyObject,
    pub tp_del: Option<destructor>,
    pub tp_version_tag: c_uint,
    pub tp_finalize: Option<destructor>,
    pub tp_vectorcall: Option<vectorcallfunc>,
    pub tp_watched: c_uchar,
}

#[repr(C)]
pub struct PyMethodDef {
    pub ml_name: *const c_char,
    pub ml_meth: Option<PyCFunction>,
    pub ml_flags: c_int,
    pub ml_doc: *const c_char,
}

#[repr(C)]
pub struct PyGetSetDef {
    pub name: *const c_char,
    pub get: Option<getter>,
    pub set: Option<setter>,
    pub doc: *const c_char,
    pub closure: *mut c_void,
}

#[repr(C)]
pub struct PyMemberDef {
    pub name: *const c_char,
    pub type_: c_int,
    pub offset: Py_ssize_t,
    pub flags: c_int,
    pub doc: *const c_char,
}

#[repr(C)]
pub struct PyModuleDef_Base {
    pub ob_base: PyObject,
    pub m_init: Option<unsafe extern "C" fn() -> *mut PyObject>,
    pub m_index: Py_ssize_t,
    pub m_copy: *mut PyObject,
}

#[repr(C)]
pub struct PyModuleDef_Slot {
    pub slot: c_int,
    pub value: *mut c_void,
}

#[repr(C)]
pub struct PyModuleDef {
    pub m_base: PyModuleDef_Base,
    pub m_name: *const c_char,
    pub m_doc: *const c_char,
    pub m_size: Py_ssize_t,
    pub m_methods: *mut PyMethodDef,
    pub m_slots: *mut PyModuleDef_Slot,
    pub m_traverse: Option<traverseproc>,
    pub m_clear: Option<inquiry>,
    pub m_free: Option<freefunc>,
}

#[repr(C)]
pub struct PyType_Slot {
    pub slot: c_int,
    pub pfunc: *mut c_void,
}

#[repr(C)]
pub struct PyType_Spec {
    pub name: *const c_char,
    pub basicsize: c_int,
    pub itemsize: c_int,
    pub flags: c_uint,
    pub slots: *mut PyType_Slot,
}

#[repr(C)]
pub struct Py_buffer {
    pub buf: *mut c_void,
    pub obj: *mut PyObject,
    pub len: Py_ssize_t,
    pub itemsize: Py_ssize_t,
    pub readonly: c_int,
    pub ndim: c_int,
    pub format: *mut c_char,
    pub shape: *mut Py_ssize_t,
    pub strides: *mut Py_ssize_t,
    pub suboffsets: *mut Py_ssize_t,
    pub internal: *mut c_void,
}

// Type flags (tp_flags) from the pinned object.h; c_ulong because MSVC x64
// stores unsigned long in 32 bits.
pub const Py_TPFLAGS_HAVE_FINALIZE: c_ulong = 1 << 0;
pub const Py_TPFLAGS_MANAGED_WEAKREF: c_ulong = 1 << 3;
pub const Py_TPFLAGS_MANAGED_DICT: c_ulong = 1 << 4;
pub const Py_TPFLAGS_PREHEADER: c_ulong = Py_TPFLAGS_MANAGED_WEAKREF | Py_TPFLAGS_MANAGED_DICT;
pub const Py_TPFLAGS_SEQUENCE: c_ulong = 1 << 5;
pub const Py_TPFLAGS_MAPPING: c_ulong = 1 << 6;
pub const Py_TPFLAGS_DISALLOW_INSTANTIATION: c_ulong = 1 << 7;
pub const Py_TPFLAGS_IMMUTABLETYPE: c_ulong = 1 << 8;
pub const Py_TPFLAGS_HEAPTYPE: c_ulong = 1 << 9;
pub const Py_TPFLAGS_BASETYPE: c_ulong = 1 << 10;
pub const Py_TPFLAGS_HAVE_VECTORCALL: c_ulong = 1 << 11;
pub const Py_TPFLAGS_READY: c_ulong = 1 << 12;
pub const Py_TPFLAGS_READYING: c_ulong = 1 << 13;
pub const Py_TPFLAGS_HAVE_GC: c_ulong = 1 << 14;
pub const Py_TPFLAGS_HAVE_STACKLESS_EXTENSION: c_ulong = 0;
pub const Py_TPFLAGS_METHOD_DESCRIPTOR: c_ulong = 1 << 17;
pub const Py_TPFLAGS_HAVE_VERSION_TAG: c_ulong = 1 << 18;
pub const Py_TPFLAGS_VALID_VERSION_TAG: c_ulong = 1 << 19;
pub const Py_TPFLAGS_IS_ABSTRACT: c_ulong = 1 << 20;
pub const Py_TPFLAGS_ITEMS_AT_END: c_ulong = 1 << 23;
pub const Py_TPFLAGS_LONG_SUBCLASS: c_ulong = 1 << 24;
pub const Py_TPFLAGS_LIST_SUBCLASS: c_ulong = 1 << 25;
pub const Py_TPFLAGS_TUPLE_SUBCLASS: c_ulong = 1 << 26;
pub const Py_TPFLAGS_BYTES_SUBCLASS: c_ulong = 1 << 27;
pub const Py_TPFLAGS_UNICODE_SUBCLASS: c_ulong = 1 << 28;
pub const Py_TPFLAGS_DICT_SUBCLASS: c_ulong = 1 << 29;
pub const Py_TPFLAGS_BASE_EXC_SUBCLASS: c_ulong = 1 << 30;
pub const Py_TPFLAGS_TYPE_SUBCLASS: c_ulong = 1 << 31;
pub const Py_TPFLAGS_DEFAULT: c_ulong = Py_TPFLAGS_HAVE_STACKLESS_EXTENSION;

// Method-definition flags (ml_flags) from the pinned methodobject.h.
pub const METH_VARARGS: c_int = 0x0001;
pub const METH_KEYWORDS: c_int = 0x0002;
pub const METH_NOARGS: c_int = 0x0004;
pub const METH_O: c_int = 0x0008;
pub const METH_CLASS: c_int = 0x0010;
pub const METH_STATIC: c_int = 0x0020;
pub const METH_COEXIST: c_int = 0x0040;
pub const METH_FASTCALL: c_int = 0x0080;
pub const METH_METHOD: c_int = 0x0200;

// Type-slot ids for PyType_FromSpec from the pinned typeslots.h; the numbers
// are part of the stable ABI.
pub const Py_bf_getbuffer: c_int = 1;
pub const Py_bf_releasebuffer: c_int = 2;
pub const Py_mp_ass_subscript: c_int = 3;
pub const Py_mp_length: c_int = 4;
pub const Py_mp_subscript: c_int = 5;
pub const Py_nb_absolute: c_int = 6;
pub const Py_nb_add: c_int = 7;
pub const Py_nb_and: c_int = 8;
pub const Py_nb_bool: c_int = 9;
pub const Py_nb_divmod: c_int = 10;
pub const Py_nb_float: c_int = 11;
pub const Py_nb_floor_divide: c_int = 12;
pub const Py_nb_index: c_int = 13;
pub const Py_nb_inplace_add: c_int = 14;
pub const Py_nb_inplace_and: c_int = 15;
pub const Py_nb_inplace_floor_divide: c_int = 16;
pub const Py_nb_inplace_lshift: c_int = 17;
pub const Py_nb_inplace_multiply: c_int = 18;
pub const Py_nb_inplace_or: c_int = 19;
pub const Py_nb_inplace_power: c_int = 20;
pub const Py_nb_inplace_remainder: c_int = 21;
pub const Py_nb_inplace_rshift: c_int = 22;
pub const Py_nb_inplace_subtract: c_int = 23;
pub const Py_nb_inplace_true_divide: c_int = 24;
pub const Py_nb_inplace_xor: c_int = 25;
pub const Py_nb_int: c_int = 26;
pub const Py_nb_invert: c_int = 27;
pub const Py_nb_lshift: c_int = 28;
pub const Py_nb_multiply: c_int = 29;
pub const Py_nb_negative: c_int = 30;
pub const Py_nb_or: c_int = 31;
pub const Py_nb_positive: c_int = 32;
pub const Py_nb_power: c_int = 33;
pub const Py_nb_remainder: c_int = 34;
pub const Py_nb_rshift: c_int = 35;
pub const Py_nb_subtract: c_int = 36;
pub const Py_nb_true_divide: c_int = 37;
pub const Py_nb_xor: c_int = 38;
pub const Py_sq_ass_item: c_int = 39;
pub const Py_sq_concat: c_int = 40;
pub const Py_sq_contains: c_int = 41;
pub const Py_sq_inplace_concat: c_int = 42;
pub const Py_sq_inplace_repeat: c_int = 43;
pub const Py_sq_item: c_int = 44;
pub const Py_sq_length: c_int = 45;
pub const Py_sq_repeat: c_int = 46;
pub const Py_tp_alloc: c_int = 47;
pub const Py_tp_base: c_int = 48;
pub const Py_tp_bases: c_int = 49;
pub const Py_tp_call: c_int = 50;
pub const Py_tp_clear: c_int = 51;
pub const Py_tp_dealloc: c_int = 52;
pub const Py_tp_del: c_int = 53;
pub const Py_tp_descr_get: c_int = 54;
pub const Py_tp_descr_set: c_int = 55;
pub const Py_tp_doc: c_int = 56;
pub const Py_tp_getattr: c_int = 57;
pub const Py_tp_getattro: c_int = 58;
pub const Py_tp_hash: c_int = 59;
pub const Py_tp_init: c_int = 60;
pub const Py_tp_is_gc: c_int = 61;
pub const Py_tp_iter: c_int = 62;
pub const Py_tp_iternext: c_int = 63;
pub const Py_tp_methods: c_int = 64;
pub const Py_tp_new: c_int = 65;
pub const Py_tp_repr: c_int = 66;
pub const Py_tp_richcompare: c_int = 67;
pub const Py_tp_setattr: c_int = 68;
pub const Py_tp_setattro: c_int = 69;
pub const Py_tp_str: c_int = 70;
pub const Py_tp_traverse: c_int = 71;
pub const Py_tp_members: c_int = 72;
pub const Py_tp_getset: c_int = 73;
pub const Py_tp_free: c_int = 74;
pub const Py_nb_matrix_multiply: c_int = 75;
pub const Py_nb_inplace_matrix_multiply: c_int = 76;
pub const Py_am_await: c_int = 77;
pub const Py_am_aiter: c_int = 78;
pub const Py_am_anext: c_int = 79;
pub const Py_tp_finalize: c_int = 80;
pub const Py_am_send: c_int = 81;

// Module-slot ids (PyModuleDef.m_slots) from the pinned moduleobject.h.
pub const Py_mod_create: c_int = 1;
pub const Py_mod_exec: c_int = 2;
pub const Py_mod_multiple_interpreters: c_int = 3;

// Values for the Py_mod_multiple_interpreters slot.
pub const Py_MOD_MULTIPLE_INTERPRETERS_NOT_SUPPORTED: *mut c_void = core::ptr::null_mut();
pub const Py_MOD_MULTIPLE_INTERPRETERS_SUPPORTED: *mut c_void =
    core::ptr::without_provenance_mut(1);
pub const Py_MOD_PER_INTERPRETER_GIL_SUPPORTED: *mut c_void = core::ptr::without_provenance_mut(2);

// Buffer-request flags (Py_buffer) from the pinned pybuffer.h.
pub const PyBUF_MAX_NDIM: c_int = 64;
pub const PyBUF_SIMPLE: c_int = 0;
pub const PyBUF_WRITABLE: c_int = 0x0001;
pub const PyBUF_WRITEABLE: c_int = PyBUF_WRITABLE;
pub const PyBUF_FORMAT: c_int = 0x0004;
pub const PyBUF_ND: c_int = 0x0008;
pub const PyBUF_STRIDES: c_int = 0x0010 | PyBUF_ND;
pub const PyBUF_C_CONTIGUOUS: c_int = 0x0020 | PyBUF_STRIDES;
pub const PyBUF_F_CONTIGUOUS: c_int = 0x0040 | PyBUF_STRIDES;
pub const PyBUF_ANY_CONTIGUOUS: c_int = 0x0080 | PyBUF_STRIDES;
pub const PyBUF_INDIRECT: c_int = 0x0100 | PyBUF_STRIDES;
pub const PyBUF_CONTIG: c_int = PyBUF_ND | PyBUF_WRITABLE;
pub const PyBUF_CONTIG_RO: c_int = PyBUF_ND;
pub const PyBUF_STRIDED: c_int = PyBUF_STRIDES | PyBUF_WRITABLE;
pub const PyBUF_STRIDED_RO: c_int = PyBUF_STRIDES;
pub const PyBUF_RECORDS: c_int = PyBUF_STRIDES | PyBUF_WRITABLE | PyBUF_FORMAT;
pub const PyBUF_RECORDS_RO: c_int = PyBUF_STRIDES | PyBUF_FORMAT;
pub const PyBUF_FULL: c_int = PyBUF_INDIRECT | PyBUF_WRITABLE | PyBUF_FORMAT;
pub const PyBUF_FULL_RO: c_int = PyBUF_INDIRECT | PyBUF_FORMAT;
pub const PyBUF_READ: c_int = 0x100;
pub const PyBUF_WRITE: c_int = 0x200;

//! CPython 3.12.10 object layouts on Windows x64 (spec section 8.2).
//!
//! The numbers are checked against the pinned headers by
//! `tools/layout_oracle.py --check`, which compiles `tools/layout_oracle.c`
//! with MSVC and prints the same offsets and sizes from `offsetof`/`sizeof`.
//! Exact layouts must match CPython byte for byte. Prefix layouts pin only
//! the leading fields listed; their tail is Coil's, and `SIZEOF` there
//! records the CPython struct for reference, not a requirement on Coil.

/// `PyObject`: the 16-byte header every object starts with (section 8.1).
pub mod object {
    pub const OB_REFCNT: usize = 0;
    pub const OB_TYPE: usize = 8;
    pub const SIZEOF: usize = 16;
}

/// `PyVarObject`: variable-size objects add `ob_size` at 16 (section 8.1).
pub mod varobject {
    pub const OB_BASE: usize = 0;
    pub const OB_SIZE: usize = 16;
    pub const SIZEOF: usize = 24;
}

/// `PyTupleObject` (exact).
pub mod tuple {
    pub const OB_ITEM: usize = 24;
    pub const SIZEOF: usize = 32;
}

/// `PyListObject` (exact).
pub mod list {
    pub const OB_ITEM: usize = 24;
    pub const ALLOCATED: usize = 32;
    pub const SIZEOF: usize = 40;
}

/// `PyBytesObject` (exact).
pub mod bytes {
    pub const OB_SHASH: usize = 24;
    pub const OB_SVAL: usize = 32;
    pub const SIZEOF: usize = 40;
}

/// `PyByteArrayObject` (exact).
pub mod bytearray {
    pub const OB_ALLOC: usize = 24;
    pub const OB_BYTES: usize = 32;
    pub const OB_START: usize = 40;
    pub const OB_EXPORTS: usize = 48;
    pub const SIZEOF: usize = 56;
}

/// `PyASCIIObject`: compact ASCII strings (exact).
pub mod ascii {
    pub const LENGTH: usize = 16;
    pub const HASH: usize = 24;
    pub const STATE: usize = 32;
    pub const SIZEOF: usize = 40;
}

/// `PyCompactUnicodeObject`: compact non-ASCII strings (exact).
pub mod compact_unicode {
    pub const UTF8_LENGTH: usize = 40;
    pub const UTF8: usize = 48;
    pub const SIZEOF: usize = 56;
}

/// `PyUnicodeObject`: legacy (subclass) strings (exact).
pub mod unicode {
    pub const DATA: usize = 56;
    pub const SIZEOF: usize = 64;
}

/// `PyFloatObject` (exact).
pub mod float {
    pub const OB_FVAL: usize = 16;
    pub const SIZEOF: usize = 24;
}

/// `PyComplexObject` (exact).
pub mod complex {
    pub const CVAL: usize = 16;
    pub const CVAL_REAL: usize = 16;
    pub const CVAL_IMAG: usize = 24;
    pub const SIZEOF: usize = 32;
}

/// `PyLongObject` (exact): `lv_tag` plus 30-bit digits.
pub mod long {
    pub const LONG_VALUE: usize = 16;
    pub const LV_TAG: usize = 16;
    pub const OB_DIGIT: usize = 24;
    pub const SIZEOF: usize = 32;
}

/// `bool` objects are `PyLongObject` instances; 3.12 has no `PyBoolObject` (exact).
pub mod bool {
    pub const SIZEOF: usize = 32;
}

/// `PyMemoryViewObject` (exact).
pub mod memoryview {
    pub const MBUF: usize = 24;
    pub const HASH: usize = 32;
    pub const FLAGS: usize = 40;
    pub const EXPORTS: usize = 48;
    pub const VIEW: usize = 56;
    pub const WEAKREFLIST: usize = 136;
    pub const OB_ARRAY: usize = 144;
    pub const SIZEOF: usize = 152;
}

/// `Py_buffer` (exact).
pub mod buffer {
    pub const BUF: usize = 0;
    pub const OBJ: usize = 8;
    pub const LEN: usize = 16;
    pub const ITEMSIZE: usize = 24;
    pub const READONLY: usize = 32;
    pub const NDIM: usize = 36;
    pub const FORMAT: usize = 40;
    pub const SHAPE: usize = 48;
    pub const STRIDES: usize = 56;
    pub const SUBOFFSETS: usize = 64;
    pub const INTERNAL: usize = 72;
    pub const SIZEOF: usize = 80;
}

/// `PyCellObject` (exact).
pub mod cell {
    pub const OB_REF: usize = 16;
    pub const SIZEOF: usize = 24;
}

/// `PyWeakReference` (exact).
pub mod weakref {
    pub const WR_OBJECT: usize = 16;
    pub const WR_CALLBACK: usize = 24;
    pub const HASH: usize = 32;
    pub const WR_PREV: usize = 40;
    pub const WR_NEXT: usize = 48;
    pub const VECTORCALL: usize = 56;
    pub const SIZEOF: usize = 64;
}

/// `PyCFunctionObject` (exact).
pub mod cfunction {
    pub const M_ML: usize = 16;
    pub const M_SELF: usize = 24;
    pub const M_MODULE: usize = 32;
    pub const M_WEAKREFLIST: usize = 40;
    pub const VECTORCALL: usize = 48;
    pub const SIZEOF: usize = 56;
}

/// `PyCMethodObject` (exact).
pub mod cmethod {
    pub const FUNC: usize = 0;
    pub const MM_CLASS: usize = 56;
    pub const SIZEOF: usize = 64;
}

/// `PyMethodDescrObject` (exact).
pub mod method_descr {
    pub const D_TYPE: usize = 16;
    pub const D_NAME: usize = 24;
    pub const D_QUALNAME: usize = 32;
    pub const D_METHOD: usize = 40;
    pub const VECTORCALL: usize = 48;
    pub const SIZEOF: usize = 56;
}

/// `PyMemberDescrObject` (exact).
pub mod member_descr {
    pub const D_TYPE: usize = 16;
    pub const D_NAME: usize = 24;
    pub const D_QUALNAME: usize = 32;
    pub const D_MEMBER: usize = 40;
    pub const SIZEOF: usize = 48;
}

/// `PyGetSetDescrObject` (exact).
pub mod getset_descr {
    pub const D_TYPE: usize = 16;
    pub const D_NAME: usize = 24;
    pub const D_QUALNAME: usize = 32;
    pub const D_GETSET: usize = 40;
    pub const SIZEOF: usize = 48;
}

/// `PyWrapperDescrObject` (exact).
pub mod wrapper_descr {
    pub const D_TYPE: usize = 16;
    pub const D_NAME: usize = 24;
    pub const D_QUALNAME: usize = 32;
    pub const D_BASE: usize = 40;
    pub const D_WRAPPED: usize = 48;
    pub const SIZEOF: usize = 56;
}

/// `PyMethodObject` (exact).
pub mod method {
    pub const IM_FUNC: usize = 16;
    pub const IM_SELF: usize = 24;
    pub const IM_WEAKREFLIST: usize = 32;
    pub const VECTORCALL: usize = 40;
    pub const SIZEOF: usize = 48;
}

/// `PySliceObject` (exact).
pub mod slice {
    pub const START: usize = 16;
    pub const STOP: usize = 24;
    pub const STEP: usize = 32;
    pub const SIZEOF: usize = 40;
}

/// `PyBaseExceptionObject` and every subclass header (exact).
pub mod base_exception {
    pub const DICT: usize = 16;
    pub const ARGS: usize = 24;
    pub const NOTES: usize = 32;
    pub const TRACEBACK: usize = 40;
    pub const CONTEXT: usize = 48;
    pub const CAUSE: usize = 56;
    pub const SUPPRESS_CONTEXT: usize = 64;
    pub const SIZEOF: usize = 72;
}

/// `PyUnicodeErrorObject`: `UnicodeDecodeError`/`UnicodeEncodeError`/
/// `UnicodeTranslateError` instances (exact).
pub mod unicode_error {
    pub const ENCODING: usize = 72;
    pub const OBJECT: usize = 80;
    pub const START: usize = 88;
    pub const END: usize = 96;
    pub const REASON: usize = 104;
    pub const SIZEOF: usize = 112;
}

/// `PyTracebackObject` (exact).
pub mod traceback {
    pub const TB_NEXT: usize = 16;
    pub const TB_FRAME: usize = 24;
    pub const TB_LASTI: usize = 32;
    pub const TB_LINENO: usize = 36;
    pub const SIZEOF: usize = 40;
}

/// `PyModuleDef` (exact).
pub mod moduledef {
    pub const M_BASE: usize = 0;
    pub const M_INIT: usize = 16;
    pub const M_INDEX: usize = 24;
    pub const M_COPY: usize = 32;
    pub const M_NAME: usize = 40;
    pub const M_DOC: usize = 48;
    pub const M_SIZE: usize = 56;
    pub const M_METHODS: usize = 64;
    pub const M_SLOTS: usize = 72;
    pub const M_TRAVERSE: usize = 80;
    pub const M_CLEAR: usize = 88;
    pub const M_FREE: usize = 96;
    pub const SIZEOF: usize = 104;
}

/// `PyTypeObject` (exact). Under LLP64 `long` is 4 bytes, so `tp_flags` and
/// `tp_version_tag` are 4-byte fields.
pub mod typeobject {
    pub const TP_NAME: usize = 24;
    pub const TP_BASICSIZE: usize = 32;
    pub const TP_ITEMSIZE: usize = 40;
    pub const TP_DEALLOC: usize = 48;
    pub const TP_VECTORCALL_OFFSET: usize = 56;
    pub const TP_GETATTR: usize = 64;
    pub const TP_SETATTR: usize = 72;
    pub const TP_AS_ASYNC: usize = 80;
    pub const TP_REPR: usize = 88;
    pub const TP_AS_NUMBER: usize = 96;
    pub const TP_AS_SEQUENCE: usize = 104;
    pub const TP_AS_MAPPING: usize = 112;
    pub const TP_HASH: usize = 120;
    pub const TP_CALL: usize = 128;
    pub const TP_STR: usize = 136;
    pub const TP_GETATTRO: usize = 144;
    pub const TP_SETATTRO: usize = 152;
    pub const TP_AS_BUFFER: usize = 160;
    pub const TP_FLAGS: usize = 168;
    pub const TP_DOC: usize = 176;
    pub const TP_TRAVERSE: usize = 184;
    pub const TP_CLEAR: usize = 192;
    pub const TP_RICHCOMPARE: usize = 200;
    pub const TP_WEAKLISTOFFSET: usize = 208;
    pub const TP_ITER: usize = 216;
    pub const TP_ITERNEXT: usize = 224;
    pub const TP_METHODS: usize = 232;
    pub const TP_MEMBERS: usize = 240;
    pub const TP_GETSET: usize = 248;
    pub const TP_BASE: usize = 256;
    pub const TP_DICT: usize = 264;
    pub const TP_DESCR_GET: usize = 272;
    pub const TP_DESCR_SET: usize = 280;
    pub const TP_DICTOFFSET: usize = 288;
    pub const TP_INIT: usize = 296;
    pub const TP_ALLOC: usize = 304;
    pub const TP_NEW: usize = 312;
    pub const TP_FREE: usize = 320;
    pub const TP_IS_GC: usize = 328;
    pub const TP_BASES: usize = 336;
    pub const TP_MRO: usize = 344;
    pub const TP_CACHE: usize = 352;
    pub const TP_SUBCLASSES: usize = 360;
    pub const TP_WEAKLIST: usize = 368;
    pub const TP_DEL: usize = 376;
    pub const TP_VERSION_TAG: usize = 384;
    pub const TP_FINALIZE: usize = 392;
    pub const TP_VECTORCALL: usize = 400;
    pub const TP_WATCHED: usize = 408;
    pub const SIZEOF: usize = 416;
}

/// `PyHeapTypeObject` (exact).
pub mod heaptypeobject {
    pub const HT_TYPE: usize = 0;
    pub const AS_ASYNC: usize = 416;
    pub const AS_NUMBER: usize = 448;
    pub const AS_MAPPING: usize = 736;
    pub const AS_SEQUENCE: usize = 760;
    pub const AS_BUFFER: usize = 840;
    pub const HT_NAME: usize = 856;
    pub const HT_SLOTS: usize = 864;
    pub const HT_QUALNAME: usize = 872;
    pub const HT_CACHED_KEYS: usize = 880;
    pub const HT_MODULE: usize = 888;
    pub const HT_TPNAME: usize = 896;
    pub const SPEC_CACHE: usize = 904;
    pub const SIZEOF: usize = 920;
}

/// `PyDictObject` (prefix): `ma_used`/`ma_version_tag` and the inline `PyDict_GET_SIZE`; the tail is Coil's.
pub mod dict {
    pub const MA_USED: usize = 16;
    pub const MA_VERSION_TAG: usize = 24;
    pub const SIZEOF: usize = 48;
}

/// `PySetObject` (prefix): `fill`/`used` and the inline `PySet_GET_SIZE`; the tail is Coil's.
pub mod set {
    pub const FILL: usize = 16;
    pub const USED: usize = 24;
    pub const SIZEOF: usize = 200;
}

/// `PyFunctionObject` (prefix): `func_globals` through `func_closure` in CPython order; the tail is Coil's.
pub mod function {
    pub const FUNC_GLOBALS: usize = 16;
    pub const FUNC_BUILTINS: usize = 24;
    pub const FUNC_NAME: usize = 32;
    pub const FUNC_QUALNAME: usize = 40;
    pub const FUNC_CODE: usize = 48;
    pub const FUNC_DEFAULTS: usize = 56;
    pub const FUNC_KWDEFAULTS: usize = 64;
    pub const FUNC_CLOSURE: usize = 72;
    pub const SIZEOF: usize = 144;
}

/// `PyCodeObject` (prefix): `co_flags` and the `co_argcount` family; the tail is Coil's.
pub mod code {
    pub const CO_FLAGS: usize = 48;
    pub const CO_ARGCOUNT: usize = 52;
    pub const CO_POSONLYARGCOUNT: usize = 56;
    pub const CO_KWONLYARGCOUNT: usize = 60;
    pub const SIZEOF: usize = 200;
}

#[cfg(test)]
mod tests {
    #[test]
    fn well_known_layouts() {
        use super::{base_exception, dict, long, object, tuple, typeobject, varobject};
        assert_eq!(object::SIZEOF, 16);
        assert_eq!(object::OB_TYPE, 8);
        assert_eq!(varobject::SIZEOF, 24);
        assert_eq!(tuple::OB_ITEM, 24);
        assert_eq!(long::LV_TAG, 16);
        assert_eq!(dict::MA_VERSION_TAG, 24);
        assert_eq!(typeobject::TP_FLAGS, 168);
        assert_eq!(base_exception::SUPPRESS_CONTEXT, 64);
    }
}

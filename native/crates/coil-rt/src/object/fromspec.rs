//! `PyType_From*Spec`: build a heap type from a `PyType_Spec`, as CPython
//! 3.12's `typeobject.c` does. The core is `_PyType_FromMetaclass_impl`;
//! the four public entry points are thin wrappers over it.

use core::ffi::{c_char, c_int, c_void};
use core::mem::offset_of;

use coil_capi_sys::{
    Py_TPFLAGS_HEAPTYPE, Py_TPFLAGS_TYPE_SUBCLASS, Py_tp_base, Py_tp_bases, Py_tp_doc,
    Py_tp_members, PyAsyncMethods, PyBufferProcs, PyMappingMethods, PyMemberDef, PyNumberMethods,
    PyObject, PySequenceMethods, PyType_Spec, PyTypeObject,
};

use super::header;
use super::typeobject::{
    PyBaseObject_Type, PyType_GenericAlloc, PyType_IsSubtype, PyType_Ready, PyType_Type, is_ready,
};
use crate::types;
use crate::types::exc as exceptions;

/// `Py_RELATIVE_OFFSET` (descrobject.h): the member offset is relative to
/// the base's `tp_basicsize`.
const Py_RELATIVE_OFFSET: c_int = 8;
/// `ALIGNOF_MAX_ALIGN_T` (object.h): the alignment of `max_align_t`.
const ALIGNOF_MAX_ALIGN_T: isize = 16;

/// `PySlot_Offset` (typeobject.c): the offset of a slot within its
/// substruct and the offset of the substruct within `PyTypeObject`.
#[repr(C)]
struct PySlotOffset {
    subslot_offset: i16,
    slot_offset: i16,
}

/// `pyslot_offsets` (typeobject.c): the slot table, indexed by slot number.
/// Index 0 is unused; indices 1..=81 are the `Py_tp_*` / `Py_nb_*` /
/// `Py_am_*` slots.
const PYSLOT_OFFSETS: [PySlotOffset; 82] = [
    PySlotOffset {
        subslot_offset: 0,
        slot_offset: 0,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyBufferProcs, bf_getbuffer) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_buffer) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyBufferProcs, bf_releasebuffer) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_buffer) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyMappingMethods, mp_ass_subscript) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_mapping) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyMappingMethods, mp_length) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_mapping) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyMappingMethods, mp_subscript) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_mapping) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_absolute) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_add) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_and) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_bool) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_divmod) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_float) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_floor_divide) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_index) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_add) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_and) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_floor_divide) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_lshift) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_multiply) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_or) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_power) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_remainder) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_rshift) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_subtract) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_true_divide) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_xor) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_int) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_invert) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_lshift) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_multiply) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_negative) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_or) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_positive) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_power) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_remainder) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_rshift) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_subtract) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_true_divide) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_xor) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_ass_item) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_concat) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_contains) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_inplace_concat) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_inplace_repeat) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_item) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_length) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PySequenceMethods, sq_repeat) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_sequence) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_alloc) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_base) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_bases) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_call) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_clear) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_dealloc) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_del) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_descr_get) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_descr_set) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_doc) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_getattr) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_getattro) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_hash) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_init) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_is_gc) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_iter) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_iternext) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_methods) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_new) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_repr) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_richcompare) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_setattr) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_setattro) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_str) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_traverse) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_members) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_getset) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_free) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_matrix_multiply) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyNumberMethods, nb_inplace_matrix_multiply) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_number) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyAsyncMethods, am_await) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_async) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyAsyncMethods, am_aiter) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_async) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyAsyncMethods, am_anext) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_async) as i16,
    },
    PySlotOffset {
        subslot_offset: -1,
        slot_offset: offset_of!(PyTypeObject, tp_finalize) as i16,
    },
    PySlotOffset {
        subslot_offset: offset_of!(PyAsyncMethods, am_send) as i16,
        slot_offset: offset_of!(PyTypeObject, tp_as_async) as i16,
    },
];

/// The heap-type layout offsets (cp312 `heaptypeobject`), relative to the
/// start of the type object (whose `ht_type` is at offset 0).
const AS_ASYNC: usize = 416;
const AS_NUMBER: usize = 448;
const AS_MAPPING: usize = 736;
const AS_SEQUENCE: usize = 760;
const AS_BUFFER: usize = 840;
const HT_NAME: usize = 856;
const HT_QUALNAME: usize = 872;
const HT_MODULE: usize = 888;
const HT_TPNAME: usize = 896;

/// `PyType_Check` (object.h): whether `op`'s type has the type-subclass
/// flag.
unsafe fn pytype_check(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: the flag read is within the type object.
    !ty.is_null() && unsafe { (*ty).tp_flags } & Py_TPFLAGS_TYPE_SUBCLASS != 0
}

/// `_align_up` (typeobject.c): round `size` up to a multiple of
/// `ALIGNOF_MAX_ALIGN_T`.
fn align_up(size: isize) -> isize {
    (size + ALIGNOF_MAX_ALIGN_T - 1) & !(ALIGNOF_MAX_ALIGN_T - 1)
}

/// `shape_differs` (typeobject.c): whether two types have different
/// instance layouts.
fn shape_differs(t1: *const PyTypeObject, t2: *const PyTypeObject) -> bool {
    // SAFETY: the reads are within the type objects.
    unsafe { (*t1).tp_basicsize != (*t2).tp_basicsize || (*t1).tp_itemsize != (*t2).tp_itemsize }
}

/// `solid_base` (typeobject.c): the nearest base whose shape differs from
/// `type`'s, or `object` if none.
fn solid_base(type_: *const PyTypeObject) -> *const PyTypeObject {
    // SAFETY: the read is within the type object.
    let base = unsafe { (*type_).tp_base };
    let base = if base.is_null() {
        &raw const PyBaseObject_Type
    } else {
        solid_base(base)
    };
    if shape_differs(type_, base) {
        type_
    } else {
        base
    }
}

/// `best_base` (typeobject.c): the base with the most-derived instance
/// layout, checking that every base is a type and an acceptable base.
/// Returns a borrowed reference.
fn best_base(bases: *mut PyObject) -> *mut PyTypeObject {
    // SAFETY: `bases` is a tuple of types by contract.
    let n = unsafe { types::tuple::PyTuple_Size(bases) };
    let mut winner: *const PyTypeObject = core::ptr::null();
    let mut base: *mut PyTypeObject = core::ptr::null_mut();
    for i in 0..n {
        // SAFETY: the item read is within the tuple.
        let base_proto = unsafe { types::tuple::PyTuple_GetItem(bases, i) };
        if !unsafe { pytype_check(base_proto) } {
            exceptions::set_string(exceptions::type_error(), b"bases must be types");
            return core::ptr::null_mut();
        }
        let base_i = base_proto as *mut PyTypeObject;
        // SAFETY: the base is a valid type object.
        if !unsafe { is_ready(base_i) } && unsafe { PyType_Ready(base_i) } < 0 {
            return core::ptr::null_mut();
        }
        // SAFETY: the flag read is within the type object.
        if unsafe { (*base_i).tp_flags } & coil_capi_sys::Py_TPFLAGS_BASETYPE == 0 {
            exceptions::set_string(
                exceptions::type_error(),
                b"type is not an acceptable base type",
            );
            return core::ptr::null_mut();
        }
        let candidate = solid_base(base_i);
        if winner.is_null() {
            winner = candidate;
            base = base_i;
        } else if unsafe {
            PyType_IsSubtype(winner as *mut PyTypeObject, candidate as *mut PyTypeObject)
        } > 0
        {
            // `winner` is a subtype of `candidate`: keep `winner`.
        } else if unsafe {
            PyType_IsSubtype(candidate as *mut PyTypeObject, winner as *mut PyTypeObject)
        } > 0
        {
            winner = candidate;
            base = base_i;
        } else {
            exceptions::set_string(
                exceptions::type_error(),
                b"multiple bases have instance lay-out conflict",
            );
            return core::ptr::null_mut();
        }
    }
    base
}

/// `_PyType_CalculateMetaclass` (typeobject.c): the most-derived metaclass
/// among `metatype` and the bases' metaclasses.
fn calculate_metaclass(metatype: *mut PyTypeObject, bases: *mut PyObject) -> *mut PyTypeObject {
    // SAFETY: `bases` is a tuple of types by contract.
    let nbases = unsafe { types::tuple::PyTuple_Size(bases) };
    let mut winner = metatype;
    for i in 0..nbases {
        // SAFETY: the item read is within the tuple.
        let tmp = unsafe { types::tuple::PyTuple_GetItem(bases, i) };
        // SAFETY: the header read is within the object.
        let tmptype = unsafe { header::type_of(tmp) };
        if unsafe { PyType_IsSubtype(winner, tmptype) } > 0 {
            continue;
        }
        if unsafe { PyType_IsSubtype(tmptype, winner) } > 0 {
            winner = tmptype;
            continue;
        }
        exceptions::set_string(
            exceptions::type_error(),
            b"metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases",
        );
        return core::ptr::null_mut();
    }
    winner
}

/// Build a one-element tuple holding a new reference to `item`.
fn pack_one(item: *mut PyObject) -> *mut PyObject {
    let tuple = types::tuple::PyTuple_New(1);
    if tuple.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `item` is a valid object.
    unsafe { exceptions::retain_object(item) };
    // SAFETY: the fresh tuple has one counted slot.
    unsafe {
        (*tuple.cast::<types::tuple::PyTupleObject>()).ob_item[0] = item;
    };
    tuple
}

/// `get_bases_tuple` (typeobject.c): normalise the `bases` argument to a
/// tuple of types. Returns a new reference.
fn get_bases_tuple(bases_in: *mut PyObject, spec: &PyType_Spec) -> *mut PyObject {
    if bases_in.is_null() {
        // Default: look in the spec, fall back to (object,).
        let mut base: *mut PyTypeObject = &raw mut PyBaseObject_Type;
        let mut bases: *mut PyObject = core::ptr::null_mut();
        // SAFETY: `spec->slots` is a null-terminated array by contract.
        unsafe {
            let mut slot = spec.slots;
            while (*slot).slot != 0 {
                match (*slot).slot {
                    Py_tp_base => base = (*slot).pfunc as *mut PyTypeObject,
                    Py_tp_bases => bases = (*slot).pfunc as *mut PyObject,
                    _ => {}
                }
                slot = slot.add(1);
            }
        }
        if bases.is_null() {
            return pack_one(base as *mut PyObject);
        }
        if unsafe { types::tuple::is_tuple(bases) } {
            // SAFETY: `bases` is a valid tuple.
            unsafe { exceptions::retain_object(bases) };
            return bases;
        }
        exceptions::set_string(exceptions::system_error(), b"Py_tp_bases is not a tuple");
        return core::ptr::null_mut();
    }
    if unsafe { types::tuple::is_tuple(bases_in) } {
        // SAFETY: `bases_in` is a valid tuple.
        unsafe { exceptions::retain_object(bases_in) };
        return bases_in;
    }
    pack_one(bases_in)
}

/// `set_tp_bases` (typeobject.c): store the bases tuple in the type. The
/// type takes ownership of the reference.
fn set_tp_bases(self_: *mut PyTypeObject, bases: *mut PyObject) {
    // SAFETY: the write is within the type object.
    unsafe { (*self_).tp_bases = bases };
}

/// `lookup_tp_dict` (typeobject.c): the type's dict.
fn lookup_tp_dict(self_: *mut PyTypeObject) -> *mut PyObject {
    // SAFETY: the read is within the type object.
    unsafe { (*self_).tp_dict }
}

/// `c_strlen`: the length of a NUL-terminated C string.
fn c_strlen(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    // SAFETY: `s` is a NUL-terminated C string by contract.
    unsafe { core::ffi::CStr::from_ptr(s) }.to_bytes().len()
}

/// The index of the last `.` in `bytes`, if any.
fn last_dot(bytes: &[u8]) -> Option<usize> {
    let mut last = None;
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'.' {
            last = Some(i);
        }
    }
    last
}

/// `find_signature` (typeobject.c): the start of the introspection
/// signature in `doc`, or null.
fn find_signature(name: *const c_char, doc: *const c_char) -> *const c_char {
    if doc.is_null() || name.is_null() {
        return core::ptr::null();
    }
    // SAFETY: `name` is a NUL-terminated C string by contract.
    let name_bytes = unsafe { core::ffi::CStr::from_ptr(name) }.to_bytes();
    let name = match last_dot(name_bytes) {
        Some(pos) => &name_bytes[pos + 1..],
        None => name_bytes,
    };
    // SAFETY: `doc` is a NUL-terminated C string by contract.
    let doc_cstr = unsafe { core::ffi::CStr::from_ptr(doc) };
    let doc_bytes = doc_cstr.to_bytes();
    if doc_bytes.len() < name.len() || &doc_bytes[..name.len()] != name {
        return core::ptr::null();
    }
    if doc_bytes[name.len()] != b'(' {
        return core::ptr::null();
    }
    // SAFETY: `doc` points into a valid C string; the offset is within it.
    unsafe { doc.add(name.len()) }
}

/// `skip_signature` (typeobject.c): the end of the introspection
/// signature, or null.
fn skip_signature(doc: *const c_char) -> *const c_char {
    const MARKER: &[u8] = b")\n--\n\n";
    // SAFETY: `doc` is a NUL-terminated C string by contract.
    let doc_bytes = unsafe { core::ffi::CStr::from_ptr(doc) }.to_bytes();
    for i in 0..doc_bytes.len() {
        if doc_bytes[i] == b'\n' && i + 1 < doc_bytes.len() && doc_bytes[i + 1] == b'\n' {
            return core::ptr::null();
        }
        if i + MARKER.len() <= doc_bytes.len() && &doc_bytes[i..i + MARKER.len()] == MARKER {
            // SAFETY: `doc` points into a valid C string; the offset is within it.
            return unsafe { doc.add(i + MARKER.len()) };
        }
    }
    core::ptr::null()
}

/// `_PyType_DocWithoutSignature` (typeobject.c): the docstring with the
/// introspection signature removed.
fn doc_without_signature(name: *const c_char, internal_doc: *const c_char) -> *const c_char {
    let doc = find_signature(name, internal_doc);
    if !doc.is_null() {
        let doc = skip_signature(doc);
        if !doc.is_null() {
            return doc;
        }
    }
    internal_doc
}

/// `check_basicsize_includes_size_and_offsets` (typeobject.c): verify the
/// type's `tp_basicsize` is large enough for its offsets.
fn check_basicsize_includes_size_and_offsets(type_: *mut PyTypeObject) -> bool {
    // SAFETY: the reads are within the type object.
    unsafe {
        let alloc = (*type_).tp_alloc;
        if alloc.is_none()
            || !core::ptr::fn_addr_eq(
                alloc.unwrap(),
                PyType_GenericAlloc as coil_capi_sys::allocfunc,
            )
        {
            // Custom allocators can ignore tp_basicsize.
            return true;
        }
        let max = (*type_).tp_basicsize;
        if !(*type_).tp_base.is_null() && (*(*type_).tp_base).tp_basicsize > (*type_).tp_basicsize {
            exceptions::set_string(
                exceptions::type_error(),
                b"tp_basicsize for type is too small for base",
            );
            return false;
        }
        if (*type_).tp_weaklistoffset + (core::mem::size_of::<*mut PyObject>() as isize) > max {
            exceptions::set_string(
                exceptions::type_error(),
                b"weaklist offset is out of bounds",
            );
            return false;
        }
        if (*type_).tp_dictoffset + (core::mem::size_of::<*mut PyObject>() as isize) > max {
            exceptions::set_string(exceptions::type_error(), b"dict offset is out of bounds");
            return false;
        }
        if (*type_).tp_vectorcall_offset + (core::mem::size_of::<*mut c_void>() as isize) > max {
            exceptions::set_string(
                exceptions::type_error(),
                b"vectorcall offset is out of bounds",
            );
            return false;
        }
    }
    true
}

/// `subtype_dealloc` (typeobject.c): the dealloc for heap types. Walks
/// `tp_base` to the nearest base with a different `tp_dealloc` and calls
/// it. The PEP 442 finalizer is already run by the central dealloc.
unsafe extern "C" fn subtype_dealloc(object: *mut PyObject) {
    // SAFETY: the object, its type and the dealloc chain are valid; the
    // block, when freed, is this allocator's.
    unsafe {
        let type_ = header::type_of(object);
        // Walk to the nearest base with a different tp_dealloc.
        let mut base = type_;
        loop {
            let basedealloc = (*base).tp_dealloc;
            if basedealloc.is_none()
                || !core::ptr::fn_addr_eq(
                    basedealloc.unwrap(),
                    subtype_dealloc as coil_capi_sys::destructor,
                )
            {
                break;
            }
            let next = (*base).tp_base;
            if next.is_null() {
                // The dealloc chain must terminate; a null base is a bug.
                crate::alloc::free_block(object.cast());
                return;
            }
            base = next;
        }
        let basedealloc = (*base).tp_dealloc;
        if let Some(dealloc) = basedealloc {
            dealloc(object);
        } else {
            crate::alloc::free_block(object.cast());
        }
    }
}

/// `_PyHeapType_GET_MEMBERS` (typeobject.c): the members array in the heap
/// type, at the metaclass's `tp_basicsize` past the start.
fn heap_type_get_members(res: *mut PyObject) -> *mut PyMemberDef {
    // SAFETY: the header read is within the type object.
    let metatype = unsafe { header::type_of(res as *const PyObject) };
    // SAFETY: the read is within the metatype.
    let basicsize = unsafe { (*metatype).tp_basicsize } as usize;
    // SAFETY: the offset is within the heap type's allocation.
    unsafe { res.cast::<u8>().add(basicsize).cast::<PyMemberDef>() }
}

/// `_PyType_FromMetaclass_impl` (typeobject.c): build a heap type from a
/// `PyType_Spec`.
unsafe fn from_metaclass_impl(
    metaclass: *mut PyTypeObject,
    module: *mut PyObject,
    spec: *mut PyType_Spec,
    bases_in: *mut PyObject,
    _allow_tp_new: c_int,
) -> *mut PyObject {
    // Invariant: a non-NULL value in one of these means this function holds
    // a strong reference or owns allocated memory; it is released at the end
    // on both success and error.
    let mut res: *mut PyObject = core::ptr::null_mut();
    let mut bases: *mut PyObject = core::ptr::null_mut();
    let mut tp_doc: *mut c_char = core::ptr::null_mut();
    let mut ht_name: *mut PyObject = core::ptr::null_mut();
    let mut _ht_tpname: *mut c_char = core::ptr::null_mut();

    let result = (|| -> *mut PyObject {
        // SAFETY: `spec` is a valid PyType_Spec by contract.
        let spec_ref = unsafe { &*spec };

        let mut nmembers: isize = 0;
        let mut weaklistoffset: isize = 0;
        let mut dictoffset: isize = 0;
        let mut vectorcalloffset: isize = 0;

        // Prepare slots that need special handling.
        // SAFETY: `spec->slots` is a null-terminated array by contract.
        unsafe {
            let mut slot = spec_ref.slots;
            while (*slot).slot != 0 {
                let slot_id = (*slot).slot;
                if slot_id < 0 || (slot_id as usize) >= PYSLOT_OFFSETS.len() {
                    exceptions::set_string(exceptions::runtime_error(), b"invalid slot offset");
                    return core::ptr::null_mut();
                }
                match slot_id {
                    Py_tp_members => {
                        if nmembers != 0 {
                            exceptions::set_string(
                                exceptions::system_error(),
                                b"Multiple Py_tp_members slots are not supported.",
                            );
                            return core::ptr::null_mut();
                        }
                        // SAFETY: `slot->pfunc` points to a null-terminated
                        // PyMemberDef array by contract.
                        let mut memb = (*slot).pfunc as *const PyMemberDef;
                        while !(*memb).name.is_null() {
                            nmembers += 1;
                            let name_bytes = core::ffi::CStr::from_ptr((*memb).name).to_bytes();
                            if name_bytes == b"__weaklistoffset__" {
                                weaklistoffset = (*memb).offset;
                            }
                            if name_bytes == b"__dictoffset__" {
                                dictoffset = (*memb).offset;
                            }
                            if name_bytes == b"__vectorcalloffset__" {
                                vectorcalloffset = (*memb).offset;
                            }
                            if (*memb).flags & Py_RELATIVE_OFFSET != 0 {
                                if spec_ref.basicsize > 0 {
                                    exceptions::set_string(
                                        exceptions::system_error(),
                                        b"With Py_RELATIVE_OFFSET, basicsize must be negative.",
                                    );
                                    return core::ptr::null_mut();
                                }
                                if (*memb).offset < 0
                                    || (*memb).offset >= -(spec_ref.basicsize as isize)
                                {
                                    exceptions::set_string(
                                        exceptions::system_error(),
                                        b"Member offset out of range (0..-basicsize)",
                                    );
                                    return core::ptr::null_mut();
                                }
                            }
                            memb = memb.add(1);
                        }
                    }
                    Py_tp_doc => {
                        if !tp_doc.is_null() {
                            exceptions::set_string(
                                exceptions::system_error(),
                                b"Multiple Py_tp_doc slots are not supported.",
                            );
                            return core::ptr::null_mut();
                        }
                        if (*slot).pfunc.is_null() {
                            crate::alloc::pymem::PyObject_Free(tp_doc as *mut c_void);
                            tp_doc = core::ptr::null_mut();
                        } else {
                            // SAFETY: `slot->pfunc` is a NUL-terminated C string.
                            let pfunc = (*slot).pfunc as *const c_char;
                            let len = c_strlen(pfunc) + 1;
                            tp_doc = crate::alloc::pymem::PyObject_Malloc(len) as *mut c_char;
                            if tp_doc.is_null() {
                                exceptions::PyErr_NoMemory();
                                return core::ptr::null_mut();
                            }
                            core::ptr::copy_nonoverlapping(pfunc, tp_doc, len);
                        }
                    }
                    _ => {}
                }
                slot = slot.add(1);
            }
        }

        // Prepare the type name and qualname.
        if spec_ref.name.is_null() {
            exceptions::set_string(
                exceptions::system_error(),
                b"Type spec does not define the name field.",
            );
            return core::ptr::null_mut();
        }
        // SAFETY: `spec->name` is a NUL-terminated C string.
        let name_bytes = unsafe { core::ffi::CStr::from_ptr(spec_ref.name) }.to_bytes();
        let s_bytes = match last_dot(name_bytes) {
            Some(pos) => &name_bytes[pos + 1..],
            None => name_bytes,
        };
        // SAFETY: the suffix points into `spec->name`, which is NUL-terminated.
        ht_name =
            unsafe { types::unicode::PyUnicode_FromString(s_bytes.as_ptr().cast::<c_char>()) };
        if ht_name.is_null() {
            return core::ptr::null_mut();
        }

        // Copy spec->name to a buffer we own.
        let name_buf_len = name_bytes.len() + 1;
        _ht_tpname = crate::alloc::pymem::PyMem_Malloc(name_buf_len) as *mut c_char;
        if _ht_tpname.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: the copy is within the allocated buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(spec_ref.name, _ht_tpname, name_buf_len);
        }

        // Get a tuple of bases.
        bases = get_bases_tuple(bases_in, spec_ref);
        if bases.is_null() {
            return core::ptr::null_mut();
        }

        // Calculate the metaclass.
        let mut metaclass = if metaclass.is_null() {
            &raw mut PyType_Type
        } else {
            metaclass
        };
        metaclass = calculate_metaclass(metaclass, bases);
        if metaclass.is_null() {
            return core::ptr::null_mut();
        }
        if !unsafe { pytype_check(metaclass as *const PyObject) } {
            exceptions::set_string(
                exceptions::type_error(),
                b"Metaclass is not a subclass of 'type'.",
            );
            return core::ptr::null_mut();
        }
        // A metaclass with a custom `tp_new` is not supported; the
        // deprecation warning CPython fires for the allowed case is
        // unreachable here (every metaclass in use is `type`).
        // SAFETY: the slot reads are within the type objects.
        let metatype_tp_new = unsafe { (*metaclass).tp_new };
        let type_tp_new = unsafe { PyType_Type.tp_new };
        if metatype_tp_new.is_some()
            && (type_tp_new.is_none()
                || !core::ptr::fn_addr_eq(metatype_tp_new.unwrap(), type_tp_new.unwrap()))
            && _allow_tp_new == 0
        {
            exceptions::set_string(
                exceptions::type_error(),
                b"Metaclasses with custom tp_new are not supported.",
            );
            return core::ptr::null_mut();
        }
        // The allowed case (custom tp_new with _allow_tp_new) fires a
        // deprecation warning in CPython; this runtime does not implement
        // `PyErr_WarnFormat` yet and the branch is unreachable at import.

        // Calculate best base, and check that all bases are type objects.
        let base = best_base(bases);
        if base.is_null() {
            return core::ptr::null_mut();
        }

        // Calculate sizes.
        let mut basicsize = spec_ref.basicsize as isize;
        let mut type_data_offset = spec_ref.basicsize as isize;
        if basicsize == 0 {
            // SAFETY: the read is within the type object.
            basicsize = unsafe { (*base).tp_basicsize };
        } else if basicsize < 0 {
            // SAFETY: the read is within the type object.
            type_data_offset = align_up(unsafe { (*base).tp_basicsize });
            basicsize = type_data_offset + align_up(-spec_ref.basicsize as isize);
            // SAFETY: the reads are within the type object.
            if unsafe { (*base).tp_itemsize != 0 }
                && (unsafe { (*base).tp_flags } | spec_ref.flags)
                    & coil_capi_sys::Py_TPFLAGS_ITEMS_AT_END
                    == 0
            {
                exceptions::set_string(
                    exceptions::system_error(),
                    b"Cannot extend variable-size class without Py_TPFLAGS_ITEMS_AT_END.",
                );
                return core::ptr::null_mut();
            }
        }
        let itemsize = spec_ref.itemsize as isize;

        // Allocate the new type.
        // SAFETY: the slot read is within the type object.
        let Some(alloc) = (unsafe { (*metaclass).tp_alloc }) else {
            exceptions::set_string(exceptions::system_error(), b"metaclass has no tp_alloc");
            return core::ptr::null_mut();
        };
        // SAFETY: the alloc call takes the metaclass and nmembers.
        res = unsafe { alloc(metaclass, nmembers) };
        if res.is_null() {
            return core::ptr::null_mut();
        }
        let res_start = res as *mut u8;
        let type_ = res as *mut PyTypeObject;

        // The flags must be initialized early, before the GC traverses us.
        // SAFETY: the write is within the type object.
        unsafe { (*type_).tp_flags = spec_ref.flags | Py_TPFLAGS_HEAPTYPE };

        // SAFETY: `module` is a valid object or null.
        unsafe {
            if !module.is_null() {
                exceptions::retain_object(module);
            }
            // Write ht_module at the heap-type offset.
            *(res_start.add(HT_MODULE).cast::<*mut PyObject>()) = module;
        }

        // Initialize essential fields.
        // SAFETY: the writes are within the heap type's allocation.
        unsafe {
            (*type_).tp_as_async = res_start.add(AS_ASYNC).cast::<PyAsyncMethods>();
            (*type_).tp_as_number = res_start.add(AS_NUMBER).cast::<PyNumberMethods>();
            (*type_).tp_as_sequence = res_start.add(AS_SEQUENCE).cast::<PySequenceMethods>();
            (*type_).tp_as_mapping = res_start.add(AS_MAPPING).cast::<PyMappingMethods>();
            (*type_).tp_as_buffer = res_start.add(AS_BUFFER).cast::<PyBufferProcs>();
        }

        // Set slots we have prepared.
        // SAFETY: `base` is a valid type object.
        unsafe { exceptions::retain_object(base as *mut PyObject) };
        // SAFETY: the write is within the type object.
        unsafe { (*type_).tp_base = base };
        set_tp_bases(type_, bases);
        bases = core::ptr::null_mut();

        // SAFETY: the write is within the type object.
        unsafe { (*type_).tp_doc = tp_doc as *const c_char };
        tp_doc = core::ptr::null_mut();

        // SAFETY: `ht_name` is a valid object.
        unsafe { exceptions::retain_object(ht_name) };
        // SAFETY: the writes are within the heap type's allocation.
        unsafe {
            *(res_start.add(HT_QUALNAME).cast::<*mut PyObject>()) = ht_name;
            *(res_start.add(HT_NAME).cast::<*mut PyObject>()) = ht_name;
        }
        ht_name = core::ptr::null_mut();

        // SAFETY: the writes are within the heap type's allocation.
        unsafe {
            (*type_).tp_name = _ht_tpname as *const c_char;
            *(res_start.add(HT_TPNAME).cast::<*mut c_char>()) = _ht_tpname;
        }
        _ht_tpname = core::ptr::null_mut();

        // Copy the sizes.
        // SAFETY: the writes are within the type object.
        unsafe {
            (*type_).tp_basicsize = basicsize;
            (*type_).tp_itemsize = itemsize;
        }

        // Copy all the ordinary slots.
        // SAFETY: `spec->slots` is a null-terminated array by contract.
        unsafe {
            let mut slot = spec_ref.slots;
            while (*slot).slot != 0 {
                let slot_id = (*slot).slot;
                match slot_id {
                    Py_tp_base | Py_tp_bases | Py_tp_doc => {
                        // Processed above.
                    }
                    Py_tp_members => {
                        let members = heap_type_get_members(res);
                        let count = nmembers as usize;
                        core::ptr::copy_nonoverlapping(
                            (*slot).pfunc as *const PyMemberDef,
                            members,
                            count,
                        );
                        // SAFETY: the write is within the type object.
                        (*type_).tp_members = members;
                        let mut memb = members;
                        for _ in 0..nmembers {
                            if (*memb).flags & Py_RELATIVE_OFFSET != 0 {
                                (*memb).flags &= !Py_RELATIVE_OFFSET;
                                (*memb).offset += type_data_offset;
                            }
                            memb = memb.add(1);
                        }
                    }
                    _ => {
                        let slotoffsets = &PYSLOT_OFFSETS[slot_id as usize];
                        if slotoffsets.subslot_offset == -1 {
                            // Set a slot in the main PyTypeObject.
                            *(res_start
                                .add(slotoffsets.slot_offset as usize)
                                .cast::<*mut c_void>()) = (*slot).pfunc;
                        } else {
                            let procs = *(res_start
                                .add(slotoffsets.slot_offset as usize)
                                .cast::<*mut c_void>())
                                as *mut u8;
                            *(procs
                                .add(slotoffsets.subslot_offset as usize)
                                .cast::<*mut c_void>()) = (*slot).pfunc;
                        }
                    }
                }
                slot = slot.add(1);
            }
        }

        // If tp_dealloc is NULL, set it to subtype_dealloc.
        // SAFETY: the read/write is within the type object.
        if unsafe { (*type_).tp_dealloc }.is_none() {
            // SAFETY: the write is within the type object.
            unsafe { (*type_).tp_dealloc = Some(subtype_dealloc) };
        }

        // Set up offsets.
        // SAFETY: the writes are within the type object.
        unsafe {
            (*type_).tp_vectorcall_offset = vectorcalloffset;
            (*type_).tp_weaklistoffset = weaklistoffset;
            (*type_).tp_dictoffset = dictoffset;
        }

        // Ready the type.
        // SAFETY: `type_` is a valid type object.
        if unsafe { PyType_Ready(type_) } < 0 {
            return core::ptr::null_mut();
        }

        if !check_basicsize_includes_size_and_offsets(type_) {
            return core::ptr::null_mut();
        }

        // CPython's `type_ready` installs `tp_dict`; this runtime's does not,
        // so give the heap type its own dict (the type owns the reference).
        // SAFETY: `type_` is a valid type object.
        if unsafe { (*type_).tp_dict }.is_null() {
            let d = types::dict::PyDict_New();
            if d.is_null() {
                return core::ptr::null_mut();
            }
            // SAFETY: `type_` is a valid type object.
            unsafe {
                (*type_).tp_dict = d;
            }
        }

        let dict = lookup_tp_dict(type_);

        // SAFETY: the type's doc and name, the dict, and every string and
        // reference created here are valid; `spec->name` is a C string.
        unsafe {
            if !(*type_).tp_doc.is_null() {
                let tp_name = (*type_).tp_name;
                let tp_doc = (*type_).tp_doc;
                let doc = doc_without_signature(tp_name, tp_doc);
                let __doc__ = types::unicode::PyUnicode_FromString(doc);
                if __doc__.is_null() {
                    return core::ptr::null_mut();
                }
                let doc_key = types::unicode::PyUnicode_FromString(c"__doc__".as_ptr());
                if doc_key.is_null() {
                    exceptions::release_object(__doc__);
                    return core::ptr::null_mut();
                }
                let r = types::dict::PyDict_SetItem(dict, doc_key, __doc__);
                exceptions::release_object(__doc__);
                exceptions::release_object(doc_key);
                if r < 0 {
                    return core::ptr::null_mut();
                }
            }

            // CPython's `type_ready` adds these offset members to the dict;
            // this runtime's does not, so only delete them when present.
            if weaklistoffset != 0 {
                let key = types::unicode::PyUnicode_FromString(c"__weaklistoffset__".as_ptr());
                if key.is_null() {
                    return core::ptr::null_mut();
                }
                let present = types::dict::PyDict_Contains(dict, key);
                if present > 0 && types::dict::PyDict_DelItem(dict, key) < 0 {
                    exceptions::release_object(key);
                    return core::ptr::null_mut();
                }
                exceptions::release_object(key);
            }
            if dictoffset != 0 {
                let key = types::unicode::PyUnicode_FromString(c"__dictoffset__".as_ptr());
                if key.is_null() {
                    return core::ptr::null_mut();
                }
                let present = types::dict::PyDict_Contains(dict, key);
                if present > 0 && types::dict::PyDict_DelItem(dict, key) < 0 {
                    exceptions::release_object(key);
                    return core::ptr::null_mut();
                }
                exceptions::release_object(key);
            }

            // Set type.__module__.
            let module_key = types::unicode::PyUnicode_FromString(c"__module__".as_ptr());
            if module_key.is_null() {
                return core::ptr::null_mut();
            }
            let r = types::dict::PyDict_Contains(dict, module_key);
            exceptions::release_object(module_key);
            if r < 0 {
                return core::ptr::null_mut();
            }
            if r == 0 {
                let name_bytes = core::ffi::CStr::from_ptr(spec_ref.name).to_bytes();
                if let Some(pos) = last_dot(name_bytes) {
                    let modname =
                        types::unicode::PyUnicode_FromStringAndSize(spec_ref.name, pos as isize);
                    if modname.is_null() {
                        return core::ptr::null_mut();
                    }
                    let module_key = types::unicode::PyUnicode_FromString(c"__module__".as_ptr());
                    if module_key.is_null() {
                        exceptions::release_object(modname);
                        return core::ptr::null_mut();
                    }
                    let r = types::dict::PyDict_SetItem(dict, module_key, modname);
                    exceptions::release_object(modname);
                    exceptions::release_object(module_key);
                    if r != 0 {
                        return core::ptr::null_mut();
                    }
                }
                // If the name has no dot, CPython fires a deprecation warning;
                // this runtime's specs all carry a module prefix.
            }
        }

        res
    })();

    // Finally: release what we still own on both success and error.
    // SAFETY: each non-null value is a reference or block we still own.
    unsafe {
        if result.is_null() && !res.is_null() {
            exceptions::release_object(res);
        }
        if !bases.is_null() {
            exceptions::release_object(bases);
        }
        if !tp_doc.is_null() {
            crate::alloc::pymem::PyObject_Free(tp_doc as *mut c_void);
        }
        if !ht_name.is_null() {
            exceptions::release_object(ht_name);
        }
        if !_ht_tpname.is_null() {
            crate::alloc::pymem::PyMem_Free(_ht_tpname as *mut c_void);
        }
    }
    result
}

/// `PyType_FromMetaclass`: build a heap type with an explicit metaclass.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromMetaclass(
    metaclass: *mut PyTypeObject,
    module: *mut PyObject,
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller's contract.
    unsafe { from_metaclass_impl(metaclass, module, spec, bases, 0) }
}

/// `PyType_FromModuleAndSpec`: build a heap type from a module and spec.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromModuleAndSpec(
    module: *mut PyObject,
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller's contract.
    unsafe { from_metaclass_impl(core::ptr::null_mut(), module, spec, bases, 1) }
}

/// `PyType_FromSpecWithBases`: build a heap type from a spec and bases.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSpecWithBases(
    spec: *mut PyType_Spec,
    bases: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the caller's contract.
    unsafe { from_metaclass_impl(core::ptr::null_mut(), core::ptr::null_mut(), spec, bases, 1) }
}

/// `PyType_FromSpec`: build a heap type from a spec.
///
/// # Safety
///
/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_FromSpec(spec: *mut PyType_Spec) -> *mut PyObject {
    // SAFETY: the caller's contract.
    unsafe {
        from_metaclass_impl(
            core::ptr::null_mut(),
            core::ptr::null_mut(),
            spec,
            core::ptr::null_mut(),
            1,
        )
    }
}

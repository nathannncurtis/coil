//! `tuple`: CPython 3.12's `PyTupleObject` — the exact layout whose
//! inline `PyTuple_SET_ITEM`/`PyTuple_GET_ITEM`/`PyTuple_GET_SIZE` access
//! works by offsets alone — the xxHash-derived hash, element-wise rich
//! comparison, the immortal empty tuple, and the `PyTuple_*` C API entry
//! points (spec sections 8.2 and 8.5).
//!
//! `tp_new` stays unset until argument parsing exists, so readying marks
//! the type non-instantiable, as r05 did for `int`.

use core::ffi::{c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC, Py_TPFLAGS_ITEMS_AT_END,
    Py_TPFLAGS_SEQUENCE, Py_TPFLAGS_TUPLE_SUBCLASS, PyObject, PyTypeObject, PyVarObject,
};

use crate::object::exceptions;
use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{
    Py_EQ, Py_GE, Py_GT, Py_LE, Py_LT, Py_NE, PyType_GenericAlloc, zero_type,
};
use crate::types::{Cmp, object_hash, objects_equal, rich_compare};

/// `tuple`'s private extra flag (`_Py_TPFLAGS_MATCH_SELF`).
const PY_TPFLAGS_MATCH_SELF: u32 = 1 << 22;

/// CPython 3.12's `PyTupleObject`: the var-object header then the inline
/// item array at `ob_item` (spec section 8.2's exact class). Tuples are
/// immutable, so there is no Coil lock tail.
#[repr(C)]
pub struct PyTupleObject {
    pub ob_base: PyVarObject,
    pub ob_item: [*mut PyObject; 1],
}

/// The immortal empty tuple (`_Py_SINGLETON(tuple_empty)`); `PyTuple_New`
/// hands it out for size 0, so `() is ()` holds as in CPython 3.12.
#[unsafe(no_mangle)]
pub static mut _PyEmpty_TupleStruct: PyTupleObject = PyTupleObject {
    ob_base: PyVarObject {
        ob_base: coil_capi_sys::PyObject {
            ob_refcnt: header::IMMORTAL_REFCNT,
            ob_type: &raw mut PyTuple_Type,
        },
        ob_size: 0,
    },
    ob_item: [core::ptr::null_mut()],
};

/// The empty tuple singleton.
pub fn empty_tuple() -> *mut PyObject {
    (&raw mut _PyEmpty_TupleStruct).cast()
}

/// `PyTuple_Check` by the fast subclass flag.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_tuple(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: the flag read is within the type object.
    !ty.is_null() && unsafe { (*ty).tp_flags } & Py_TPFLAGS_TUPLE_SUBCLASS != 0
}

/// `tuple_alloc`: a zeroed block of `size` item slots with the header and
/// `ob_size` written. Null with `MemoryError` set.
fn tuple_alloc(size: isize) -> *mut PyTupleObject {
    // SAFETY: `PyTuple_Type` is static and readied by init.
    let object = unsafe { PyType_GenericAlloc(&raw mut PyTuple_Type, size) };
    if object.is_null() {
        return core::ptr::null_mut();
    }
    let tuple = object.cast::<PyTupleObject>();
    // SAFETY: the var-object header is the first 24 bytes of the fresh,
    // exclusively owned block; `GenericAlloc` leaves `ob_size` zero.
    unsafe { (*tuple).ob_base.ob_size = size };
    tuple
}

/// Build a tuple copying `items`; the empty case returns the singleton
/// (CPython's `_PyTuple_FromArray`).
pub(crate) fn from_array(items: &[*mut PyObject]) -> *mut PyObject {
    if items.is_empty() {
        return empty_tuple();
    }
    let tuple = tuple_alloc(items.len() as isize);
    if tuple.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block holds `items.len()` slots.
    unsafe {
        core::ptr::copy_nonoverlapping(items.as_ptr(), (*tuple).ob_item.as_mut_ptr(), items.len());
    }
    tuple.cast()
}

unsafe extern "C" fn tuple_dealloc(object: *mut PyObject) {
    // Tuples live on the GC heap (the collector's sweep handles items).
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

unsafe extern "C" fn tuple_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut c_void,
) -> c_int {
    let tuple = object.cast::<PyTupleObject>();
    // SAFETY: every item read is within the tuple's allocation.
    unsafe {
        let size = (*tuple).ob_base.ob_size as usize;
        for i in 0..size {
            let item = (*tuple).ob_item.as_ptr().add(i).read();
            if !item.is_null() && visit(item, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

/// CPython 3.12's tuple hash: the xxHash-derived fold of the items' hashes
/// (`tupleobject.c`'s `tuplehash`), keeping the historical `hash(())`.
unsafe extern "C" fn tuplehash(v: *mut PyObject) -> isize {
    const XXPRIME_1: u64 = 11400714785074694791;
    const XXPRIME_2: u64 = 14029467366897019727;
    const XXPRIME_5: u64 = 2870177450012600261;

    let tuple = v.cast::<PyTupleObject>();
    // SAFETY: the size and item reads are within the tuple's allocation.
    let len = unsafe { (*tuple).ob_base.ob_size } as usize;
    let items = unsafe { core::slice::from_raw_parts((*tuple).ob_item.as_ptr(), len) };
    let mut acc: u64 = XXPRIME_5;
    for item in items {
        // SAFETY: items of a visible tuple are valid objects.
        let lane = unsafe { object_hash(*item) } as u64;
        if lane == u64::MAX {
            // The item's hash raised; the error is already set.
            return -1;
        }
        acc = acc.wrapping_add(lane.wrapping_mul(XXPRIME_2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(XXPRIME_1);
    }
    // Add the length, mangled to keep hash(())'s historical value.
    acc = acc.wrapping_add((len as u64) ^ (XXPRIME_5 ^ 3527539));
    if acc == u64::MAX {
        return 1546275796;
    }
    acc as isize
}

/// CPython's `tuplerichcompare`: find the first unequal element, then
/// compare lengths or that element with the requested operator.
unsafe extern "C" fn tuplerichcompare(
    v: *mut PyObject,
    w: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: flag reads within the type objects.
    unsafe {
        if !is_tuple(v) || !is_tuple(w) {
            return singletons::not_implemented();
        }
        let vt = v.cast::<PyTupleObject>();
        let wt = w.cast::<PyTupleObject>();
        let vlen = (*vt).ob_base.ob_size;
        let wlen = (*wt).ob_base.ob_size;
        let vitems = core::slice::from_raw_parts((*vt).ob_item.as_ptr(), vlen.max(0) as usize);
        let witems = core::slice::from_raw_parts((*wt).ob_item.as_ptr(), wlen.max(0) as usize);
        let mut i = 0;
        while i < vitems.len() && i < witems.len() {
            match objects_equal(vitems[i], witems[i]) {
                Ok(true) => i += 1,
                Ok(false) => break,
                Err(()) => return core::ptr::null_mut(),
            }
        }
        if i >= vitems.len() || i >= witems.len() {
            // No differing element: compare sizes.
            let ordering = vlen.cmp(&wlen);
            return match (op, ordering) {
                (Py_EQ, core::cmp::Ordering::Equal)
                | (Py_LE, core::cmp::Ordering::Less)
                | (Py_LE, core::cmp::Ordering::Equal)
                | (Py_GE, core::cmp::Ordering::Greater)
                | (Py_GE, core::cmp::Ordering::Equal) => singletons::true_(),
                _ => singletons::false_(),
            };
        }
        if op == Py_EQ {
            return singletons::false_();
        }
        if op == Py_NE {
            return singletons::true_();
        }
        // Compare the differing element with the proper operator
        // (`rich_compare` tries the reflected operator itself).
        match rich_compare(vitems[i], witems[i], op) {
            Cmp::Bool(true) => singletons::true_(),
            Cmp::Bool(false) => singletons::false_(),
            Cmp::NotImplemented => {
                let a = crate::util::cstr((*header::type_of(vitems[i])).tp_name);
                let b = crate::util::cstr((*header::type_of(witems[i])).tp_name);
                crate::tstate::set_string(
                    exceptions::type_error(),
                    format!(
                        "'{}' not supported between instances of '{}' and '{}'",
                        op_name(op),
                        a,
                        b
                    )
                    .as_bytes(),
                );
                core::ptr::null_mut()
            }
            Cmp::Error => core::ptr::null_mut(),
        }
    }
}

/// The symbolic name of a rich-comparison operator, for error messages.
fn op_name(op: c_int) -> &'static str {
    match op {
        Py_LT => "<",
        Py_GT => ">",
        Py_LE => "<=",
        Py_GE => ">=",
        Py_EQ => "==",
        _ => "!=",
    }
}

const fn tuple_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"tuple".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::tuple::OB_ITEM as isize;
    t.tp_itemsize = core::mem::size_of::<*mut PyObject>() as isize;
    t.tp_dealloc = Some(tuple_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_HAVE_GC
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_TUPLE_SUBCLASS
        | Py_TPFLAGS_SEQUENCE
        | PY_TPFLAGS_MATCH_SELF
        | Py_TPFLAGS_ITEMS_AT_END;
    t.tp_traverse = Some(tuple_traverse);
    t.tp_richcompare = Some(tuplerichcompare);
    t.tp_hash = Some(tuplehash);
    t.tp_iter = Some(tuple_iter);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `tuple` type object (CPython's `PyTuple_Type`).
#[unsafe(no_mangle)]
pub static mut PyTuple_Type: PyTypeObject = tuple_type();

// ------------------------------------------------------------- C API entry

/// See the CPython 3.12 contract for this entry point: a tuple with NULL
/// items the caller must fill before exposing it; the empty tuple is the
/// shared singleton.
#[unsafe(no_mangle)]
pub extern "C" fn PyTuple_New(size: isize) -> *mut PyObject {
    if size < 0 {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    if size == 0 {
        return empty_tuple();
    }
    tuple_alloc(size).cast()
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be null or a tuple object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_Size(op: *mut PyObject) -> isize {
    if !(unsafe { is_tuple(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the var-object header is the first 24 bytes.
    unsafe { (*op.cast::<PyTupleObject>()).ob_base.ob_size }
}

/// See the CPython 3.12 contract for this entry point: a borrowed item.
///
/// # Safety
///
/// `op` must be a tuple object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_GetItem(op: *mut PyObject, i: isize) -> *mut PyObject {
    // SAFETY: `PyTuple_GetItemWithError` performs the same checks.
    unsafe { PyTuple_GetItemWithError(op, i) }
}

/// See the CPython 3.12 contract for this entry point: a borrowed item,
/// with errors reported through the thread state.
///
/// # Safety
///
/// `op` must be a tuple object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_GetItemWithError(op: *mut PyObject, i: isize) -> *mut PyObject {
    if !(unsafe { is_tuple(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the size read is within the tuple.
    let size = unsafe { (*op.cast::<PyTupleObject>()).ob_base.ob_size };
    if i < 0 || i >= size {
        crate::tstate::set_string(exceptions::index_error(), b"tuple index out of range");
        return core::ptr::null_mut();
    }
    // SAFETY: the item read is within the tuple's allocation.
    unsafe {
        *(*op.cast::<PyTupleObject>())
            .ob_item
            .as_ptr()
            .add(i as usize)
    }
}

/// See the CPython 3.12 contract for this entry point: only NULL slots
/// may be assigned (the caller is still building the tuple).
///
/// # Safety
///
/// `op` must be a tuple object; `newitem` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_SetItem(
    op: *mut PyObject,
    i: isize,
    newitem: *mut PyObject,
) -> c_int {
    if !(unsafe { is_tuple(op) }) || i < 0 {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    let tuple = op.cast::<PyTupleObject>();
    // SAFETY: the size read is within the tuple.
    if i >= unsafe { (*tuple).ob_base.ob_size } {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // Immutable tuples carry no lock; the contract's NULL-slot rule makes
    // the store uncontended by construction.
    // SAFETY: the item write is within the tuple's allocation.
    unsafe { (*tuple).ob_item.as_mut_ptr().add(i as usize).write(newitem) };
    0
}

/// See the CPython 3.12 contract for this entry point: a new tuple of the
/// borrowed slice, with CPython's clamping.
///
/// # Safety
///
/// `op` must be a tuple object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyTuple_GetSlice(
    op: *mut PyObject,
    ilow: isize,
    ihigh: isize,
) -> *mut PyObject {
    if !(unsafe { is_tuple(op) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the size read is within the tuple.
    let size = unsafe { (*op.cast::<PyTupleObject>()).ob_base.ob_size };
    let ilow = ilow.clamp(0, size);
    let ihigh = ihigh.clamp(ilow, size);
    // SAFETY: the header read follows the type check.
    if ilow == 0
        && ihigh == size
        && core::ptr::eq(unsafe { header::type_of(op) }, &raw mut PyTuple_Type)
    {
        return op;
    }
    // SAFETY: the slice reads are within the tuple's allocation.
    let items = unsafe {
        core::slice::from_raw_parts(
            (*op.cast::<PyTupleObject>())
                .ob_item
                .as_ptr()
                .add(ilow as usize),
            (ihigh - ilow) as usize,
        )
    };
    from_array(items)
}

/// See the CPython 3.12 contract for this entry point: resize a tuple
/// still exclusively owned by the caller.
///
/// # Safety
///
/// `pv` must point to a caller-owned exact tuple (the empty singleton
/// when its size is 0); the new tuple is stored back through `pv`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyTuple_Resize(pv: *mut *mut PyObject, newsize: isize) -> c_int {
    // SAFETY: `pv` is the caller's slot for the tuple.
    let v = unsafe { *pv };
    // SAFETY: the header read is within the object.
    let size = if v.is_null() {
        -1
    } else {
        unsafe { (*v.cast::<PyTupleObject>()).ob_base.ob_size }
    };
    // SAFETY: the header read is within the object.
    let exact = !v.is_null() && core::ptr::eq(unsafe { header::type_of(v) }, &raw mut PyTuple_Type);
    if !exact || (size != 0 && unsafe { header::refcnt(v) } != 1) || newsize < 0 {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        // SAFETY: the caller's slot again.
        unsafe { *pv = core::ptr::null_mut() };
        return -1;
    }
    if size == newsize {
        return 0;
    }
    if newsize == 0 {
        if size != 0 {
            // SAFETY: `v` came from `PyType_GenericAlloc`.
            unsafe { crate::alloc::free_block(v.cast()) };
        }
        // SAFETY: the caller's slot again.
        unsafe { *pv = empty_tuple() };
        return 0;
    }
    if size == 0 {
        // The empty tuple is static and never resized in place.
        // SAFETY: the fresh tuple goes straight into the caller's slot.
        unsafe { *pv = PyTuple_New(newsize) };
        // SAFETY: the caller's slot again.
        return if unsafe { *pv }.is_null() { -1 } else { 0 };
    }
    // SAFETY: `v` came from `PyType_GenericAlloc`.
    let fresh = unsafe {
        crate::alloc::realloc_block(
            v.cast(),
            coil_rt_abi::cp312::tuple::OB_ITEM
                + core::mem::size_of::<*mut PyObject>() * newsize as usize,
        )
    };
    if fresh.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return -1;
    }
    let tuple = fresh.cast::<PyTupleObject>();
    // SAFETY: the fresh block covers the new size; growth is zeroed.
    unsafe { (*tuple).ob_base.ob_size = newsize };
    // SAFETY: the caller's slot again.
    unsafe { *pv = tuple.cast() };
    0
}

// ------------------------------------------------------------- iteration

/// CPython's `_PyTupleIterObject` (a free-layout object: reached from C
/// only through functions).
#[repr(C)]
struct PyTupleIterObject {
    ob_base: PyObject,
    it_index: isize,
    it_seq: *mut PyObject,
}

unsafe extern "C" fn tuple_iter_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

unsafe extern "C" fn tuple_iter_traverse(
    object: *mut PyObject,
    visit: coil_capi_sys::visitproc,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    let it = object.cast::<PyTupleIterObject>();
    // SAFETY: the field read is within the iterator's allocation.
    unsafe {
        let seq = (*it).it_seq;
        if !seq.is_null() && visit(seq, arg) != 0 {
            return -1;
        }
    }
    0
}

/// `PyObject_SelfIter` for the iterator types.
pub(crate) unsafe extern "C" fn self_iter(object: *mut PyObject) -> *mut PyObject {
    object
}

unsafe extern "C" fn tuple_iter_iternext(object: *mut PyObject) -> *mut PyObject {
    let it = object.cast::<PyTupleIterObject>();
    // SAFETY: the iterator fields are within the allocation.
    unsafe {
        let seq = (*it).it_seq;
        if seq.is_null() {
            return core::ptr::null_mut();
        }
        let size = (*seq.cast::<PyTupleObject>()).ob_base.ob_size;
        if (*it).it_index < size {
            let item = *(*seq.cast::<PyTupleObject>())
                .ob_item
                .as_ptr()
                .add((*it).it_index as usize);
            (*it).it_index += 1;
            return item;
        }
        (*it).it_seq = core::ptr::null_mut();
        core::ptr::null_mut()
    }
}

const fn tuple_iter_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"tuple_iterator".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyTupleIterObject>() as isize;
    t.tp_dealloc = Some(tuple_iter_dealloc);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC;
    t.tp_traverse = Some(tuple_iter_traverse);
    t.tp_iter = Some(self_iter);
    t.tp_iternext = Some(tuple_iter_iternext);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `tuple_iterator` type (CPython's `PyTupleIter_Type`).
static mut TUPLE_ITER_TYPE: PyTypeObject = tuple_iter_type();

/// The `tuple_iterator` type object, for init.
pub(crate) fn tuple_iter_type_ptr() -> *mut PyTypeObject {
    &raw mut TUPLE_ITER_TYPE
}

/// `tuple_iter` (`tuple.__iter__`).
unsafe extern "C" fn tuple_iter(seq: *mut PyObject) -> *mut PyObject {
    if !(unsafe { is_tuple(seq) }) {
        crate::tstate::set_string(
            exceptions::system_error(),
            b"bad argument to internal function",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: `TUPLE_ITER_TYPE` is static; init readies it.
    let it =
        unsafe { PyType_GenericAlloc(&raw mut TUPLE_ITER_TYPE, 0) }.cast::<PyTupleIterObject>();
    if it.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block is exclusively ours.
    unsafe {
        (*it).it_index = 0;
        (*it).it_seq = seq;
    }
    it.cast()
}

//! Static type objects and their initialisation: `PyType_Ready`, the C3
//! method resolution order and `tp_*` slot inheritance as CPython 3.12's
//! `typeobject.c` does it (spec sections 8.2 and 8.3).
//!
//! Three steps of CPython's `type_ready` are deferred until the container
//! types exist: `type_ready_fill_dict` (descriptors need dict and str) and
//! the `tp_bases`/`tp_mro`/`tp_subclasses` tuples. The C3 linearisation is
//! computed here and kept in a side table; the container tasks materialise
//! it as real tuples in those slots. Until then `tp_mro` stays null and
//! `mro_of` is the lookup path.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use core::ffi::{c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_DISALLOW_INSTANTIATION, Py_TPFLAGS_HAVE_GC,
    Py_TPFLAGS_HAVE_VECTORCALL, Py_TPFLAGS_ITEMS_AT_END, Py_TPFLAGS_READY,
    Py_TPFLAGS_TYPE_SUBCLASS, PyMemberDef, PyObject, PyTypeObject, freefunc, hashfunc, visitproc,
};

use super::exceptions;
use super::header;
use crate::types;
use crate::util;

/// Rich-comparison opcodes (object.h); macros in C, so not in
/// coil-capi-sys.
pub const Py_LT: c_int = 0;
pub const Py_LE: c_int = 1;
pub const Py_EQ: c_int = 2;
pub const Py_NE: c_int = 3;
pub const Py_GT: c_int = 4;
pub const Py_GE: c_int = 5;

/// `_Py_TPFLAGS_MATCH_SELF` (object.h's private bit 22): pattern matching
/// may compare by value; CPython 3.12 gives it to `int` and `float` alone
/// among this runtime's types.
pub const _Py_TPFLAGS_MATCH_SELF: u32 = 1 << 22;

/// A zeroed `PyTypeObject` to build static initialisers from: every slot
/// unset, `PyType_Ready` fills the rest. Static types are born immortal —
/// CPython asserts they already are before readying them
/// (`_PyStaticType_InitBuiltin`), and spec section 8.1 says every type
/// object is immortal.
pub const fn zero_type() -> PyTypeObject {
    PyTypeObject {
        ob_refcnt: header::IMMORTAL_REFCNT,
        ob_type: core::ptr::null_mut(),
        ob_size: 0,
        tp_name: core::ptr::null(),
        tp_basicsize: 0,
        tp_itemsize: 0,
        tp_dealloc: None,
        tp_vectorcall_offset: 0,
        tp_getattr: None,
        tp_setattr: None,
        tp_as_async: core::ptr::null_mut(),
        tp_repr: None,
        tp_as_number: core::ptr::null_mut(),
        tp_as_sequence: core::ptr::null_mut(),
        tp_as_mapping: core::ptr::null_mut(),
        tp_hash: None,
        tp_call: None,
        tp_str: None,
        tp_getattro: None,
        tp_setattro: None,
        tp_as_buffer: core::ptr::null_mut(),
        tp_flags: 0,
        tp_doc: core::ptr::null(),
        tp_traverse: None,
        tp_clear: None,
        tp_richcompare: None,
        tp_weaklistoffset: 0,
        tp_iter: None,
        tp_iternext: None,
        tp_methods: core::ptr::null_mut(),
        tp_members: core::ptr::null_mut(),
        tp_getset: core::ptr::null_mut(),
        tp_base: core::ptr::null_mut(),
        tp_dict: core::ptr::null_mut(),
        tp_descr_get: None,
        tp_descr_set: None,
        tp_dictoffset: 0,
        tp_init: None,
        tp_alloc: None,
        tp_new: None,
        tp_free: None,
        tp_is_gc: None,
        tp_bases: core::ptr::null_mut(),
        tp_mro: core::ptr::null_mut(),
        tp_cache: core::ptr::null_mut(),
        tp_subclasses: core::ptr::null_mut(),
        tp_weaklist: core::ptr::null_mut(),
        tp_del: None,
        tp_version_tag: 0,
        tp_finalize: None,
        tp_vectorcall: None,
        tp_watched: 0,
    }
}

/// The C3 linearisations this runtime computed, keyed by type address, in
/// the slot `tp_mro` will point at once tuples exist (see the module docs).
fn mro_table() -> &'static Mutex<HashMap<usize, Vec<usize>>> {
    static TABLE: OnceLock<Mutex<HashMap<usize, Vec<usize>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The MRO of a readied type, or its `tp_base` chain when the type was
/// readied before this table existed. Static types are single-inheritance,
/// so the two agree.
///
/// # Safety
///
/// `t` must be null or a pointer to a `PyTypeObject`.
pub unsafe fn mro_of(t: *mut PyTypeObject) -> Vec<*mut PyTypeObject> {
    if t.is_null() {
        return Vec::new();
    }
    if let Some(mro) = mro_table()
        .lock()
        .expect("mro table poisoned")
        .get(&(t as usize))
    {
        return mro.iter().map(|a| *a as *mut PyTypeObject).collect();
    }
    // SAFETY: each `tp_base` read is within a type object.
    let mut chain = Vec::new();
    let mut cur = t;
    while !cur.is_null() {
        chain.push(cur);
        cur = unsafe { (*cur).tp_base };
    }
    chain
}

/// `PyType_IsSubtype`: whether `b` occurs in `a`'s MRO.
///
/// # Safety
///
/// `a` and `b` must be null or pointers to `PyTypeObject`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_IsSubtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> c_int {
    // SAFETY: the MRO lookup only reads type addresses.
    unsafe { mro_of(a).contains(&b) as c_int }
}

/// `PyFloat_Check`-style subtype tests over the `tp_base` chain — the MRO
/// of the static types this runtime has (heap types arrive with section 8.3).
///
/// # Safety
///
/// `a` must be null or a pointer to a `PyTypeObject` whose `tp_base` chain
/// is well-formed.
pub(crate) unsafe fn chain_is_subtype(a: *mut PyTypeObject, b: *mut PyTypeObject) -> bool {
    // SAFETY: each `tp_base` read is within a type object.
    let mut cur = a;
    while !cur.is_null() {
        if core::ptr::eq(cur, b) {
            return true;
        }
        cur = unsafe { (*cur).tp_base };
    }
    false
}

/// CPython's `c3_merge` over the MRO fragments of the direct bases plus the
/// bases list itself. An inconsistent order carries CPython's message.
fn c3_merge(mut seqs: Vec<Vec<*mut PyTypeObject>>) -> Result<Vec<*mut PyTypeObject>, String> {
    let mut result = Vec::new();
    seqs.retain(|seq| !seq.is_empty());
    while !seqs.is_empty() {
        let mut chosen = None;
        for seq in &seqs {
            let head = seq[0];
            let in_tail = seqs
                .iter()
                .any(|other| !core::ptr::eq(other, seq) && other[1..].contains(&head));
            if !in_tail {
                chosen = Some(head);
                break;
            }
        }
        let Some(head) = chosen else {
            return Err("Cannot create a consistent method resolution order (MRO)".into());
        };
        result.push(head);
        for seq in &mut seqs {
            if seq.first() == Some(&head) {
                seq.remove(0);
            }
        }
        seqs.retain(|seq| !seq.is_empty());
    }
    Ok(result)
}

/// The linearisation of `type` over `bases` (CPython's `c3_linearization`):
/// the type followed by the C3 merge of the bases' MROs and the bases
/// themselves. Static types pass their single base; `type.__new__` will
/// pass the full bases tuple.
///
/// # Safety
///
/// Every entry of `bases` must point to a readied `PyTypeObject`.
pub unsafe fn c3_linearization(
    type_: *mut PyTypeObject,
    bases: &[*mut PyTypeObject],
) -> Result<Vec<*mut PyTypeObject>, String> {
    // SAFETY: every base was readied, so `mro_of` reads initialized MROs.
    let mut seqs: Vec<Vec<*mut PyTypeObject>> =
        bases.iter().map(|b| unsafe { mro_of(*b) }).collect();
    seqs.push(bases.to_vec());
    let merged = c3_merge(seqs)?;
    Ok([type_].into_iter().chain(merged).collect())
}

/// Pointer-derived default hash (pyhash.c's `_PyHashPointer`): rotate right
/// by 4 so the zero low bits of aligned pointers do not collide, with -1
/// mapped to -2.
///
/// # Safety
///
/// `object` is treated as an address only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_HashPointer(object: *const c_void) -> isize {
    let y = (object as usize).rotate_right(4);
    if y as isize == -1 { -2 } else { y as isize }
}

/// `PyObject_HashNotImplemented`: the slot `type_ready` installs on types
/// that must be unhashable.
///
/// # Safety
///
/// `object` must point to a valid `PyObject` header.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_HashNotImplemented(object: *mut PyObject) -> isize {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(object) };
    let name = if ty.is_null() {
        "?"
    } else {
        // SAFETY: `tp_name` of a readied type is a valid C string.
        unsafe { util::cstr((*ty).tp_name) }
    };
    let message = format!("unhashable type: '{name}'");
    crate::tstate::set_string(exceptions::type_error(), message.as_bytes());
    -1
}

unsafe extern "C" fn object_dealloc(object: *mut PyObject) {
    // SAFETY: the header read is within the object.
    unsafe {
        let ty = header::type_of(object);
        if let Some(free) = (*ty).tp_free {
            free(object.cast());
            return;
        }
        // A type without `tp_free` was allocated on the GC heap.
        crate::alloc::free_block(object.cast());
    }
}

pub(crate) unsafe extern "C" fn object_identity_richcompare(
    a: *mut PyObject,
    b: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // `object_richcompare`: `==` is identity and otherwise NotImplemented so
    // the reflected operand gets its chance (issue #1393); `!=` delegates to
    // the type's own `==` slot and inverts it unless that answered
    // NotImplemented; the ordering comparisons always defer.
    if op == Py_EQ {
        return if core::ptr::eq(a, b) {
            super::singletons::true_()
        } else {
            super::singletons::not_implemented()
        };
    }
    if op == Py_NE {
        // SAFETY: the header read is within the object.
        let ty = unsafe { header::type_of(a) };
        // SAFETY: the type's slot takes any objects.
        return match unsafe { (*ty).tp_richcompare } {
            None => super::singletons::not_implemented(),
            Some(eq) => {
                // SAFETY: the slot is the type's richcompare.
                let res = unsafe { eq(a, b, Py_EQ) };
                if res.is_null() || core::ptr::eq(res, super::singletons::not_implemented()) {
                    return res;
                }
                // The `==` slots reachable here answer True or False only,
                // so inverting is the pointer test; `PyObject_IsTrue`'s
                // general form needs the bool protocol.
                if core::ptr::eq(res, super::singletons::true_()) {
                    super::singletons::false_()
                } else {
                    super::singletons::true_()
                }
            }
        };
    }
    super::singletons::not_implemented()
}

unsafe extern "C" fn object_new(
    type_: *mut PyTypeObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> *mut PyObject {
    // The calling machinery always passes a tuple and a dict or null; only
    // the sizes matter here.
    // SAFETY: layout reads of the argument containers.
    let excess = unsafe { util::var_size(args) > 0 || util::dict_used(kwds) > 0 };
    // SAFETY: field reads within the type object.
    unsafe {
        if excess || (*type_).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION != 0 {
            let name = util::cstr((*type_).tp_name);
            let message = if excess {
                format!("{name}() takes no arguments")
            } else {
                format!("cannot create '{name}' instances")
            };
            crate::tstate::set_string(exceptions::type_error(), message.as_bytes());
            return core::ptr::null_mut();
        }
        match (*type_).tp_alloc {
            Some(alloc) => alloc(type_, 0),
            None => {
                let name = util::cstr((*type_).tp_name);
                let message = format!("cannot create '{name}' instances");
                crate::tstate::set_string(exceptions::type_error(), message.as_bytes());
                core::ptr::null_mut()
            }
        }
    }
}

unsafe extern "C" fn object_init(
    _self: *mut PyObject,
    args: *mut PyObject,
    kwds: *mut PyObject,
) -> c_int {
    // SAFETY: layout reads of the argument containers.
    if unsafe { util::var_size(args) > 0 || util::dict_used(kwds) > 0 } {
        crate::tstate::set_string(
            exceptions::type_error(),
            b"object.__init__() takes exactly one argument (the instance to initialize)",
        );
        return -1;
    }
    0
}

/// `PyType_GenericAlloc` for static sizes: a zeroed GC-heap block with the
/// header written. GC bookkeeping for counted-regime objects is the memory
/// task's; this path hands out tracing-regime memory (spec section 9).
///
/// # Safety
///
/// `type_` must be a readied type; the result is freed by its `tp_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_GenericAlloc(
    type_: *mut PyTypeObject,
    nitems: isize,
) -> *mut PyObject {
    // SAFETY: the size reads are within the type object.
    let (basicsize, itemsize) = unsafe { ((*type_).tp_basicsize, (*type_).tp_itemsize) };
    let Some(size) = basicsize.checked_add(itemsize * nitems.max(0)) else {
        return core::ptr::null_mut();
    };
    let size = size as usize;
    let block = crate::alloc::alloc_object(size);
    if block.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    // SAFETY: a fresh zeroed block of `size` bytes.
    unsafe { header::c_init(block.cast(), type_) };
    block.cast()
}

unsafe extern "C" fn type_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: the referents are PyObject pointers or null.
    unsafe {
        let ty = object.cast::<PyTypeObject>();
        for referent in [
            (*ty).tp_dict,
            (*ty).tp_cache,
            (*ty).tp_mro,
            (*ty).tp_bases,
            (*ty).tp_base.cast(),
            (*ty).tp_subclasses.cast(),
        ] {
            if !referent.is_null() && visit(referent, arg) != 0 {
                return -1;
            }
        }
    }
    0
}

unsafe extern "C" fn type_dealloc(object: *mut PyObject) {
    // Static type objects are immortal; reaching their dealloc means a
    // stray decref, which resets the count the way CPython does.
    // SAFETY: the header write is within the object.
    unsafe { (*object).ob_refcnt = header::IMMORTAL_REFCNT };
}

const fn object_type() -> PyTypeObject {
    let mut t = zero_type();
    // CPython's initialiser pins it statically; only `type`'s self
    // reference needs init's help.
    t.ob_type = &raw mut PyType_Type;
    t.tp_name = c"object".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::object::SIZEOF as isize;
    t.tp_dealloc = Some(object_dealloc);
    t.tp_richcompare = Some(object_identity_richcompare);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
    t.tp_hash = Some(hash_pointer);
    t.tp_init = Some(object_init);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_new = Some(object_new);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// `hashfunc`-shaped wrapper for `_Py_HashPointer`.
unsafe extern "C" fn hash_pointer(object: *mut PyObject) -> isize {
    // SAFETY: the pointer is passed through unchanged.
    unsafe { _Py_HashPointer(object.cast()) }
}

/// `type_new` stands in as the spec's stub (section 10.1): creating classes
/// arrives with the heap-type machinery, and the slot must stay filled so
/// `type_ready_set_new` leaves `type` instantiation-shaped as in CPython.
unsafe extern "C" fn type_new(
    _type_: *mut PyTypeObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: type_new is not implemented",
    );
    core::ptr::null_mut()
}

const fn type_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"type".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::heaptypeobject::SIZEOF as isize;
    t.tp_itemsize = core::mem::size_of::<PyMemberDef>() as isize;
    t.tp_dealloc = Some(type_dealloc);
    t.tp_vectorcall_offset = coil_rt_abi::cp312::typeobject::TP_VECTORCALL as isize;
    t.tp_flags = Py_TPFLAGS_DEFAULT
        | Py_TPFLAGS_HAVE_GC
        | Py_TPFLAGS_BASETYPE
        | Py_TPFLAGS_TYPE_SUBCLASS
        | Py_TPFLAGS_ITEMS_AT_END;
    t.tp_traverse = Some(type_traverse);
    t.tp_weaklistoffset = coil_rt_abi::cp312::typeobject::TP_WEAKLIST as isize;
    t.tp_dictoffset = coil_rt_abi::cp312::typeobject::TP_DICT as isize;
    // CPython's initialiser carries type_new here; without a filled slot
    // ready_set_new would disallow instantiation, which CPython's `type`
    // never is.
    t.tp_new = Some(type_new);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// `object` (CPython's `PyBaseObject_Type`).
#[unsafe(no_mangle)]
pub static mut PyBaseObject_Type: PyTypeObject = object_type();

/// `type` itself.
#[unsafe(no_mangle)]
pub static mut PyType_Type: PyTypeObject = type_type();

/// `type`'s `ob_type` points at itself; a static initialiser cannot spell
/// that relocation inside the struct, so init fixes it before anything is
/// readied.
fn fix_type_self_reference() {
    // SAFETY: init runs once before any concurrent access can exist.
    unsafe {
        PyType_Type.ob_type = &raw mut PyType_Type;
    }
}

/// Whether `t`'s ready bit is set.
///
/// # Safety
///
/// `t` must be null or a pointer to a `PyTypeObject`.
unsafe fn is_ready(t: *mut PyTypeObject) -> bool {
    // SAFETY: the flag read is within the type object.
    !t.is_null() && unsafe { (*t).tp_flags } & Py_TPFLAGS_READY != 0
}

/// CPython's `type_ready_set_base`: default the base to `object` and ready
/// it first. False on failure with the exception set.
///
/// # Safety
///
/// `t` must be a pointer to a `PyTypeObject` being readied.
unsafe fn ready_set_base(t: *mut PyTypeObject) -> bool {
    // SAFETY: field accesses are within the type object.
    unsafe {
        let mut base = (*t).tp_base;
        if base.is_null() && !core::ptr::eq(t, &raw mut PyBaseObject_Type) {
            base = &raw mut PyBaseObject_Type;
            (*t).tp_base = base;
        }
        if !base.is_null() && !is_ready(base) && type_ready_impl(base) < 0 {
            return false;
        }
    }
    true
}

/// CPython's `type_ready_set_type`: default `ob_type` to the base's.
///
/// # Safety
///
/// `t` must be a pointer to a `PyTypeObject` being readied.
unsafe fn ready_set_type(t: *mut PyTypeObject) {
    // SAFETY: field accesses are within the type object.
    unsafe {
        if (*t).ob_type.is_null() && !(*t).tp_base.is_null() {
            (*t).ob_type = (*(*t).tp_base).ob_type;
        }
    }
}

/// CPython's `type_ready_mro`: the C3 linearisation, registered in the side
/// table (see the module docs).
///
/// # Safety
///
/// `t` must be a pointer to a `PyTypeObject` being readied with a readied
/// base.
unsafe fn ready_mro(t: *mut PyTypeObject) -> bool {
    // SAFETY: `t` points to a type object being readied.
    let base = unsafe { (*t).tp_base };
    let bases: Vec<*mut PyTypeObject> = if base.is_null() {
        Vec::new()
    } else {
        vec![base]
    };
    // SAFETY: `bases` holds the readied base.
    match unsafe { c3_linearization(t, &bases) } {
        Ok(mro) => {
            mro_table()
                .lock()
                .expect("mro table poisoned")
                .insert(t as usize, mro.iter().map(|a| *a as usize).collect());
            true
        }
        Err(message) => {
            crate::tstate::set_string(exceptions::type_error(), message.as_bytes());
            false
        }
    }
}

/// CPython's `type_ready_set_new`: a static type whose only base is
/// `object` and that defines no `tp_new` must not become callable; the
/// rest inherit the base's `tp_new`.
///
/// # Safety
///
/// `t` must be a pointer to a `PyTypeObject` being readied.
unsafe fn ready_set_new(t: *mut PyTypeObject) {
    // SAFETY: field accesses are within the type object.
    unsafe {
        if (*t).tp_new.is_none()
            && core::ptr::eq((*t).tp_base, &raw mut PyBaseObject_Type)
            && (*t).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION == 0
        {
            (*t).tp_flags |= Py_TPFLAGS_DISALLOW_INSTANTIATION;
        }
        if (*t).tp_flags & Py_TPFLAGS_DISALLOW_INSTANTIATION == 0 {
            if (*t).tp_new.is_none() && !(*t).tp_base.is_null() {
                (*t).tp_new = (*(*t).tp_base).tp_new;
            }
        } else {
            (*t).tp_new = None;
        }
    }
}

/// CPython's `inherit_special`: flags, sizes and offsets that come from the
/// dominant base, plus the fast subclass flag of the base. The remaining
/// fast-subclass flags (bytes, unicode) are checked as those types land.
/// Sets have no fast-subclass flag; their checks subtype against the two
/// type objects directly.
///
/// # Safety
///
/// `type_` and `base` must be pointers to type objects, `base` readied.
unsafe fn inherit_special(type_: *mut PyTypeObject, base: *mut PyTypeObject) {
    // SAFETY: field accesses are within the type objects.
    unsafe {
        if (*type_).tp_flags & Py_TPFLAGS_HAVE_GC == 0
            && (*base).tp_flags & Py_TPFLAGS_HAVE_GC != 0
            && (*type_).tp_traverse.is_none()
            && (*type_).tp_clear.is_none()
        {
            (*type_).tp_flags |= Py_TPFLAGS_HAVE_GC;
            (*type_).tp_traverse = (*base).tp_traverse;
            (*type_).tp_clear = (*base).tp_clear;
        }
        (*type_).tp_flags |= (*base).tp_flags
            & (coil_capi_sys::Py_TPFLAGS_MANAGED_WEAKREF | coil_capi_sys::Py_TPFLAGS_MANAGED_DICT);
        if (*type_).tp_basicsize == 0 {
            (*type_).tp_basicsize = (*base).tp_basicsize;
        }
        if (*type_).tp_itemsize == 0 {
            (*type_).tp_itemsize = (*base).tp_itemsize;
        }
        if (*type_).tp_weaklistoffset == 0 {
            (*type_).tp_weaklistoffset = (*base).tp_weaklistoffset;
        }
        if (*type_).tp_dictoffset == 0 {
            (*type_).tp_dictoffset = (*base).tp_dictoffset;
        }
        if PyType_IsSubtype(base, exceptions::base_exception_type()) != 0 {
            (*type_).tp_flags |= coil_capi_sys::Py_TPFLAGS_BASE_EXC_SUBCLASS;
        } else if PyType_IsSubtype(base, &raw mut PyType_Type) != 0 {
            (*type_).tp_flags |= Py_TPFLAGS_TYPE_SUBCLASS;
        } else if PyType_IsSubtype(base, &raw mut types::long::PyLong_Type) != 0 {
            (*type_).tp_flags |= coil_capi_sys::Py_TPFLAGS_LONG_SUBCLASS;
        } else if PyType_IsSubtype(base, &raw mut types::tuple::PyTuple_Type) != 0 {
            (*type_).tp_flags |= coil_capi_sys::Py_TPFLAGS_TUPLE_SUBCLASS;
        } else if PyType_IsSubtype(base, &raw mut types::list::PyList_Type) != 0 {
            (*type_).tp_flags |= coil_capi_sys::Py_TPFLAGS_LIST_SUBCLASS;
        } else if PyType_IsSubtype(base, &raw mut types::dict::PyDict_Type) != 0 {
            (*type_).tp_flags |= coil_capi_sys::Py_TPFLAGS_DICT_SUBCLASS;
        }
    }
}

/// Whether `base`'s slot is its own rather than one its base passed down
/// (CPython's `SLOTDEFINED`); `base_of_base` is null when `base` has none.
fn slot_defined<T: PartialEq>(slot: &Option<T>, base_of_base: &Option<T>) -> bool {
    match (slot, base_of_base) {
        (Some(value), Some(inherited)) => value != inherited,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

macro_rules! copy_struct_slots {
    ($dst:expr, $src:expr, $basebase:expr, $($slot:ident),* $(,)?) => {
        $(
            if $dst.$slot.is_none()
                && slot_defined(&$src.$slot, &$basebase.as_ref().and_then(|b| b.$slot))
            {
                $dst.$slot = $src.$slot;
            }
        )*
    };
}

/// CPython's `inherit_slots` for one base: every unset slot whose value the
/// base defines — itself, not merely inherited from its own base — is
/// copied, with the pairing rules of `typeobject.c` for attributes,
/// comparisons and the GC-conditional `tp_free`.
///
/// # Safety
///
/// `type_` and `base` must be pointers to type objects.
unsafe fn inherit_slots(type_: *mut PyTypeObject, base: *mut PyTypeObject) {
    // SAFETY: field accesses are within the type objects.
    unsafe {
        let basebase = (*base).tp_base;

        if !(*type_).tp_as_number.is_null() && !(*base).tp_as_number.is_null() {
            // SAFETY: both pointers address valid structs of the type's own.
            let dst = &mut *(*type_).tp_as_number;
            let src = &*(*base).tp_as_number;
            let basebase_struct = if basebase.is_null() {
                None
            } else {
                (*basebase).tp_as_number.as_ref()
            };
            copy_struct_slots!(
                dst,
                src,
                basebase_struct,
                nb_add,
                nb_subtract,
                nb_multiply,
                nb_remainder,
                nb_divmod,
                nb_power,
                nb_negative,
                nb_positive,
                nb_absolute,
                nb_bool,
                nb_invert,
                nb_lshift,
                nb_rshift,
                nb_and,
                nb_xor,
                nb_or,
                nb_int,
                nb_float,
                nb_inplace_add,
                nb_inplace_subtract,
                nb_inplace_multiply,
                nb_inplace_remainder,
                nb_inplace_power,
                nb_inplace_lshift,
                nb_inplace_rshift,
                nb_inplace_and,
                nb_inplace_xor,
                nb_inplace_or,
                nb_floor_divide,
                nb_true_divide,
                nb_inplace_floor_divide,
                nb_inplace_true_divide,
                nb_index,
                nb_matrix_multiply,
                nb_inplace_matrix_multiply,
            );
        }

        if !(*type_).tp_as_async.is_null() && !(*base).tp_as_async.is_null() {
            // SAFETY: both pointers address valid structs of the type's own.
            let dst = &mut *(*type_).tp_as_async;
            let src = &*(*base).tp_as_async;
            let basebase_struct = if basebase.is_null() {
                None
            } else {
                (*basebase).tp_as_async.as_ref()
            };
            copy_struct_slots!(
                dst,
                src,
                basebase_struct,
                am_await,
                am_aiter,
                am_anext,
                am_send,
            );
        }

        if !(*type_).tp_as_sequence.is_null() && !(*base).tp_as_sequence.is_null() {
            // SAFETY: both pointers address valid structs of the type's own.
            let dst = &mut *(*type_).tp_as_sequence;
            let src = &*(*base).tp_as_sequence;
            let basebase_struct = if basebase.is_null() {
                None
            } else {
                (*basebase).tp_as_sequence.as_ref()
            };
            copy_struct_slots!(
                dst,
                src,
                basebase_struct,
                sq_length,
                sq_concat,
                sq_repeat,
                sq_item,
                sq_ass_item,
                sq_contains,
                sq_inplace_concat,
                sq_inplace_repeat,
            );
        }

        if !(*type_).tp_as_mapping.is_null() && !(*base).tp_as_mapping.is_null() {
            // SAFETY: both pointers address valid structs of the type's own.
            let dst = &mut *(*type_).tp_as_mapping;
            let src = &*(*base).tp_as_mapping;
            let basebase_struct = if basebase.is_null() {
                None
            } else {
                (*basebase).tp_as_mapping.as_ref()
            };
            copy_struct_slots!(
                dst,
                src,
                basebase_struct,
                mp_length,
                mp_subscript,
                mp_ass_subscript,
            );
        }

        if !(*type_).tp_as_buffer.is_null() && !(*base).tp_as_buffer.is_null() {
            // SAFETY: both pointers address valid structs of the type's own.
            let dst = &mut *(*type_).tp_as_buffer;
            let src = &*(*base).tp_as_buffer;
            let basebase_struct = if basebase.is_null() {
                None
            } else {
                (*basebase).tp_as_buffer.as_ref()
            };
            copy_struct_slots!(dst, src, basebase_struct, bf_getbuffer, bf_releasebuffer,);
        }

        macro_rules! copy_slot {
            ($slot:ident) => {{
                if (*type_).$slot.is_none()
                    && slot_defined(
                        &(*base).$slot,
                        &basebase.as_ref().map(|b| b.$slot).flatten(),
                    )
                {
                    (*type_).$slot = (*base).$slot;
                }
            }};
        }
        copy_slot!(tp_dealloc);
        if (*type_).tp_getattr.is_none() && (*type_).tp_getattro.is_none() {
            (*type_).tp_getattr = (*base).tp_getattr;
            (*type_).tp_getattro = (*base).tp_getattro;
        }
        if (*type_).tp_setattr.is_none() && (*type_).tp_setattro.is_none() {
            (*type_).tp_setattr = (*base).tp_setattr;
            (*type_).tp_setattro = (*base).tp_setattro;
        }
        copy_slot!(tp_repr);
        // Always inherit `tp_vectorcall_offset` so `PyVectorcall_Call`
        // works; the flag follows only when `tp_call` is not overridden.
        if (*type_).tp_vectorcall_offset == 0 {
            (*type_).tp_vectorcall_offset = (*base).tp_vectorcall_offset;
        }
        if (*type_).tp_call.is_none() && (*base).tp_flags & Py_TPFLAGS_HAVE_VECTORCALL != 0 {
            (*type_).tp_flags |= Py_TPFLAGS_HAVE_VECTORCALL;
        }
        copy_slot!(tp_call);
        copy_slot!(tp_str);
        if (*type_).tp_richcompare.is_none() && (*type_).tp_hash.is_none() {
            (*type_).tp_richcompare = (*base).tp_richcompare;
            (*type_).tp_hash = (*base).tp_hash;
        }
        copy_slot!(tp_iter);
        copy_slot!(tp_iternext);
        copy_slot!(tp_descr_get);
        copy_slot!(tp_descr_set);
        if (*type_).tp_dictoffset == 0 {
            (*type_).tp_dictoffset = (*base).tp_dictoffset;
        }
        copy_slot!(tp_init);
        copy_slot!(tp_alloc);
        copy_slot!(tp_is_gc);
        copy_slot!(tp_finalize);
        if (*type_).tp_flags & Py_TPFLAGS_HAVE_GC == (*base).tp_flags & Py_TPFLAGS_HAVE_GC {
            if (*type_).tp_free.is_none() {
                (*type_).tp_free = (*base).tp_free;
            }
        } else if (*type_).tp_flags & Py_TPFLAGS_HAVE_GC != 0
            && (*type_).tp_free.is_none()
            && (*base).tp_free.map(|f| f as usize)
                == Some({
                    let default: freefunc = crate::alloc::pymem::PyObject_Free;
                    default as usize
                })
        {
            // The base's default `tp_free` becomes the GC-heap free below
            // once `PyObject_GC_Del` exists; the allocator is the same.
            (*type_).tp_free = Some(crate::alloc::pymem::PyObject_Free);
        }
    }
}

/// CPython's `type_ready_inherit_as_structs`: a static type without its own
/// `tp_as_*` struct points at its base's, so a subtype can add slots by
/// installing its own struct without touching the base's.
///
/// # Safety
///
/// `t` must be a pointer to a type object with a readied base.
unsafe fn ready_inherit_as_structs(t: *mut PyTypeObject) {
    // SAFETY: field accesses are within the type objects.
    unsafe {
        let base = (*t).tp_base;
        if !base.is_null() {
            if (*t).tp_as_async.is_null() {
                (*t).tp_as_async = (*base).tp_as_async;
            }
            if (*t).tp_as_number.is_null() {
                (*t).tp_as_number = (*base).tp_as_number;
            }
            if (*t).tp_as_sequence.is_null() {
                (*t).tp_as_sequence = (*base).tp_as_sequence;
            }
            if (*t).tp_as_mapping.is_null() {
                (*t).tp_as_mapping = (*base).tp_as_mapping;
            }
            if (*t).tp_as_buffer.is_null() {
                (*t).tp_as_buffer = (*base).tp_as_buffer;
            }
        }
    }
}

/// CPython's `type_ready_set_hash`: a type still without `tp_hash` after
/// inheritance is unhashable.
///
/// # Safety
///
/// `t` must be a pointer to a type object.
unsafe fn ready_set_hash(t: *mut PyTypeObject) {
    // SAFETY: the field access is within the type object.
    unsafe {
        if (*t).tp_hash.is_none() {
            (*t).tp_hash = Some(PyObject_HashNotImplemented as hashfunc);
        }
    }
}

/// CPython's `type_ready_post_checks`: GC types must traverse. False on
/// failure with the exception set.
///
/// # Safety
///
/// `t` must be a pointer to a type object.
unsafe fn ready_post_checks(t: *mut PyTypeObject) -> bool {
    // SAFETY: the flag and slot reads are within the type object.
    unsafe {
        if (*t).tp_flags & Py_TPFLAGS_HAVE_GC != 0 && (*t).tp_traverse.is_none() {
            let name = util::cstr((*t).tp_name);
            let message =
                format!("type {name} has the Py_TPFLAGS_HAVE_GC flag but has no traverse function");
            crate::tstate::set_string(exceptions::system_error(), message.as_bytes());
            return false;
        }
    }
    true
}

/// CPython's `type_ready`; see the module docs for the steps deferred to
/// the container-type tasks.
///
/// # Safety
///
/// `t` must be null or a pointer to a `PyTypeObject` that no other thread
/// is touching.
unsafe fn type_ready_impl(t: *mut PyTypeObject) -> c_int {
    // SAFETY: all accesses stay within `t` and its readied bases.
    unsafe {
        if t.is_null() {
            return -1;
        }
        if (*t).tp_flags & Py_TPFLAGS_READY != 0 {
            return 0;
        }
        if (*t).tp_name.is_null() {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"Type does not define the tp_name field.",
            );
            return -1;
        }
        if !ready_set_base(t) || !ready_mro(t) {
            return -1;
        }
        ready_set_type(t);
        ready_set_new(t);
        let base = (*t).tp_base;
        if !base.is_null() {
            inherit_special(t, base);
        }
        for entry in mro_of(t).into_iter().skip(1) {
            inherit_slots(t, entry);
        }
        ready_inherit_as_structs(t);
        ready_set_hash(t);
        if !ready_post_checks(t) {
            return -1;
        }
        (*t).tp_flags |= Py_TPFLAGS_READY;
    }
    0
}

/// See the CPython 3.12 contract for this entry point: finish a type
/// object's initialisation — base, MRO, inherited slots and flags.
///
/// # Safety
///
/// `t` must be null or a pointer to a `PyTypeObject` no other thread is
/// touching; re-readying a prepared type returns immediately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyType_Ready(t: *mut PyTypeObject) -> c_int {
    static SELF_REFERENCE: OnceLock<()> = OnceLock::new();
    SELF_REFERENCE.get_or_init(fix_type_self_reference);
    // SAFETY: single-threaded under the v1 GIL; see the safety contract.
    unsafe { type_ready_impl(t) }
}

/// Ready the static types of this runtime in dependency order. Idempotent;
/// the runtime's init (and the tests) call it before any type is used.
pub fn init_static_types() -> c_int {
    // SAFETY: init runs before any concurrent access can exist, and each
    // type is readied once through its READY flag.
    unsafe {
        let objects = [
            &raw mut PyBaseObject_Type,
            &raw mut PyType_Type,
            &raw mut types::long::PyLong_Type,
            &raw mut types::bool_::PyBool_Type,
            &raw mut types::float_::PyFloat_Type,
            &raw mut types::complex::PyComplex_Type,
            super::singletons::none_type(),
            super::singletons::ellipsis_type(),
            super::singletons::not_implemented_type(),
            exceptions::base_exception_type(),
            exceptions::type_error_type(),
            exceptions::value_error_type(),
            exceptions::overflow_error_type(),
            exceptions::zero_division_error_type(),
            exceptions::memory_error_type(),
            exceptions::system_error_type(),
            exceptions::not_implemented_error_type(),
            exceptions::index_error_type(),
            exceptions::key_error_type(),
            exceptions::runtime_error_type(),
            &raw mut types::tuple::PyTuple_Type,
            &raw mut types::list::PyList_Type,
            &raw mut types::dict::PyDict_Type,
            &raw mut types::set::PySet_Type,
            &raw mut types::set::PyFrozenSet_Type,
            types::tuple::tuple_iter_type_ptr(),
            types::list::list_iter_type_ptr(),
            types::set::set_iter_type_ptr(),
        ];
        for t in objects {
            if PyType_Ready(t) < 0 {
                return -1;
            }
        }
        for t in types::dict::dict_static_types() {
            if PyType_Ready(t) < 0 {
                return -1;
            }
        }
        materialize_type_tuples();
    }
    0
}

/// `type_ready`'s deferred last step: fill `tp_mro` and `tp_bases` with
/// real tuples built from the side table (see the module docs). Runs once
/// every static type is readied, since building the tuples needs
/// `PyTuple_Type`. `tp_subclasses` stays null — only heap subclasses fill
/// it, and none exist yet.
fn materialize_type_tuples() {
    // SAFETY: the table holds exactly the readied types, and their MROs
    // and bases are valid type pointers.
    unsafe {
        let entries: Vec<(usize, Vec<usize>)> = mro_table()
            .lock()
            .expect("mro table poisoned")
            .iter()
            .map(|(ty, mro)| (*ty, mro.clone()))
            .collect();
        for (ty_addr, mro) in entries {
            let ty = ty_addr as *mut PyTypeObject;
            let mro_items: Vec<*mut PyObject> = mro.iter().map(|a| *a as *mut PyObject).collect();
            let base = (*ty).tp_base;
            let base_items: Vec<*mut PyObject> = if base.is_null() {
                Vec::new()
            } else {
                vec![base.cast()]
            };
            // SAFETY: the items are valid type pointers, and the fresh
            // tuples are built before any of the slots becomes visible.
            (*ty).tp_mro = types::tuple::from_array(&mro_items);
            (*ty).tp_bases = types::tuple::from_array(&base_items);
        }
    }
}

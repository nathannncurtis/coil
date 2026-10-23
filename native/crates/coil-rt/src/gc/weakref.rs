//! Weak references: CPython 3.12's exact `PyWeakReference` layout, the
//! weak-reference table of section 9.2 — the runtime-side registry that
//! keeps weakrefs discoverable without making them strong references —
//! and phase 6, which clears weakrefs to dead objects and runs their
//! callbacks in `handle_weakrefs` order.

use core::ffi::{c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, Py_TPFLAGS_HAVE_GC, PyObject, PyTypeObject, visitproc,
};

use crate::gc::mark::{self, MarkState};
use crate::object::header;
use crate::object::typeobject::{PyType_GenericAlloc, zero_type};

/// CPython 3.12's `PyWeakReference` (weakrefobject.h): the header, the
/// referent, the callback, the hash and the per-referent list links. In
/// this runtime the links stay unused — the weak-reference table holds the
/// lists — but the layout is byte-for-byte CPython's, as spec section 8.2
/// requires for the exact class.
#[repr(C)]
pub struct PyWeakReference {
    pub ob_base: PyObject,
    pub wr_object: *mut PyObject,
    pub wr_callback: *mut PyObject,
    pub wr_hash: isize,
    pub wr_prev: *mut PyWeakReference,
    pub wr_next: *mut PyWeakReference,
}

/// One row of the weak-reference table: a weakref and its referent, in
/// creation order — the order `handle_weakrefs` clears and calls back in.
#[derive(Clone)]
struct Row {
    referent: usize,
    weakref: usize,
}

/// The weak-reference table (section 9.2): creation-ordered rows plus the
/// per-referent index. A root: every live weakref object is marked from
/// it, which is what keeps weakrefs — and through `tp_traverse` their
/// callbacks — alive until they have fired.
struct Table {
    rows: Vec<Row>,
}

fn table() -> &'static std::sync::Mutex<Table> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<Table>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(Table { rows: Vec::new() }))
}

/// Whether `object`'s type supports weak references (CPython's
/// `PyType_SUPPORTS_WEAKREFS`: a managed weakref flag or a weaklist
/// offset).
///
/// # Safety
///
/// `object` must be null or point to a valid `PyObject` header.
unsafe fn supports_weakrefs(object: *mut PyObject) -> bool {
    if object.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(object) };
    if ty.is_null() {
        return false;
    }
    // SAFETY: the field reads are within the type object.
    unsafe {
        (*ty).tp_weaklistoffset != 0
            || (*ty).tp_flags & coil_capi_sys::Py_TPFLAGS_MANAGED_WEAKREF != 0
    }
}

unsafe extern "C" fn weakref_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut c_void,
) -> c_int {
    // CPython's `weakref_traverse`: the callback only, never the
    // referent — the whole point of a weak reference.
    // SAFETY: the field read is within the weakref.
    let callback = unsafe { (*object.cast::<PyWeakReference>()).wr_callback };
    if !callback.is_null() {
        // SAFETY: the slot contract.
        return unsafe { visit(callback, arg) };
    }
    0
}

unsafe extern "C" fn weakref_dealloc(object: *mut PyObject) {
    // The collector owns the block; the sweep frees weakrefs like any
    // other dead object, and the next collection purges their rows.
    // SAFETY: the object came from `PyType_GenericAlloc`.
    unsafe { crate::alloc::free_block(object.cast()) };
}

unsafe extern "C" fn weakref_hash(object: *mut PyObject) -> isize {
    // CPython's `weakref_hash`: the live referent's hash; cleared
    // weakrefs hash like `None`.
    // SAFETY: the field read is within the weakref.
    let wr = unsafe { &*object.cast::<PyWeakReference>() };
    let referent = wr.wr_object;
    if core::ptr::eq(referent, crate::object::singletons::none()) {
        return 0xFCA8_6420;
    }
    // SAFETY: `wr_object` is a live object or `None` between collections.
    unsafe { crate::types::object_hash(referent) }
}

const fn weakref_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"weakref".as_ptr();
    t.tp_basicsize = core::mem::size_of::<PyWeakReference>() as isize;
    t.tp_dealloc = Some(weakref_dealloc);
    t.tp_hash = Some(weakref_hash);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_HAVE_GC | Py_TPFLAGS_BASETYPE;
    t.tp_traverse = Some(weakref_traverse);
    t.tp_alloc = Some(PyType_GenericAlloc);
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// The `weakref` type object (CPython's `PyWeakRef_Type`).
#[unsafe(no_mangle)]
pub static mut PyWeakRef_Type: PyTypeObject = weakref_type();

/// Whether `op` is a weakref (CPython's `PyWeakref_Check`).
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub unsafe fn is_weakref(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    core::ptr::eq(ty, &raw mut PyWeakRef_Type)
}

/// See the CPython 3.12 contract for this entry point: a weak reference to
/// `ob`, or a callback-carrying one when `callback` is non-null. NULL when
/// `ob` does not support weak references (`TypeError` set) or the callback
/// is not callable.
///
/// # Safety
///
/// `ob` must be null-checkable and a valid object; `callback` must be null
/// or a valid object of a readied type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_NewRef(
    ob: *mut PyObject,
    callback: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the header and type reads are within valid objects.
    unsafe {
        if !supports_weakrefs(ob) {
            let name = crate::util::cstr((*header::type_of(ob)).tp_name);
            let message = format!("cannot create weak reference to '{name}' object");
            crate::tstate::set_string(crate::types::exc::type_error(), message.as_bytes());
            return core::ptr::null_mut();
        }
        if !callback.is_null() {
            let ty = header::type_of(callback);
            if ty.is_null() || (*ty).tp_call.is_none() {
                crate::tstate::set_string(
                    crate::types::exc::type_error(),
                    b"callback must be callable",
                );
                return core::ptr::null_mut();
            }
        }
        let object = PyType_GenericAlloc(&raw mut PyWeakRef_Type, 0);
        if object.is_null() {
            return core::ptr::null_mut();
        }
        let wr = object.cast::<PyWeakReference>();
        (*wr).wr_object = ob;
        (*wr).wr_callback = callback;
        (*wr).wr_hash = -1;
        (*wr).wr_prev = core::ptr::null_mut();
        (*wr).wr_next = core::ptr::null_mut();
        table()
            .lock()
            .expect("weak-reference table poisoned")
            .rows
            .push(Row {
                referent: ob as usize,
                weakref: object as usize,
            });
        object
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `reference` must be null or a weakref object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_GetRef(
    reference: *mut PyObject,
    pref: *mut *mut PyObject,
) -> c_int {
    if pref.is_null() {
        return -1;
    }
    // SAFETY: the write target is the caller's out-parameter.
    unsafe { *pref = core::ptr::null_mut() };
    if reference.is_null() {
        return -1;
    }
    // SAFETY: the caller's contract on `reference`.
    if !unsafe { is_weakref(reference) } {
        crate::tstate::set_string(
            crate::types::exc::system_error(),
            b"bad argument to internal function",
        );
        return -1;
    }
    // SAFETY: the field read is within the weakref.
    let referent = unsafe { (*reference.cast::<PyWeakReference>()).wr_object };
    // SAFETY: as above.
    unsafe {
        if referent.is_null() {
            *pref = crate::object::singletons::none();
        } else {
            // The contract of CPython 3.12: a new strong reference, so a
            // live referent cannot die under the caller. An immortal
            // referent skips the count.
            crate::types::exc::retain_object(referent);
            *pref = referent;
        }
    }
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// The arguments, when non-null, must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyWeakref_NewProxy(
    _ob: *mut PyObject,
    _callback: *mut PyObject,
) -> *mut PyObject {
    // The proxy half of the C API needs attribute-forwarding descriptors,
    // which arrive with the object-protocol task.
    crate::tstate::set_string(
        crate::types::exc::not_implemented_error(),
        b"coil: PyWeakref_NewProxy is not implemented",
    );
    core::ptr::null_mut()
}

/// Mark the weak-reference table's weakrefs — the section 9.2 root that
/// keeps weakrefs, and through `tp_traverse` their callbacks, alive until
/// they have fired.
pub(crate) fn mark_table(marks: &mut MarkState) {
    for row in table()
        .lock()
        .expect("weak-reference table poisoned")
        .rows
        .iter()
    {
        marks.mark(row.weakref as *mut PyObject);
    }
}

/// Drop every row whose weakref was swept, or whose referent was swept
/// this collection. Runs after the sweep, inside the collection: no
/// allocation can recycle a freed referent's address first, and rows whose
/// referent lives outside the GC heap (weakrefs to immortal types) stay.
pub(crate) fn purge_dead_rows() {
    table()
        .lock()
        .expect("weak-reference table poisoned")
        .rows
        .retain(|row| {
            crate::gc::block_is_live_object(row.weakref)
                && !crate::gc::heap_object_was_freed(row.referent)
        });
}

/// Phase 6 (section 9.3): clear the weak references to every dead object
/// and run their callbacks, in `handle_weakrefs` order — the table's
/// creation order — with resurrections re-marked after each callback.
/// The sweep's dead set reads the mark bits this leaves.
pub(crate) fn handle_weakrefs(marks: &mut MarkState) {
    let rows = table()
        .lock()
        .expect("weak-reference table poisoned")
        .rows
        .clone();
    for row in rows {
        // The weakref must still be a live allocation: one swept in an
        // earlier collection left a row that `purge_dead_rows` clears.
        if !crate::gc::block_is_live_object(row.weakref) {
            continue;
        }
        let weakref = row.weakref as *mut PyObject;
        let wr = weakref.cast::<PyWeakReference>();
        // SAFETY: the weakref is a live object of this runtime.
        let referent = unsafe { (*wr).wr_object };
        if referent.is_null() || core::ptr::eq(referent, crate::object::singletons::none()) {
            // Already cleared by an earlier collection.
            continue;
        }
        // A referent outside the GC heap never dies; phase 6 skips it.
        match mark::is_marked(row.referent) {
            None => continue,
            // Resurrected by a finalizer or an earlier callback: leave
            // the weakref live, exactly as CPython does.
            Some(true) => continue,
            Some(false) => {}
        }
        // SAFETY: the weakref is live and its fields are writable under
        // the collector's stop.
        let callback = unsafe { (*wr).wr_callback };
        // CPython's `clear_weakref`: the referent becomes `None`, and the
        // hash a cleared weakref keeps is `None`'s.
        unsafe { (*wr).wr_object = crate::object::singletons::none() };
        if !callback.is_null() {
            call_callback(callback, weakref);
            // A callback may resurrect the referent or anything else;
            // re-mark before the next row reads mark bits.
            marks.mark(weakref);
            crate::gc::mark_all(marks);
        }
    }
}

/// `handle_weakrefs`'s callback invocation: `callback(weakref)`, with an
/// exception swallowed as unraisable, on the collecting thread.
fn call_callback(callback: *mut PyObject, weakref: *mut PyObject) {
    // SAFETY: the slot read is within the callback's type.
    let ty = unsafe { header::type_of(callback) };
    // SAFETY: as `call_callback`'s caller checked callability at creation;
    // a type that lost its slot since is skipped unraisably.
    let Some(call) = (unsafe { (*ty).tp_call }) else {
        return;
    };
    let args = [weakref];
    let tuple = crate::types::tuple::from_array(&args);
    if tuple.is_null() {
        crate::tstate::clear();
        return;
    }
    // SAFETY: the callback's own calling slot with its argument tuple.
    let result = unsafe { call(callback, tuple, core::ptr::null_mut()) };
    if result.is_null() {
        // CPython writes the failure unraisable and carries on.
        // SAFETY: `PyErr_WriteUnraisable` takes the callback.
        unsafe { crate::types::exc::PyErr_WriteUnraisable(callback) };
    } else {
        // The result of a weakref callback is discarded; the runtime owns
        // the reference `tp_call` returned.
        // SAFETY: a runtime-created object whose reference `tp_call`
        // handed us.
        unsafe { crate::types::exc::release_object(result) };
    }
    // The argument tuple's reference is the collector's own; without this
    // drop it would linger as a pin that keeps the weakref's graph alive
    // forever.
    // SAFETY: the tuple was built above for this call alone.
    unsafe { crate::types::exc::release_object(tuple) };
    crate::tstate::clear();
}

//! The GC half of `object.h` (spec section 10.1): the `PyObject_GC_*`
//! entry points and the variable-size object initialisers. This
//! runtime's collector finds objects by walking its own pages, so
//! tracking is residency itself — `PyObject_GC_Track`/`UnTrack` hold the
//! ABI shape but carry no list to link into.

use core::ffi::c_int;

use coil_capi_sys::{Py_TPFLAGS_HAVE_GC, PyObject, PyTypeObject, PyVarObject};

use crate::gc::counted;
use crate::object::header;
use crate::tstate;
use crate::types::exc as exceptions;

/// See the CPython 3.12 contract for this entry point. With residency
/// tracking (spec section 9.3) there is no generation list to join, so
/// the call validates the type's GC flag and does nothing else.
///
/// # Safety
///
/// `op` must be a valid object of a `Py_TPFLAGS_HAVE_GC` type that is
/// not already tracked.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_Track(op: *mut PyObject) {
    // SAFETY: the flag read is within the type object.
    debug_assert!(unsafe { (*header::type_of(op)).tp_flags } & Py_TPFLAGS_HAVE_GC != 0);
}

/// See the CPython 3.12 contract for this entry point; as
/// [`PyObject_GC_Track`], untracking is residency itself.
///
/// # Safety
///
/// `op` must be a valid tracked object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_UnTrack(op: *mut PyObject) {
    // SAFETY: the flag read is within the type object.
    debug_assert!(unsafe { (*header::type_of(op)).tp_flags } & Py_TPFLAGS_HAVE_GC != 0);
}

/// See the CPython 3.12 contract for this entry point. Every object of
/// a GC type is part of the collection here, so tracked-ness is the
/// type's flag.
///
/// # Safety
///
/// `op` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_IsTracked(op: *mut PyObject) -> c_int {
    // SAFETY: the flag read is within the type object.
    c_int::from(unsafe { (*header::type_of(op)).tp_flags } & Py_TPFLAGS_HAVE_GC != 0)
}

/// See the CPython 3.12 contract for this entry point: the page bitmap
/// bit the collector's finalization phase shares with
/// `PyObject_CallFinalizer`.
///
/// # Safety
///
/// `op` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_IsFinalized(op: *mut PyObject) -> c_int {
    // SAFETY: the page lookup and bit read are the allocator's own.
    match counted::object_page(op as usize) {
        Some((page, granule)) => c_int::from(page.finalized(granule)),
        None => 0,
    }
}

/// CPython's `PyObject_IS_GC` macro as a function: a GC type whose
/// `tp_is_gc` (when present) accepts the object.
///
/// # Safety
///
/// `op` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_IS_GC(op: *mut PyObject) -> c_int {
    // SAFETY: the reads follow the C contract.
    unsafe {
        let ty = header::type_of(op);
        if (*ty).tp_flags & Py_TPFLAGS_HAVE_GC == 0 {
            return 0;
        }
        match (*ty).tp_is_gc {
            Some(is_gc) => is_gc(op),
            None => 1,
        }
    }
}

/// See the CPython 3.12 contract for this entry point: free a block
/// allocated with [`counted`] `GC_New`.
///
/// # Safety
///
/// `op` must be a block the object allocator returned for a GC type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GC_Del(op: *mut PyObject) {
    // SAFETY: the caller passed an allocator block; the runtime's own
    // bookkeeping (foreign registration, side counts) is dropped with
    // it the way `dealloc_object` does before `tp_free`.
    unsafe {
        let addr = op as usize;
        if counted::object_page(addr).is_none() {
            crate::alloc::foreign::unregister(addr);
        }
        counted::drop_side_counts(addr);
        crate::alloc::free_block(op.cast());
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must address writable memory that nothing else has seen; `ty`
/// must be readied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_InitVar(
    op: *mut PyVarObject,
    ty: *mut PyTypeObject,
    size: isize,
) -> *mut PyObject {
    if op.is_null() {
        tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    // SAFETY: the caller's contract; `PyObject_Init` performs the
    // header initialisation and the regime bookkeeping.
    unsafe {
        let object = counted::PyObject_Init(op.cast(), ty);
        (*op).ob_size = size;
        object
    }
}

/// CPython's `_PyObject_NewVar`: allocate and initialise a
/// variable-size object of `ty`.
///
/// # Safety
///
/// `ty` must be readied; the result is freed by its `tp_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_NewVar(
    ty: *mut PyTypeObject,
    nitems: isize,
) -> *mut PyVarObject {
    // SAFETY: the size reads are within the type object.
    unsafe {
        let size = ((*ty).tp_basicsize.max(0) as usize)
            .saturating_add(((*ty).tp_itemsize.max(0) as usize) * nitems.unsigned_abs());
        // SAFETY: the allocation choice follows the regime.
        let block = if counted::type_is_counted(ty) {
            crate::alloc::alloc_counted(size)
        } else {
            crate::alloc::alloc_object(size)
        };
        if block.is_null() {
            tstate::set_string(exceptions::memory_error(), b"");
            return core::ptr::null_mut();
        }
        // SAFETY: the fresh block is `size` writable bytes.
        PyObject_InitVar(block.cast(), ty, nitems).cast()
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `ty` must be readied and GC-tracked-capable; the result is freed by
/// its `tp_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GC_New(ty: *mut PyTypeObject) -> *mut PyObject {
    // SAFETY: the GC-domain allocation and initialisation are
    // `_PyObject_New`'s.
    unsafe { counted::_PyObject_New(ty) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `ty` must be readied and GC-tracked-capable; the result is freed by
/// its `tp_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_GC_NewVar(
    ty: *mut PyTypeObject,
    nitems: isize,
) -> *mut PyVarObject {
    // SAFETY: the allocation and initialisation are `_PyObject_NewVar`'s.
    unsafe { _PyObject_NewVar(ty, nitems) }
}

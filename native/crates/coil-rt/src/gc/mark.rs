//! Marking over the allocator's side bitmaps (section 9.3 phase 4): heap
//! objects by the mark bit at their first granule, runtime objects by
//! their type-specific trace functions — every one of this runtime's
//! container types installs its own `tp_traverse` — and C objects through
//! `tp_traverse` with a marking visitproc, exactly as the spec words it.
//! Objects outside the GC heap (static types, `PyModuleDef`s, anything in
//! the foreign-object table) are never marked or freed, only traced
//! through, with a per-collection seen-set breaking cycles among them.

use core::ffi::c_void;
use std::collections::HashSet;

use coil_capi_sys::PyObject;

use crate::alloc::page_table;
use crate::object::header;

/// The state of one collection's marking: the work stack and the
/// foreign-object dedupe set.
pub(crate) struct MarkState {
    stack: Vec<*mut PyObject>,
    seen_foreign: HashSet<usize>,
}

impl MarkState {
    pub(crate) fn new() -> MarkState {
        MarkState {
            stack: Vec::new(),
            seen_foreign: HashSet::new(),
        }
    }

    /// Mark `object` and everything reachable from it. Safe to call on any
    /// pointer-sized value that is null or an object: garbage, foreign
    /// objects and non-object heap blocks are recognised and skipped.
    pub(crate) fn mark(&mut self, object: *mut PyObject) {
        if object.is_null() {
            return;
        }
        self.push(object);
        self.drain();
    }

    /// Mark every entry of `objects`.
    pub(crate) fn mark_all(&mut self, objects: &[*mut PyObject]) {
        for object in objects {
            if !object.is_null() {
                self.push(*object);
            }
        }
        self.drain();
    }

    fn push(&mut self, object: *mut PyObject) {
        let Some(page) = page_table::lookup(object as usize) else {
            // Outside the GC heap: a static type, a PyModuleDef, an
            // extension's own allocation. Never freed, so no mark bit;
            // traced through when first seen.
            let key = object as usize;
            if self.seen_foreign.insert(key) {
                self.trace(object);
            }
            return;
        };
        let base = page.base();
        let granule = page.granule_of(base, object as usize);
        if !page.is_object(granule) {
            // GC-heap memory that is not an object start: a raw
            // `PyObject_Malloc` block or an interior pointer. Raw blocks
            // carry no header and are C-owned; skip.
            return;
        }
        if page.mark(granule) {
            // Already marked this collection.
            return;
        }
        page.set_mark(granule);
        // The instance keeps its type alive: a heap type object is a
        // GC-heap object itself, and only this reference may hold it.
        // SAFETY: an object's header is the first 16 bytes.
        let ty = unsafe { header::type_of(object) };
        if !ty.is_null() {
            self.push(ty.cast());
        }
        self.stack.push(object);
    }

    fn drain(&mut self) {
        while let Some(object) = self.stack.pop() {
            self.trace(object);
        }
    }

    /// Trace one object's referents through its type's `tp_traverse`.
    fn trace(&mut self, object: *mut PyObject) {
        // SAFETY: the header read is within the object.
        let ty = unsafe { header::type_of(object) };
        if ty.is_null() {
            return;
        }
        // SAFETY: the slot read is within the type object.
        let Some(traverse) = (unsafe { (*ty).tp_traverse }) else {
            return;
        };
        // SAFETY: the visitor is `visit_mark`, whose argument is this
        // `MarkState`.
        unsafe {
            traverse(object, visit_mark, self as *mut MarkState as *mut c_void);
        }
    }
}

/// The marking `visitproc` of section 9.3 phase 4, shared with every
/// traverse-shaped root (`m_traverse`, `tp_traverse` from callers outside
/// this module).
pub(crate) unsafe extern "C" fn visit_mark(
    object: *mut PyObject,
    arg: *mut c_void,
) -> core::ffi::c_int {
    // SAFETY: `arg` is the `MarkState` `trace` passed.
    let marks = unsafe { &mut *(arg as *mut MarkState) };
    marks.push(object);
    // Zero continues the traversal; the stack drains breadth-first after.
    0
}

/// Whether the object at `addr` is currently marked. `None` when `addr` is
/// not a GC-heap object (foreign objects are never dead by definition).
pub(crate) fn is_marked(addr: usize) -> Option<bool> {
    let page = page_table::lookup(addr)?;
    let granule = page.granule_of(page.base(), addr);
    if !page.is_object(granule) {
        return None;
    }
    Some(page.mark(granule))
}

/// A heap type object at `addr` (an instance of `type` on the GC heap),
/// for the sweep's MRO bookkeeping.
pub(crate) fn is_heap_type(addr: usize) -> bool {
    let Some(page) = page_table::lookup(addr) else {
        return false;
    };
    let granule = page.granule_of(page.base(), addr);
    if !page.is_object(granule) {
        return false;
    }
    // SAFETY: an object's header is the first 16 bytes.
    let ty = unsafe { header::type_of(addr as *mut PyObject) };
    core::ptr::eq(ty, &raw mut crate::object::typeobject::PyType_Type)
}

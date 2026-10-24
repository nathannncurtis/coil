//! The collector: roots, the two regimes with pins and the zero-count
//! table, mark and sweep, stack-map walks, pacing, finalisation and weak
//! references (spec sections 9.2 to 9.6).
//!
//! The full collection's phases: 1 stop, 2 the ZCT pass, 3 the
//! shadow-count subtraction, 4 mark from all roots, 5 finalise (PEP 442),
//! 6 clear weak references and run their callbacks in `handle_weakrefs`
//! order, 7 `tp_clear`, 8 `tp_dealloc` once on the dead counted-regime
//! and C-created objects, 9 free every dead block, with nothing reused
//! before that point and objects allocated during the destructor phases
//! born marked. `counted.rs` owns the regime machinery phases 2, 3, 7
//! and 8 lean on; the sweep frees what no destructor claimed.

pub mod counted;
pub mod finalize;
pub mod mark;
pub mod pacing;
pub mod roots;
pub mod stack;
pub mod weakref;

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};

use coil_capi_sys::{PyObject, PyTypeObject};

use crate::alloc::page::PageHeader;
use crate::gc::mark::MarkState;
use crate::object::header;

/// Automatic pacing's switch: `gc.disable()` clears it and only automatic
/// collections stop (section 9.6).
static ENABLED: AtomicBool = AtomicBool::new(true);
/// Set for the whole of a collection: what makes allocation inside the
/// finalizer and callback phases mark the objects it creates, and what
/// stops the pacing trigger and reentrant collections.
static COLLECTING: AtomicBool = AtomicBool::new(false);

/// Objects allocated while COLLECTING is set: born marked (section 9.3
/// phase 9's rule), and re-traced with the roots after each phase that
/// can resurrect, so the objects they reference survive the sweep.
fn born_marked() -> &'static std::sync::Mutex<Vec<usize>> {
    static BORN: std::sync::OnceLock<std::sync::Mutex<Vec<usize>>> = std::sync::OnceLock::new();
    BORN.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// The collection mutex: one collection at a time process-wide.
fn collect_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

/// Whether automatic pacing is on (`gc.isenabled()`).
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// `gc.enable()`.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// `gc.disable()`: stops automatic pacing alone; explicit `gc.collect()`
/// still runs (section 9.6).
pub fn disable() {
    ENABLED.store(false, Ordering::Relaxed);
}

/// Whether a collection is in progress on this process.
pub(crate) fn is_collecting() -> bool {
    COLLECTING.load(Ordering::Relaxed)
}

/// The hook the object allocation paths call once a fresh object's block
/// exists: during a collection the object is born marked and listed for
/// the post-phase re-trace.
pub(crate) fn note_born(addr: usize) {
    if !is_collecting() {
        return;
    }
    if let Some(page) = crate::alloc::page_of(addr) {
        page.set_mark(page.granule_of(page.base(), addr));
    }
    born_marked()
        .lock()
        .expect("born-marked list poisoned")
        .push(addr);
}

/// `gc.collect()`: a full collection. Returns the number of unreachable
/// objects it freed. Reentrant calls (a finalizer or weakref callback
/// reaching back here) return 0 immediately.
pub fn collect() -> usize {
    if is_collecting() {
        return 0;
    }
    let _serial = collect_lock().lock().expect("collection lock poisoned");
    COLLECTING.store(true, Ordering::Relaxed);
    let freed = run_collection();
    COLLECTING.store(false, Ordering::Relaxed);
    freed
}

/// Whether `addr` addresses a live GC-heap object (an allocated block
/// whose first granule is an object start).
pub(crate) fn block_is_live_object(addr: usize) -> bool {
    let Some(page) = crate::alloc::page_of(addr) else {
        return false;
    };
    let granule = page.granule_of(page.base(), addr);
    page.allocated(granule) && page.is_object(granule)
}

/// Whether `addr` addressed a GC-heap object that has since been swept:
/// inside the heap's pages but no longer an allocated object. Foreign
/// addresses answer false — they were never heap objects.
pub(crate) fn heap_object_was_freed(addr: usize) -> bool {
    match crate::alloc::page_of(addr) {
        None => false,
        Some(page) => {
            let granule = page.granule_of(page.base(), addr);
            !(page.allocated(granule) && page.is_object(granule))
        }
    }
}

/// One full collection: phases 1 through 9 of section 9.3.
fn run_collection() -> usize {
    // The debug-heap quarantine of the previous cycle gives its blocks
    // back before this one's walk.
    #[cfg(feature = "debug-heap")]
    counted::release_quarantine();

    // Phase 1, stop: with the GIL the collecting thread is the only
    // attached thread; every other thread's last compiled frame is
    // recorded in its thread state, which the roots walk reads. The
    // collection mutex serialises collections process-wide.
    let pages = crate::alloc::pool::snapshot_pages();

    // Phase 2: process the zero-count tables — every R object whose two
    // counts reached zero gets its stack walk and, when nothing holds
    // it, its `tp_dealloc` — before any liveness is decided.
    counted::process_zct_for_collection();

    // Phase 4's preparation: clear the mark bits and gather the object
    // list once.
    let mut marks = MarkState::new();
    let mut objects: Vec<usize> = Vec::new();
    for page in &pages {
        let base = page.base();
        walk_page_objects(page, &mut |addr| {
            page.clear_mark(page.granule_of(base, addr));
            objects.push(addr);
        });
    }

    // The debug-heap recomputation of `heap_refs` from a heap walk,
    // before any phase can move or free anything.
    #[cfg(feature = "debug-heap")]
    // SAFETY: every address is a live object of this allocator.
    unsafe {
        counted::verify_heap_refs(&objects)
    };

    // Phase 3: the subtraction — a shadow `ob_refcnt`, the real count
    // less the references other objects' counted slots and GC-tracked
    // counted objects hold, decides which objects a positive count still
    // pins; an R object is also held by any heap reference or ZCT entry.
    // SAFETY: every address is a live object of this allocator.
    let pins = unsafe { counted::subtract(&objects) };

    // Phase 4: mark from all roots. The positive-shadow-count pins go
    // first — they are the roots the counts themselves vouch for.
    marks.mark_all(&pins);
    mark_all(&mut marks);

    // Phase 5: finalise the dead with pending finalizers, once each, then
    // re-mark — resurrection is detected here, never inside the finalizer.
    let dead: Vec<usize> = objects
        .iter()
        .copied()
        .filter(|addr| mark::is_marked(*addr) == Some(false))
        .collect();
    // From here through phase 8 destructors run: `tp_free` queues the
    // block instead of freeing, so the walk's page bits stay honest.
    counted::begin_sweep();
    finalize::finalize_dead(&dead);
    mark_all(&mut marks);

    // Phase 6: clear weak references to the dead and run their callbacks.
    weakref::handle_weakrefs(&mut marks);

    // Phase 7: `tp_clear` every dead GC-tracked counted object, breaking
    // the cycles its own refcounts cannot.
    // SAFETY: every address is a dead heap object of this allocator.
    unsafe { counted::clear_pass(&dead) };

    // Phase 8: `tp_dealloc` once on every dead R object and every dead
    // C-created T object, after the dead traced T objects nothing will
    // run a destructor for give up their `heap_refs`.
    // SAFETY: as `clear_pass`.
    unsafe { counted::dealloc_pass(&dead) };

    // Phase 9: free every remaining dead block. Nothing was reused
    // before this point; allocation during the sweep is in the
    // born-marked list, and the sweep's queued frees flush right after.
    let mut live_bytes = 0u64;
    let mut freed = 0usize;
    for addr in &objects {
        match mark::is_marked(*addr) {
            Some(true) => {
                // SAFETY: `addr` is a live object of this allocator.
                if let Some(capacity) = unsafe { crate::alloc::block_capacity(*addr as *const u8) }
                {
                    live_bytes += capacity as u64;
                }
            }
            // Objects phase 8 deallocated are already queued.
            Some(false) if !counted::is_claimed(*addr) => {
                sweep_object(*addr);
                freed += 1;
            }
            Some(false) => {}
            None => {}
        }
    }
    freed += counted::end_sweep(&dead);
    weakref::purge_dead_rows();
    born_marked()
        .lock()
        .expect("born-marked list poisoned")
        .clear();
    pacing::record_collection(live_bytes);
    freed
}

/// Every root, marked: the registries of `roots.rs`, the finalizer queue,
/// and the objects born during this collection. The weak-reference table
/// is deliberately absent — its rows link, they do not own, so a weakref
/// lives by external references alone and phase 6 pins the ones whose
/// callbacks it still owes. Idempotent under the mark bits, so the phases
/// that can resurrect call it again.
pub(crate) fn mark_all(marks: &mut MarkState) {
    roots::mark_all_roots(marks);
    finalize::mark_queue(marks);
    let born: Vec<usize> = born_marked()
        .lock()
        .expect("born-marked list poisoned")
        .clone();
    for addr in born {
        marks.mark(addr as *mut PyObject);
    }
}

/// Visit every live object of `page` (an allocated block whose first
/// granule is an object start): small pages block by block, a large run's
/// single object at its data offset, after the 16-byte pre-header when it
/// carries one.
fn walk_page_objects(page: &'static PageHeader, visit: &mut dyn FnMut(usize)) {
    let base = page.base();
    let data = base + crate::alloc::page::DATA_OFFSET;
    if !page.is_small() {
        if page.has_prehdr(0) && page.is_object(1) {
            visit(data + crate::alloc::PREHEADER);
        } else if page.is_object(0) {
            visit(data);
        }
        return;
    }
    let block_size = page.block_size();
    let blocks = crate::alloc::page::DATA_BYTES / block_size;
    for i in 0..blocks {
        let g0 = i * (block_size / crate::alloc::page::GRANULE);
        if !page.allocated(g0) {
            continue;
        }
        if page.is_object(g0) {
            visit(page.granule_addr(base, g0));
        } else if page.has_prehdr(g0) && page.is_object(g0 + 1) {
            // A counted-regime block: the object sits one granule in,
            // behind its 16-byte pre-header.
            visit(page.granule_addr(base, g0 + 1));
        }
    }
}

/// Phase 9's free of one dead object: nothing else — no destructor, no
/// aux memory release; those are phases 7 and 8. Two exceptions the
/// allocator's own correctness needs: a dead heap type's MRO table entry
/// must not outlive its block (a new type at the same address would
/// inherit a stale MRO), and its owned name buffer is the type's own
/// plain allocation.
fn sweep_object(addr: usize) {
    if mark::is_heap_type(addr) {
        let ty = addr as *mut PyTypeObject;
        crate::object::typeobject::forget_mro(ty);
        // SAFETY: the field read is within the dead heap type's block.
        let name = unsafe {
            core::ptr::read_unaligned(
                (ty as *const u8).add(coil_rt_abi::cp312::heaptypeobject::HT_TPNAME)
                    as *const *mut u8,
            )
        };
        if !name.is_null() {
            // SAFETY: the buffer came from this allocator.
            unsafe { crate::alloc::free_block(name) };
        }
    }
    // The `--features debug-heap` poisoning and the counted-regime
    // quarantine ride in `free_block` itself, so every free path shares
    // them.
    // SAFETY: `addr` is a dead object address this allocator returned.
    unsafe { crate::alloc::free_block(addr as *mut u8) };
}

/// `gc.get_objects()`: every GC-heap object as a tuple, in heap-walk
/// order. The tuple itself is a fresh, rooted object.
pub fn get_objects() -> *mut PyObject {
    let pages = crate::alloc::pool::snapshot_pages();
    let mut objects: Vec<*mut PyObject> = Vec::new();
    for page in &pages {
        walk_page_objects(page, &mut |addr| objects.push(addr as *mut PyObject));
    }
    crate::types::tuple::from_array(&objects)
}

/// `gc.get_referents()`: the objects `object` references directly, through
/// its type's `tp_traverse`, as a tuple.
///
/// # Safety
///
/// `object` must be null or a valid `PyObject` of a readied type.
pub unsafe fn get_referents(object: *mut PyObject) -> *mut PyObject {
    if object.is_null() {
        return crate::types::tuple::empty_tuple();
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(object) };
    // SAFETY: the slot read is within the type object.
    let Some(traverse) = (unsafe { (*ty).tp_traverse }) else {
        return crate::types::tuple::empty_tuple();
    };
    let mut referents: Vec<*mut PyObject> = Vec::new();
    // SAFETY: the visitor appends to the vector passed as its argument.
    unsafe {
        traverse(
            object,
            visit_collect,
            &mut referents as *mut Vec<*mut PyObject> as *mut c_void,
        );
    };
    crate::types::tuple::from_array(&referents)
}

/// `get_referents`' visitor.
unsafe extern "C" fn visit_collect(object: *mut PyObject, arg: *mut c_void) -> core::ffi::c_int {
    // SAFETY: `arg` is the vector `get_referents` passed.
    let referents = unsafe { &mut *(arg as *mut Vec<*mut PyObject>) };
    if !object.is_null() {
        referents.push(object);
    }
    0
}

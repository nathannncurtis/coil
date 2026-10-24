//! The counted regime of spec section 9.3: extension-defined types whose
//! lifetime is a count — C references in `ob_refcnt`, compiled-heap
//! references in `heap_refs`, stack references deferred to the zero-count
//! table (Deutsch-Bobrow). This module owns the regime classification,
//! `heap_refs` storage (the pre-header, the side map for R objects
//! without one), the store barrier, the ZCT, `_Py_Dealloc` by regime, and
//! the passes a full collection adds: the shadow-`ob_refcnt` subtraction
//! (phase 3), `tp_clear` (phase 7) and the once-each `tp_dealloc` pass
//! (phase 8). The tracing regime keeps `object/gc.rs`'s reachability
//! rule; both live in one heap whose page bits say which rule applies.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use coil_capi_sys::{PyObject, PyTypeObject};

use crate::alloc::page::PageHeader;
use crate::gc::mark;
use crate::object::header;
use crate::tstate;

// ------------------------------------------------------ regime classification

/// The `tp_dealloc` addresses this runtime itself implements: a type
/// whose dealloc landed here belongs to the tracing regime however it was
/// declared; any other dealloc makes the type counted (R) — it is
/// extension-defined (section 9.3). Registered from `init_static_types`,
/// where the runtime's own types exist.
fn coil_deallocs() -> &'static Mutex<HashSet<usize>> {
    static SET: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record `ty` as one of the runtime's own types: its dealloc (when it
/// has one) is Coil's, so instances are tracing-regime objects.
///
/// # Safety
///
/// `ty` must be a type object being readied, once, before concurrent
/// access can exist.
pub unsafe fn note_coil_type(ty: *mut PyTypeObject) {
    // SAFETY: the caller's contract; the slot read is within the type.
    if let Some(dealloc) = unsafe { (*ty).tp_dealloc } {
        coil_deallocs()
            .lock()
            .expect("coil-dealloc registry poisoned")
            .insert(dealloc as usize);
    }
}

/// The types whose slots hold barrier-accounted references rather than
/// counted references: the traced containers (dict, set — spec 9.3's
/// "never counted"), which the runtime creates itself and which still
/// store R values under the barrier. The heap-walk count check of
/// `--features debug-heap` uses this to recompute `heap_refs` from the
/// same edges the barrier adjusted.
fn barrier_container_types() -> &'static Mutex<HashSet<usize>> {
    static SET: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    SET.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record `ty` as a container whose stores go through the barrier.
pub fn note_barrier_container(ty: *mut PyTypeObject) {
    barrier_container_types()
        .lock()
        .expect("barrier-container registry poisoned")
        .insert(ty as usize);
}

/// Whether `ty`'s instances live under the counted regime: an
/// extension-defined type — a dealloc that is not this runtime's. Types
/// without a dealloc are the compiler's own classes: tracing regime.
///
/// # Safety
///
/// `ty` must be a readied type object.
pub(crate) unsafe fn type_is_counted(ty: *mut PyTypeObject) -> bool {
    // SAFETY: the slot read is within the type object.
    match unsafe { (*ty).tp_dealloc } {
        None => false,
        Some(dealloc) => !coil_deallocs()
            .lock()
            .expect("coil-dealloc registry poisoned")
            .contains(&(dealloc as usize)),
    }
}

/// The page and object granule of a heap address, when it has both.
fn object_page(addr: usize) -> Option<(&'static PageHeader, usize)> {
    let page = crate::alloc::page_of(addr)?;
    Some((page, page.granule_of(page.base(), addr)))
}

/// Whether `object` lives under the counted regime: the page's regime bit
/// for heap objects, the side map for R objects without a pre-header.
///
/// # Safety
///
/// `object` must be null or a valid object of a readied type.
pub(crate) unsafe fn is_counted_object(object: *mut PyObject) -> bool {
    let addr = object as usize;
    if addr == 0 {
        return false;
    }
    match object_page(addr) {
        Some((page, granule)) => page.regime(granule),
        None => side_counts()
            .lock()
            .expect("side-count map poisoned")
            .contains_key(&addr),
    }
}

// ------------------------------------------------------------- the side map

/// The heap_refs bookkeeping of an R object without a pre-header: foreign
/// memory (`PyObject_Init` on extension-owned storage) or an in-heap
/// block that never got one. The map also carries the flags those
/// objects cannot keep in page bits.
#[derive(Clone, Copy)]
struct SideEntry {
    heap_refs: isize,
    zct_pending: bool,
    escaped: bool,
}

fn side_counts() -> &'static Mutex<HashMap<usize, SideEntry>> {
    static MAP: OnceLock<Mutex<HashMap<usize, SideEntry>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The pre-header's second word is free per spec 9.3; its bit 0 says the
/// object sits in a zero-count table.
const PREHDR_ZCT_PENDING: isize = 1;

/// Read one word of an R object's pre-header.
///
/// # Safety
///
/// `object` must be an R object with a pre-header.
unsafe fn preheader_word(object: *mut PyObject, index: usize) -> isize {
    // SAFETY: the pre-header is the 16 bytes before the object.
    unsafe {
        core::ptr::read_unaligned(
            ((object as usize - crate::alloc::PREHEADER) as *const u8)
                .add(index * core::mem::size_of::<isize>()) as *const isize,
        )
    }
}

/// Write one word of an R object's pre-header.
///
/// # Safety
///
/// As `preheader_word`.
unsafe fn write_preheader_word(object: *mut PyObject, index: usize, value: isize) {
    // SAFETY: as `preheader_word`.
    unsafe {
        core::ptr::write_unaligned(
            ((object as usize - crate::alloc::PREHEADER) as *mut u8)
                .add(index * core::mem::size_of::<isize>()) as *mut isize,
            value,
        )
    }
}

/// `heap_refs` of `object` (0 for anything but an R object).
///
/// # Safety
///
/// `object` must be a valid object whose counts are not concurrently
/// adjusted.
pub(crate) unsafe fn heap_refs(object: *mut PyObject) -> isize {
    if !unsafe { is_counted_object(object) } {
        return 0;
    }
    // SAFETY: the pre-header read is within the object's block.
    if unsafe { crate::alloc::preheader(object.cast()) }.is_some() {
        return unsafe { preheader_word(object, 0) };
    }
    side_counts()
        .lock()
        .expect("side-count map poisoned")
        .get(&(object as usize))
        .map_or(0, |entry| entry.heap_refs)
}

/// `heap_refs` of `object` for integration tests (0 for anything but an R
/// object).
///
/// # Safety
///
/// As `heap_refs`.
pub unsafe fn heap_refs_of(object: *mut PyObject) -> isize {
    // SAFETY: forwarded with the caller's validity guarantee.
    unsafe { heap_refs(object) }
}

/// Adjust `heap_refs` by `delta` and return the new value.
///
/// # Safety
///
/// As `heap_refs`; the caller guarantees the result stays non-negative.
unsafe fn adjust_heap_refs(object: *mut PyObject, delta: isize) -> isize {
    // SAFETY: the pre-header probe reads only page bits of the object's block.
    if unsafe { crate::alloc::preheader(object.cast()) }.is_some() {
        let value = unsafe { preheader_word(object, 0) } + delta;
        check_underflow(object, value);
        unsafe { write_preheader_word(object, 0, value) };
        return value;
    }
    let mut map = side_counts().lock().expect("side-count map poisoned");
    let entry = map.entry(object as usize).or_insert(SideEntry {
        heap_refs: 0,
        zct_pending: false,
        escaped: false,
    });
    entry.heap_refs += delta;
    check_underflow(object, entry.heap_refs);
    entry.heap_refs
}

/// The `--features debug-heap` underflow check: `heap_refs` going below
/// zero means the barrier paired an increment with two decrements.
#[cfg(feature = "debug-heap")]
fn check_underflow(object: *mut PyObject, value: isize) {
    assert!(
        value >= 0,
        "debug-heap: heap_refs underflow at {object:p} ({value})"
    );
}

#[cfg(not(feature = "debug-heap"))]
fn check_underflow(_object: *mut PyObject, _value: isize) {}

/// Whether `object` sits in a zero-count table right now — the ZCT's
/// presence counts as a holder (section 9.3).
///
/// # Safety
///
/// `object` must be a valid R object.
pub(crate) unsafe fn zct_pending(object: *mut PyObject) -> bool {
    // SAFETY: the pre-header probe reads only page bits of the object's block.
    if unsafe { crate::alloc::preheader(object.cast()) }.is_some() {
        return unsafe { preheader_word(object, 1) } & PREHDR_ZCT_PENDING != 0;
    }
    side_counts()
        .lock()
        .expect("side-count map poisoned")
        .get(&(object as usize))
        .is_some_and(|entry| entry.zct_pending)
}

/// Set or clear the ZCT-pending flag.
///
/// # Safety
///
/// As `zct_pending`.
unsafe fn set_zct_pending(object: *mut PyObject, pending: bool) {
    // SAFETY: the pre-header probe reads only page bits of the object's block.
    if unsafe { crate::alloc::preheader(object.cast()) }.is_some() {
        // SAFETY: the flag lives in the pre-header's second word.
        let mut flags = unsafe { preheader_word(object, 1) };
        if pending {
            flags |= PREHDR_ZCT_PENDING;
        } else {
            flags &= !PREHDR_ZCT_PENDING;
        }
        unsafe { write_preheader_word(object, 1, flags) };
        return;
    }
    let mut map = side_counts().lock().expect("side-count map poisoned");
    if let Some(entry) = map.get_mut(&(object as usize)) {
        entry.zct_pending = pending;
    }
}

/// Whether a C-created object escaped into compiled code (section 9.3).
///
/// # Safety
///
/// `object` must be a valid object.
pub(crate) unsafe fn escaped(object: *mut PyObject) -> bool {
    match object_page(object as usize) {
        Some((page, granule)) => page.escaped(granule),
        None => side_counts()
            .lock()
            .expect("side-count map poisoned")
            .get(&(object as usize))
            .is_some_and(|entry| entry.escaped),
    }
}

/// Mark `object` as escaped: a pointer crossing into compiled code. The
/// boundary machinery (C API values returned to compiled code, values
/// read out of counted containers) calls this; a T object's refcount
/// death then belongs to the collector, an R object's to the ZCT, so a
/// frame slot still holding it cannot dangle.
pub fn note_escape(object: *mut PyObject) {
    let addr = object as usize;
    if addr == 0 {
        return;
    }
    if let Some((page, granule)) = object_page(addr) {
        page.set_escaped(granule);
    } else {
        let mut map = side_counts().lock().expect("side-count map poisoned");
        let entry = map.entry(addr).or_insert(SideEntry {
            heap_refs: 0,
            zct_pending: false,
            escaped: false,
        });
        entry.escaped = true;
    }
}

// ------------------------------------------------------------ the barrier

/// `coil_rt_barrier_ext` (abi.toml `[functions]`): the barrier emitted
/// when the stored value's static type is an extension type or Top.
/// Extension-typed values carry a pre-header or a side entry by
/// construction, so the adjustment is unconditional; the shared body
/// still tolerates any value.
///
/// # Safety
///
/// Both pointers must be null or valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_barrier_ext(overwritten: *mut PyObject, stored: *mut PyObject) {
    // SAFETY: the caller's contract.
    unsafe { barrier(overwritten, stored) };
}

/// `coil_rt_barrier_dynamic` (abi.toml `[functions]`): the barrier whose
/// value may be of any type: the page table's regime bit decides whether
/// `stored` carries `heap_refs` at all.
///
/// # Safety
///
/// As `coil_rt_barrier_ext`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_barrier_dynamic(
    overwritten: *mut PyObject,
    stored: *mut PyObject,
) {
    // SAFETY: the caller's contract.
    unsafe { barrier(overwritten, stored) };
}

/// The barrier's body: every store into heap memory that is not a
/// counted container's slot — traced containers, compiled objects'
/// fields — adjusts `heap_refs` on the R side of the store and marks a
/// C-created T value stored into the heap as escaped.
///
/// # Safety
///
/// Both pointers must be null or valid objects.
pub(crate) unsafe fn barrier(overwritten: *mut PyObject, stored: *mut PyObject) {
    if !overwritten.is_null() {
        // SAFETY: the caller's contract.
        unsafe { barrier_dec(overwritten) };
    }
    if !stored.is_null() {
        // SAFETY: the caller's contract.
        unsafe { barrier_inc(stored) };
        // A C constructor's result entering heap memory it does not own
        // a counted slot of has escaped: its refcount death is the
        // collector's to decide, not an immediate `_Py_Dealloc`.
        if !unsafe { is_counted_object(stored) } && c_created(stored) {
            note_escape(stored);
        }
    }
}

/// One stored R value gained a heap reference.
///
/// # Safety
///
/// `value` must be a valid object.
unsafe fn barrier_inc(value: *mut PyObject) {
    if !unsafe { is_counted_object(value) } {
        return;
    }
    // SAFETY: the value is an R object whose counts this thread adjusts.
    unsafe { adjust_heap_refs(value, 1) };
}

/// One overwritten R value lost a heap reference; a value reaching zero
/// with no C reference left joins the zero-count table.
///
/// # Safety
///
/// `value` must be a valid object.
pub(crate) unsafe fn barrier_dec(value: *mut PyObject) {
    if !unsafe { is_counted_object(value) } {
        return;
    }
    // During a sweep, the reference being dropped may point at an object
    // this collection has already dealtlocated: its count is moot.
    if claims_open() && is_claimed(value as usize) {
        return;
    }
    // SAFETY: the value is an R object whose counts this thread adjusts.
    let refs = unsafe { adjust_heap_refs(value, -1) };
    if refs == 0
        // SAFETY: the header read is within the object.
        && unsafe { header::refcnt(value) } == 0
        // SAFETY: the flag read is within the object's bookkeeping.
        && !unsafe { zct_pending(value) }
    {
        // SAFETY: the object's counts are both zero and this thread
        // owns the enqueue.
        unsafe { zct_enqueue(value) };
    }
}

/// A read out of a counted container's slot: the borrowed pointer
/// crossing into compiled code makes a C-created T value escaped.
///
/// # Safety
///
/// `value` must be null or a valid object.
pub(crate) unsafe fn note_read_from_counted(value: *mut PyObject) {
    if value.is_null() || unsafe { is_counted_object(value) } {
        return;
    }
    if c_created(value) {
        note_escape(value);
    }
}

/// The `c_created` page bit (foreign objects never carry it).
fn c_created(object: *mut PyObject) -> bool {
    object_page(object as usize).is_some_and(|(page, granule)| page.c_created(granule))
}

/// Whether `object` is a container whose slots hold counted references
/// (`PyTuple_New`, `PyList_New` — section 9.3): stores steal or incref
/// `ob_refcnt`, the death of the container drops them, and a
/// collection's subtraction counts them as internal.
pub fn slots_are_counted(object: *mut PyObject) -> bool {
    object_page(object as usize).is_some_and(|(page, granule)| page.counted_slots(granule))
}

/// Mark a fresh container's slots as counted.
///
/// # Safety
///
/// `object` must be a freshly created container no other thread has seen.
pub unsafe fn mark_counted_slots(object: *mut PyObject) {
    if let Some((page, granule)) = object_page(object as usize) {
        page.set_counted_slots(granule);
    }
}

// -------------------------------------------------------- the zero-count table

/// One thread's zero-count table: the R objects whose two counts reached
/// zero, waiting for the stack walk that decides their fate. Lives in
/// the thread state's `zct` field.
#[repr(C)]
pub struct Zct {
    pub entries: Mutex<Vec<usize>>,
}

/// Allocate a thread's ZCT.
pub fn new_zct() -> *mut core::ffi::c_void {
    Box::into_raw(Box::new(Zct {
        entries: Mutex::new(Vec::new()),
    })) as *mut core::ffi::c_void
}

/// The process-wide entry count, the trigger's measure.
static ZCT_TOTAL: AtomicUsize = AtomicUsize::new(0);
/// Set while a ZCT processing pass runs, so nested triggers cannot
/// re-enter it.
static ZCT_PROCESSING: AtomicBool = AtomicBool::new(false);
/// The threshold at which a safepoint processes the table (spec 9.3).
pub const ZCT_THRESHOLD: usize = 64;

/// Enqueue `object` in its thread's ZCT. The pending flag doubles as the
/// "held" marker a collection's subtraction honours.
///
/// # Safety
///
/// `object` must be a live R object whose counts are both zero.
unsafe fn zct_enqueue(object: *mut PyObject) {
    // SAFETY: the flag write is within the object's bookkeeping.
    unsafe { set_zct_pending(object, true) };
    let state = tstate::current();
    // SAFETY: the state is this thread's own and its `zct` was set at
    // creation.
    let zct = unsafe { (*state).zct as *mut Zct };
    // SAFETY: the pointer came from `new_zct` and lives as long as the
    // thread state.
    unsafe {
        (*zct)
            .entries
            .lock()
            .expect("zct poisoned")
            .push(object as usize)
    };
    ZCT_TOTAL.fetch_add(1, Ordering::Relaxed);
}

/// The allocation-time safepoint: with at least `ZCT_THRESHOLD` entries
/// waiting, a fresh heap request processes the table first (spec 9.3).
pub(crate) fn zct_safepoint() {
    if ZCT_TOTAL.load(Ordering::Relaxed) >= ZCT_THRESHOLD {
        process_zct();
    }
}

/// The eval-breaker tick: any waiting entry is processed when a thread
/// re-attaches, the runtime's stand-in for the 5 ms switch interval.
pub(crate) fn zct_tick() {
    if ZCT_TOTAL.load(Ordering::Relaxed) > 0 {
        process_zct();
    }
}

/// Process every thread's zero-count table: walk every thread's compiled
/// frames, keep the entries a live frame slot holds, and give everything
/// else its `tp_dealloc`, in insertion order. Presence on the table
/// counted as a holder until exactly here.
pub fn process_zct() {
    // A nested trigger (a dealloc that allocates) must not re-enter, and
    // a collection processes the table as its phase 2.
    if crate::gc::is_collecting() || ZCT_PROCESSING.swap(true, Ordering::AcqRel) {
        return;
    }
    run_zct_pass();
    ZCT_PROCESSING.store(false, Ordering::Release);
}

/// Phase 2's entry: the same pass, run inside a collection where the
/// `is_collecting` guard would otherwise stop it.
pub(crate) fn process_zct_for_collection() {
    if ZCT_PROCESSING.swap(true, Ordering::AcqRel) {
        return;
    }
    run_zct_pass();
    ZCT_PROCESSING.store(false, Ordering::Release);
}

fn run_zct_pass() -> usize {
    let mut entries = Vec::new();
    for state in tstate::registry::all() {
        // SAFETY: registered states live for the process.
        let zct = unsafe { (*state).zct as *mut Zct };
        if zct.is_null() {
            continue;
        }
        // SAFETY: as `zct_enqueue`.
        let drained: Vec<usize> = unsafe {
            (*zct)
                .entries
                .lock()
                .expect("zct poisoned")
                .drain(..)
                .collect()
        };
        ZCT_TOTAL.fetch_sub(drained.len(), Ordering::Relaxed);
        entries.extend(drained);
    }
    let held = stack_addresses();
    let mut freed = 0usize;
    for addr in entries {
        let object = addr as *mut PyObject;
        // A cascade inside this pass may have freed the object or
        // re-enqueued it; a fresh object may even own the address now —
        // its pending flag is clear, which tells the two apart. Foreign
        // R objects answer no page table; the foreign registry vouches
        // for them instead.
        // SAFETY: the flag read is within a live object's bookkeeping.
        if !unsafe { zct_pending(object) }
            || !(crate::gc::block_is_live_object(addr) || crate::alloc::foreign::contains(addr))
        {
            continue;
        }
        // SAFETY: as above.
        unsafe { set_zct_pending(object, false) };
        // SAFETY: the header and count reads are within the object.
        unsafe {
            if header::refcnt(object) > 0 || heap_refs(object) > 0 {
                // Revived: the counts re-arm the enqueue paths, so the
                // entry drops and the object stays valid as it was.
                continue;
            }
            if held.contains(&addr) {
                // Stack-held (Deutsch-Bobrow): the frame slot's eventual
                // clear is a raw store nothing observes, so the entry
                // stays on the table for the next processing.
                set_zct_pending(object, true);
                zct_requeue(addr);
                continue;
            }
            dealloc_object(object);
            freed += 1;
        }
    }
    freed
}

/// Every address a live compiled frame's reference slots hold, over all
/// threads (section 9.4's walk, one table per thread).
fn stack_addresses() -> HashSet<usize> {
    let mut addresses = HashSet::new();
    for state in tstate::registry::all() {
        // SAFETY: registered states live for the process; the frame
        // pointer is read only while its thread is stopped by the GIL.
        let frame = unsafe { (*state).frame } as usize;
        if frame != 0 {
            // SAFETY: the frame is a live compiled frame anchored in the
            // thread state.
            unsafe {
                crate::gc::stack::walk_frames(frame, &mut |value: *mut PyObject| {
                    addresses.insert(value as usize);
                })
            };
        }
    }
    addresses
}

/// Put a stack-held entry back on this thread's table for the next
/// processing pass.
fn zct_requeue(addr: usize) {
    let state = tstate::current();
    // SAFETY: the state is this thread's own and its `zct` was set at
    // creation.
    let zct = unsafe { (*state).zct as *mut Zct };
    // SAFETY: as `zct_enqueue`.
    unsafe { (*zct).entries.lock().expect("zct poisoned").push(addr) };
    ZCT_TOTAL.fetch_add(1, Ordering::Relaxed);
}

// ------------------------------------------- reference counts and _Py_Dealloc

/// `Py_IncRef`.
///
/// # Safety
///
/// `object` must be null or a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_IncRef(object: *mut PyObject) {
    // SAFETY: the C API contract.
    unsafe { _Py_IncRef(object) };
}

/// `_Py_IncRef`.
///
/// # Safety
///
/// As `Py_IncRef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_IncRef(object: *mut PyObject) {
    if object.is_null() || unsafe { header::is_immortal(object) } {
        return;
    }
    // SAFETY: the reference count is the header's first field.
    unsafe { (*object).ob_refcnt += 1 };
}

/// `Py_DecRef`.
///
/// # Safety
///
/// `object` must be null or an object this runtime created whose header
/// is not concurrently accessed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn Py_DecRef(object: *mut PyObject) {
    // SAFETY: the C API contract.
    unsafe { _Py_DecRef(object) };
}

/// `_Py_DecRef`.
///
/// # Safety
///
/// As `Py_DecRef`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_DecRef(object: *mut PyObject) {
    if object.is_null() {
        return;
    }
    // Inside a collection's sweep a reference into an object this
    // collection already destroyed is moot — its destructor ran, and a
    // member drop from another destructor (or a callback) must not touch
    // the dead header. CPython's trashcan plays this role.
    if claims_open() && is_claimed(object as usize) {
        return;
    }
    if unsafe { header::is_immortal(object) } {
        return;
    }
    // SAFETY: as `Py_DecRef`.
    unsafe {
        (*object).ob_refcnt -= 1;
        if (*object).ob_refcnt == 0 {
            dealloc_at_zero(object);
        }
    };
}

/// `_Py_Dealloc`: the object's counts have reached zero; what happens
/// next is decided by its regime (section 9.3).
///
/// # Safety
///
/// `object` must be a valid object whose `ob_refcnt` just reached zero.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_Dealloc(object: *mut PyObject) {
    if object.is_null() {
        return;
    }
    // SAFETY: the caller's contract.
    unsafe { dealloc_at_zero(object) };
}

/// The routing both `_Py_DecRef` and the runtime's own `Py_DECREF`
/// equivalent land in.
///
/// # Safety
///
/// As `_Py_Dealloc`.
pub(crate) unsafe fn dealloc_at_zero(object: *mut PyObject) {
    // SAFETY: the header reads are within the object.
    unsafe {
        if is_counted_object(object) {
            // R: the heap's references and any pending ZCT entry defer
            // the dealloc; an object compiled code never saw (not
            // escaped) with no heap references dies at once.
            if heap_refs(object) == 0 && !zct_pending(object) && !escaped(object) {
                dealloc_object(object);
            } else {
                zct_enqueue(object);
            }
            return;
        }
        let addr = object as usize;
        match object_page(addr) {
            None => {
                // Foreign memory: the collector cannot reach it, so only
                // its own dealloc can free it.
                dealloc_object(object);
            }
            Some((page, granule)) => {
                if page.c_created(granule) && !page.escaped(granule) {
                    // A C constructor's result that never escaped into
                    // compiled code: its lifetime was the C reference
                    // alone, and it ends here.
                    dealloc_object(object);
                }
                // Otherwise the object is compiled-code memory (or an
                // escaped C object): the collector owns its death.
            }
        }
    }
}

/// Destroy `object` now: its pending finalizer (PEP 442) runs first, a
/// finalizer that resurrects aborts, then `tp_dealloc` — or the plain
/// block free for the compiler's own types. During a sweep the block
/// free queues instead (nothing is reused before phase 9), and the
/// once-per-collection claim keeps cascades from re-entering.
///
/// # Safety
///
/// `object` must be a valid object whose lifetime is over.
pub(crate) unsafe fn dealloc_object(object: *mut PyObject) {
    let addr = object as usize;
    if claims_open() && !claim_dealloc(addr) {
        return;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(object) };
    // PEP 442 at refcount death (CPython's `subtype_dealloc` runs a
    // pending finalizer before `tp_dealloc`); the bitmap bit keeps it to
    // once, ever, shared with the collection's phase 5.
    // SAFETY: the slot read is within the type object.
    if let Some(finalize) = unsafe { (*ty).tp_finalize } {
        let already = match object_page(addr) {
            Some((page, granule)) => {
                if page.finalized(granule) {
                    true
                } else {
                    page.set_finalized(granule);
                    false
                }
            }
            // Foreign objects carry no bit; they are freed below, so the
            // run is once by construction.
            None => false,
        };
        if !already {
            // SAFETY: the state is this thread's own.
            let state = tstate::current();
            // SAFETY: the finalizer call takes the object.
            unsafe {
                let saved = (*state).current_exception;
                let saved_tb = (*state).current_traceback;
                let saved_context = (*state).context;
                (*state).current_exception = core::ptr::null_mut();
                (*state).current_traceback = core::ptr::null_mut();
                (*state).context = core::ptr::null_mut();
                finalize(object);
                if !tstate::occurred().is_null() {
                    crate::types::exc::PyErr_WriteUnraisable(object);
                }
                tstate::clear();
                (*state).current_exception = saved;
                (*state).current_traceback = saved_tb;
                (*state).context = saved_context;
            }
            // A finalizer that stored the object somewhere revived it:
            // its counts rose again and this dealloc is off.
            // SAFETY: the count reads are within the object.
            let counted = unsafe { is_counted_object(object) };
            let refs = unsafe { header::refcnt(object) };
            let revived = if counted {
                (unsafe { heap_refs(object) }) > 0 || refs > 0
            } else {
                refs > 0
            };
            if revived {
                return;
            }
        }
    }
    if object_page(addr).is_none() {
        crate::alloc::foreign::unregister(addr);
    }
    side_counts()
        .lock()
        .expect("side-count map poisoned")
        .remove(&addr);
    // SAFETY: the slot read is within the type object.
    match unsafe { (*ty).tp_dealloc } {
        // SAFETY: the dealloc call takes the object.
        Some(dealloc) => unsafe { dealloc(object) },
        // The compiler's own types carry no dealloc: the block is the
        // whole object.
        // SAFETY: the object is this allocator's.
        None => {
            // SAFETY: as above.
            unsafe { crate::alloc::free_block(object.cast()) };
        }
    }
}

/// Drop a counted container's reference to `item` when the container
/// dies. During a sweep the item may already be deallocated by this
/// collection — its count is moot then — so the drop is skipped.
///
/// # Safety
///
/// `item` must be null or a valid object.
pub(crate) unsafe fn decref_unless_claimed(item: *mut PyObject) {
    if !item.is_null() && !(claims_open() && is_claimed(item as usize)) {
        // SAFETY: the C API contract.
        unsafe { _Py_DecRef(item) };
    }
}

// ---------------------------------------------------------- creation entries

/// `PyObject_Init`: the header of an extension's own allocation. The
/// regime bits follow the type: a counted type's object gets its regime
/// bit (and its side entry when it has no pre-header), foreign memory is
/// recorded so the collector never frees it.
///
/// # Safety
///
/// `object` must address `Py_TYPE`-sized writable memory that nothing
/// else has seen; `ty` must be readied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_Init(
    object: *mut PyObject,
    ty: *mut PyTypeObject,
) -> *mut PyObject {
    if object.is_null() || ty.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the caller's contract.
    unsafe { header::c_init(object, ty) };
    // SAFETY: the type is readied.
    if unsafe { type_is_counted(ty) } {
        match object_page(object as usize) {
            Some((page, granule)) => page.set_regime(granule),
            None => {
                crate::alloc::foreign::register(object as usize);
                side_counts()
                    .lock()
                    .expect("side-count map poisoned")
                    .entry(object as usize)
                    .or_insert(SideEntry {
                        heap_refs: 0,
                        zct_pending: false,
                        escaped: false,
                    });
            }
        }
    } else if object_page(object as usize).is_none() {
        crate::alloc::foreign::register(object as usize);
    }
    object
}

/// `_PyObject_New`: allocate and initialise one object of `ty`.
///
/// # Safety
///
/// `ty` must be readied; the result is freed by its `tp_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyObject_New(ty: *mut PyTypeObject) -> *mut PyObject {
    // SAFETY: the size read is within the type object.
    let size = unsafe { (*ty).tp_basicsize }.max(0) as usize;
    // SAFETY: the allocation choice follows the regime.
    let block = unsafe {
        if type_is_counted(ty) {
            crate::alloc::alloc_counted(size)
        } else {
            crate::alloc::alloc_object(size)
        }
    };
    if block.is_null() {
        crate::tstate::set_string(crate::types::exc::memory_error(), b"");
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block is `size` writable bytes.
    unsafe { PyObject_Init(block.cast(), ty) }
}

/// `PyObject_GenericAlloc`: the default `tp_alloc`, regime-aware.
///
/// # Safety
///
/// As `PyType_GenericAlloc`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GenericAlloc(
    ty: *mut PyTypeObject,
    nitems: isize,
) -> *mut PyObject {
    // SAFETY: as `PyType_GenericAlloc`.
    unsafe { crate::object::typeobject::PyType_GenericAlloc(ty, nitems) }
}

// ------------------------------------------------------------- sweep passes

/// Set between a collection's phases 5 and 9: `tp_free` queues the block
/// instead of freeing, so the heap walk's page bits stay honest while
/// destructors run (section 9.3).
static SWEEPING: AtomicBool = AtomicBool::new(false);

/// The per-collection sweep state: the queued block frees, and the
/// once-per-collection claim that keeps the sweep's cascades from
/// deallocating an object twice. `claims_open` outlives the queueing
/// window: the ZCT drain after the sweep still claims, so its frees
/// count and its cascades stay once-only.
#[derive(Default)]
struct SweepState {
    pending_free: Vec<usize>,
    deallocated: HashSet<usize>,
    claims_open: bool,
}

fn sweep_state() -> &'static Mutex<SweepState> {
    static STATE: OnceLock<Mutex<SweepState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(SweepState::default()))
}

/// Whether a sweep is in progress.
pub(crate) fn is_sweeping() -> bool {
    SWEEPING.load(Ordering::Relaxed)
}

/// Whether this collection's dealloc claims are taken: from `begin_sweep`
/// until the post-sweep ZCT drain has finished.
fn claims_open() -> bool {
    sweep_state()
        .lock()
        .expect("sweep state poisoned")
        .claims_open
}

/// Queue a block free the sweep deferred; the flush after phase 8 frees
/// them for real.
///
/// # Safety
///
/// The address must be one `free_block` validated during the sweep.
pub(crate) unsafe fn queue_sweep_free(addr: usize) {
    sweep_state()
        .lock()
        .expect("sweep state poisoned")
        .pending_free
        .push(addr);
}

/// The once-per-collection dealloc claim: first caller wins.
fn claim_dealloc(addr: usize) -> bool {
    sweep_state()
        .lock()
        .expect("sweep state poisoned")
        .deallocated
        .insert(addr)
}

/// Whether `addr` was deallocated by this collection already.
pub(crate) fn is_claimed(addr: usize) -> bool {
    sweep_state()
        .lock()
        .expect("sweep state poisoned")
        .deallocated
        .contains(&addr)
}

/// Begin the sweep window (phases 5 through 8 run destructors).
pub(crate) fn begin_sweep() {
    SWEEPING.store(true, Ordering::Relaxed);
    let mut state = sweep_state().lock().expect("sweep state poisoned");
    state.pending_free.clear();
    state.deallocated.clear();
    state.claims_open = true;
}

/// End the sweep window and free every queued block. Returns how many
/// of `dead` this collection deallocated — the collection's own count,
/// not the transients a destructor created and dropped along the way.
pub(crate) fn end_sweep(dead: &[usize]) -> usize {
    SWEEPING.store(false, Ordering::Release);
    let mut state = sweep_state().lock().expect("sweep state poisoned");
    let queued = core::mem::take(&mut state.pending_free);
    let before = state.deallocated.clone();
    drop(state);
    // The flag is clear, so these free for real; a block may free from
    // either side of its counted pre-header.
    for addr in queued {
        // SAFETY: every address was one `free_block` validated during
        // the sweep.
        unsafe { crate::alloc::free_block(addr as *mut u8) };
    }
    // Phase 8's barrier decrements brought R objects to zero whose last
    // holder was itself collected: they joined the ZCT inside the sweep
    // and are freed in this same collection (section 9.3), not at the
    // next trigger. Cascades in the drain can enqueue more, so it runs
    // until the tables stop producing frees.
    if !ZCT_PROCESSING.swap(true, Ordering::AcqRel) {
        while run_zct_pass() != 0 {}
        ZCT_PROCESSING.store(false, Ordering::Release);
    }
    let mut state = sweep_state().lock().expect("sweep state poisoned");
    let swept = dead
        .iter()
        .filter(|addr| state.deallocated.contains(*addr))
        .count();
    let drained = state
        .deallocated
        .iter()
        .filter(|addr| !before.contains(addr) && !dead.contains(addr))
        .count();
    state.claims_open = false;
    swept + drained
}

/// Phase 3's subtraction: which snapshot objects a positive count still
/// holds. A shadow `ob_refcnt` — the real count minus the references
/// other objects' counted slots and GC-tracked counted objects hold —
/// decides the T regime; an R object is additionally held by any heap
/// reference or ZCT entry.
///
/// # Safety
///
/// Every address must be a live heap object of this allocator.
pub(crate) unsafe fn subtract(objects: &[usize]) -> Vec<*mut PyObject> {
    // The two internal-reference maps of gcmodule.c's subtraction, one
    // per contribution to a lifetime: `internal` holds the references
    // other objects' counted slots and GC-tracked counted objects keep
    // in `ob_refcnt`, `heap_internal` the barrier-accounted heap
    // references traced containers and compiled objects hold.
    let mut internal: HashMap<usize, usize> = HashMap::new();
    let mut heap_internal: HashMap<usize, usize> = HashMap::new();
    for &addr in objects {
        let counted_refs = holds_internal_refs(addr);
        let heap_refs = holds_heap_edge_refs(addr);
        if !counted_refs && !heap_refs {
            continue;
        }
        // SAFETY: the address is a live object.
        let object = addr as *mut PyObject;
        // SAFETY: the header read is within the object.
        let ty = unsafe { header::type_of(object) };
        // SAFETY: the slot read is within the type object.
        let Some(traverse) = (unsafe { (*ty).tp_traverse }) else {
            continue;
        };
        // SAFETY: the visitor contract.
        unsafe {
            if counted_refs {
                traverse(
                    object,
                    visit_bump,
                    &mut internal as *mut _ as *mut core::ffi::c_void,
                );
            }
            if heap_refs {
                traverse(
                    object,
                    visit_bump,
                    &mut heap_internal as *mut _ as *mut core::ffi::c_void,
                );
            }
        }
    }
    let mut held = Vec::new();
    for &addr in objects {
        let object = addr as *mut PyObject;
        // SAFETY: the header read is within the object.
        let shadow =
            unsafe { header::refcnt(object) } - internal.get(&addr).copied().unwrap_or(0) as isize;
        // SAFETY: the count read is within the object's bookkeeping.
        unsafe {
            if is_counted_object(object) {
                let heap =
                    heap_refs(object) - heap_internal.get(&addr).copied().unwrap_or(0) as isize;
                if shadow > 0 || heap > 0 || zct_pending(object) {
                    held.push(object);
                }
            } else if shadow > 0 {
                held.push(object);
            }
        }
    }
    held
}

/// `subtract`'s visitor: one internal reference found.
unsafe extern "C" fn visit_bump(
    object: *mut PyObject,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    if !object.is_null() {
        // SAFETY: `arg` is the map `subtract` passed.
        let internal = unsafe { &mut *(arg as *mut HashMap<usize, usize>) };
        *internal.entry(object as usize).or_insert(0) += 1;
    }
    0
}

/// Whether the object at `addr` holds references that are `ob_refcnt`
/// units in other objects: a counted container's slots, or the INCREF'd
/// slots of a GC-tracked counted object (a C data structure the GC
/// tracks, section 9.3).
fn holds_internal_refs(addr: usize) -> bool {
    let Some((page, granule)) = object_page(addr) else {
        return false;
    };
    if page.counted_slots(granule) {
        return true;
    }
    page.regime(granule)
        // SAFETY: the address is a live object of a readied type.
        && unsafe { (*header::type_of(addr as *mut PyObject)).tp_traverse }.is_some()
}

/// Whether `addr` holds barrier-accounted heap references: a traced
/// container — compiled code's own, or a C-created one whose stores go
/// through the functions that carry the barrier — or any other
/// compiled-code object. These are the edges `verify_heap_refs`
/// recomputes; an R object's `heap_refs` only ever counts them, so the
/// subtraction must see them as internal (a cycle through a dict and an
/// extension object is row 7 of the timing table).
fn holds_heap_edge_refs(addr: usize) -> bool {
    let Some((page, granule)) = object_page(addr) else {
        return false;
    };
    if page.regime(granule) || page.counted_slots(granule) {
        return false;
    }
    // Raw allocations carry no type until `PyObject_Init` runs.
    // SAFETY: the header read is within the block.
    let ty = unsafe { header::type_of(addr as *mut PyObject) };
    if ty.is_null() {
        return false;
    }
    // SAFETY: the slot read is within the type object.
    unsafe { (*ty).tp_traverse }.is_some()
        && (!page.c_created(granule)
            || barrier_container_types()
                .lock()
                .expect("barrier-container set poisoned")
                .contains(&(ty as usize)))
}

/// Phase 7: `tp_clear` on every dead GC-tracked counted object — the
/// extension structs whose cycles the refcounts alone cannot break.
/// The `Py_CLEAR`s inside route through `_Py_DecRef`, whose sweep window
/// records cascaded deallocs instead of freeing blocks.
///
/// # Safety
///
/// Every address must be a dead heap object of this allocator.
pub(crate) unsafe fn clear_pass(dead: &[usize]) {
    for &addr in dead {
        // A finalizer may have resurrected this one back into a root:
        // re-marked entries are alive again and none of the destructive
        // passes may touch them.
        if mark::is_marked(addr) == Some(true) {
            continue;
        }
        // An earlier `tp_clear` may have deallocated this one already:
        // a cycle's members free each other inline (the claim below the
        // sweep window), and a dead block must not be cleared again.
        if is_claimed(addr) {
            continue;
        }
        let Some((page, granule)) = object_page(addr) else {
            continue;
        };
        if !page.regime(granule) {
            continue;
        }
        let object = addr as *mut PyObject;
        // SAFETY: the header read is within the object.
        let ty = unsafe { header::type_of(object) };
        // SAFETY: the slot read is within the type object.
        if let Some(clear) = unsafe { (*ty).tp_clear } {
            // SAFETY: the clear call takes the dead object.
            unsafe { clear(object) };
        }
    }
}

/// Phase 8: `tp_dealloc` once on every dead R object and every dead
/// C-created T object. First the dead traced T objects whose slots hold
/// `heap_refs` units — the ones no destructor will run for — give them
/// up, in this sweep; then the deallocs, guarded by the once-claim.
///
/// # Safety
///
/// Every address must be a dead heap object of this allocator.
pub(crate) unsafe fn dealloc_pass(dead: &[usize]) {
    // (a) the heap_refs subtraction for objects the sweep frees without
    // a destructor: compiled-code objects and `from_array` tuples.
    for &addr in dead {
        if mark::is_marked(addr) == Some(true) {
            continue;
        }
        let Some((page, granule)) = object_page(addr) else {
            continue;
        };
        if page.regime(granule) || page.c_created(granule) {
            continue;
        }
        let object = addr as *mut PyObject;
        // SAFETY: the header read is within the object.
        let ty = unsafe { header::type_of(object) };
        if ty.is_null() {
            // Raw allocations carry no type until `PyObject_Init` runs.
            continue;
        }
        // SAFETY: the slot read is within the type object.
        let Some(traverse) = (unsafe { (*ty).tp_traverse }) else {
            continue;
        };
        // SAFETY: the visitor contract; the visit only drops counts.
        unsafe { traverse(object, visit_release, core::ptr::null_mut()) };
    }
    // (b) the deallocations, in the dead list's heap-walk order.
    for &addr in dead {
        if mark::is_marked(addr) == Some(true) {
            continue;
        }
        let Some((page, granule)) = object_page(addr) else {
            continue;
        };
        if !page.regime(granule) && !page.c_created(granule) {
            continue;
        }
        // SAFETY: the address is a dead object whose lifetime is over.
        unsafe { dealloc_object(addr as *mut PyObject) };
    }
}

/// `dealloc_pass` (a)'s visitor: one heap reference given up.
unsafe extern "C" fn visit_release(
    object: *mut PyObject,
    _arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    if !object.is_null() {
        // SAFETY: the referent is an object this dead one referenced.
        unsafe { barrier_dec(object) };
    }
    0
}

// --------------------------------------------------- the debug-heap checks

/// The `--features debug-heap` quarantine: freed R objects keep their
/// block for one more collection — the bits say "not an object", so no
/// walk sees them and no allocator reuses them — and the next
/// collection's start frees them for real.
#[cfg(feature = "debug-heap")]
fn quarantine() -> &'static Mutex<Vec<usize>> {
    static LIST: OnceLock<Mutex<Vec<usize>>> = OnceLock::new();
    LIST.get_or_init(|| Mutex::new(Vec::new()))
}

/// Quarantine the counted object whose block was just freed, if `object`
/// is one: poison the body, hide it from the walks, keep the block.
///
/// # Safety
///
/// `object` must be a live counted object about to be freed.
#[cfg(feature = "debug-heap")]
pub(crate) unsafe fn quarantine_counted(object: usize) -> bool {
    let Some((page, granule)) = object_page(object) else {
        return false;
    };
    if !page.regime(granule) {
        return false;
    }
    let base = page.base();
    let block = if page.is_small() {
        page.block_start(base, object)
    } else {
        base + crate::alloc::page::DATA_OFFSET
    };
    // The block's pre-header marker is what makes a counted allocation;
    // the regime bit alone can sit on any block whose bits a caller
    // juggled by hand.
    if !page.has_prehdr(page.granule_of(base, block)) {
        return false;
    }
    // Poison every byte of the body; the allocator's free-list link
    // takes the block's first word.
    let capacity = unsafe { crate::alloc::block_capacity(block as *const u8) }.unwrap_or(0);
    if capacity > 8 {
        // SAFETY: the block is dead and exclusively the runtime's.
        unsafe {
            core::ptr::write_bytes(
                (block + 8) as *mut u8,
                crate::alloc::page::POISON_BYTE,
                capacity - 8,
            )
        };
    }
    // Hide the object from every walk and barrier test, but keep the
    // block and its pre-header marker so the deferred free resolves the
    // block again. The object bits all sit at the object's granule.
    page.clear_is_object(granule);
    page.clear_regime(granule);
    page.clear_escaped(granule);
    page.clear_c_created(granule);
    page.clear_counted_slots(granule);
    page.clear_mark(granule);
    page.clear_finalized(granule);
    quarantine()
        .lock()
        .expect("quarantine list poisoned")
        .push(object);
    true
}

/// Free last cycle's quarantined objects, at the start of a collection.
#[cfg(feature = "debug-heap")]
pub(crate) fn release_quarantine() {
    let queued = core::mem::take(&mut *quarantine().lock().expect("quarantine list poisoned"));
    for object in queued {
        // SAFETY: the address was a counted object whose block the
        // quarantine kept; the regime bits are clear, so this frees for
        // real.
        unsafe { crate::alloc::free_block(object as *mut u8) };
    }
}

/// The `--features debug-heap` recomputation: walk the heap, count every
/// live traced-T edge into an R object the barrier could have made, and
/// compare with the stored `heap_refs`. A mismatch means a store path
/// skipped the barrier.
///
/// # Safety
///
/// Every address must be a live heap object of this allocator.
#[cfg(feature = "debug-heap")]
pub(crate) unsafe fn verify_heap_refs(objects: &[usize]) {
    let mut counted: HashMap<usize, isize> = HashMap::new();
    for &addr in objects {
        let object = addr as *mut PyObject;
        // SAFETY: the header read is within the object.
        let ty = unsafe { header::type_of(object) };
        if ty.is_null() {
            // Raw allocations carry no type until `PyObject_Init` runs.
            continue;
        }
        // Type objects hold their graph by `ob_refcnt`, the compiler's
        // own objects and the traced containers by the barrier.
        if core::ptr::eq(ty, &raw mut crate::object::typeobject::PyType_Type) {
            continue;
        }
        let Some((page, granule)) = object_page(addr) else {
            continue;
        };
        if page.regime(granule) || page.counted_slots(granule) {
            continue;
        }
        if page.c_created(granule)
            && !barrier_container_types()
                .lock()
                .expect("barrier-container registry poisoned")
                .contains(&(ty as usize))
        {
            // A C constructor's object: its references are `ob_refcnt`
            // units (iterators' sequences, exception fields), except
            // the traced containers.
            continue;
        }
        // SAFETY: the slot read is within the type object.
        let Some(traverse) = (unsafe { (*ty).tp_traverse }) else {
            continue;
        };
        // SAFETY: the visitor contract.
        unsafe {
            traverse(
                object,
                visit_count,
                &mut counted as *mut _ as *mut core::ffi::c_void,
            )
        };
    }
    for &addr in objects {
        let object = addr as *mut PyObject;
        // SAFETY: the count read is within the object's bookkeeping.
        if !unsafe { is_counted_object(object) } {
            continue;
        }
        let expected = counted.get(&addr).copied().unwrap_or(0);
        // SAFETY: as above.
        let actual = unsafe { heap_refs(object) };
        assert_eq!(
            actual, expected,
            "debug-heap: heap_refs mismatch at {object:p}"
        );
    }
}

/// `verify_heap_refs`'s visitor: one traced edge into an R object.
#[cfg(feature = "debug-heap")]
unsafe extern "C" fn visit_count(
    object: *mut PyObject,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    if !object.is_null() && unsafe { is_counted_object(object) } {
        // SAFETY: `arg` is the map `verify_heap_refs` passed.
        let counted = unsafe { &mut *(arg as *mut HashMap<usize, isize>) };
        *counted.entry(object as usize).or_insert(0) += 1;
    }
    0
}

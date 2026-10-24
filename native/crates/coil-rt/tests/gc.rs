//! Collector conformance (spec sections 9.2 to 9.6): reachability, cycles,
//! pins, PEP 442 finalisation with resurrection, weak-reference clearing
//! and callback order, stack-map frame walking with synthetic frames,
//! module tables and `sys.modules` as roots, pacing, `gc.collect`,
//! `gc.disable`, `gc.get_objects`, `gc.get_referents`, and the
//! multi-threaded allocate-and-collect hammer.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use coil_capi_sys::{PyModuleDef, PyObject, PyTypeObject, visitproc};
use python312::gc;
use python312::gc::{roots, stack, weakref};
use python312::object::{singletons, typeobject};
use python312::tstate;
use python312::types::exc as exceptions;
use python312::types::{list, long, tuple};

static INIT: Once = Once::new();

/// The tests mutate process-global collector state — the root registries,
/// the weak-reference table, the thread states — so they serialize, like
/// the GIL tests do.
static SERIAL: Mutex<()> = Mutex::new(());

fn init() -> MutexGuard<'static, ()> {
    INIT.call_once(|| {
        assert_eq!(typeobject::init_static_types(), 0);
        // Explicit collections only: pacing has its own test, which
        // re-enables the switch around its allocations.
        gc::disable();
    });
    SERIAL.lock().unwrap()
}

/// Drop the reference count a C API constructor left: the object becomes
/// what compiled code owns — no C references, lifetime by reachability
/// alone (compiled code never touches `ob_refcnt`).
fn unpin(object: *mut PyObject) {
    // SAFETY: the header write hands back the constructor's reference;
    // the tests own every object they unpin.
    unsafe {
        (*object).ob_refcnt = 0;
    }
}

/// Whether `object` is in `gc.get_objects()`'s live set: the public
/// liveness probe. The tuple `get_objects` returns is the suite's own
/// reference; dropping and freeing it here keeps it from pinning the
/// whole heap for the next collection.
fn live(object: *mut PyObject) -> bool {
    let objects = gc::get_objects();
    let mut found = false;
    // SAFETY: `get_objects` returns a fresh tuple of live objects.
    unsafe {
        for i in 0..tuple::PyTuple_Size(objects) {
            if core::ptr::eq(tuple::PyTuple_GetItem(objects, i), object) {
                found = true;
                break;
            }
        }
        (*objects).ob_refcnt = 0;
        assert!(python312::alloc::free_block(objects.cast()));
    }
    found
}

/// Beyond the small-int cache: every `int` the tests unpin is a heap
/// object the collector can free.
fn int(value: i64) -> *mut PyObject {
    long::PyLong_FromLongLong(value)
}

// -------------------------------------------------- the tests' object types

/// Slot offsets of the test node: three reference slots after the header.
const SLOT0: usize = 16;
const SLOT1: usize = 24;
const SLOT2: usize = 32;

fn slot(object: *mut PyObject, offset: usize) -> *mut PyObject {
    // SAFETY: the node's slots are within its allocation and written by
    // the tests alone.
    unsafe { core::ptr::read_unaligned((object as *const u8).add(offset) as *const *mut PyObject) }
}

fn set_slot(object: *mut PyObject, offset: usize, value: *mut PyObject) {
    // SAFETY: as `slot`, and the object is not shared across threads.
    unsafe {
        core::ptr::write_unaligned((object as *mut u8).add(offset) as *mut *mut PyObject, value)
    }
}

/// What the finalizable node's finalizer observed.
static FINALIZER_LOG: Mutex<Vec<usize>> = Mutex::new(Vec::new());
/// Whether the finalizer saw a pending exception (it must not).
static FINALIZER_SAW_EXCEPTION: AtomicBool = AtomicBool::new(false);
/// Whether the node's finalizer resurrects its object, and where into: a
/// registered module table's single slot, the runtime's own root
/// mechanism.
static RESURRECT_NODE: AtomicBool = AtomicBool::new(false);
/// Whether the node's finalizer raises, for the unraisable path.
static FINALIZER_RAISES: AtomicBool = AtomicBool::new(false);
static RESURRECT_SLOT: Mutex<usize> = Mutex::new(0);

fn store_into_resurrect_slot(object: *mut PyObject) {
    let target = *RESURRECT_SLOT.lock().unwrap();
    if target != 0 {
        // SAFETY: the slot belongs to a registered module table that
        // lives for the process.
        unsafe { core::ptr::write_unaligned(target as *mut *mut PyObject, object) };
    }
}

unsafe extern "C" fn node_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    for offset in [SLOT0, SLOT1, SLOT2] {
        let referent = slot(object, offset);
        if !referent.is_null() {
            // SAFETY: the visitor contract.
            if unsafe { visit(referent, arg) } != 0 {
                return -1;
            }
        }
    }
    0
}

unsafe extern "C" fn node_finalize(object: *mut PyObject) {
    FINALIZER_SAW_EXCEPTION.store(!tstate::occurred().is_null(), Ordering::Relaxed);
    FINALIZER_LOG.lock().unwrap().push(object as usize);
    if RESURRECT_NODE.load(Ordering::Relaxed) {
        store_into_resurrect_slot(object);
    }
    if FINALIZER_RAISES.load(Ordering::Relaxed) {
        tstate::set_string(exceptions::value_error(), b"finalizer failure");
    }
}

const fn node_type() -> PyTypeObject {
    let mut t = typeobject::zero_type();
    t.tp_name = c"gc_test_node".as_ptr();
    t.tp_basicsize = 64;
    t.tp_flags = coil_capi_sys::Py_TPFLAGS_DEFAULT
        | coil_capi_sys::Py_TPFLAGS_HAVE_GC
        | coil_capi_sys::Py_TPFLAGS_BASETYPE;
    t.tp_traverse = Some(node_traverse);
    t.tp_finalize = Some(node_finalize);
    // The capability flag alone: the weak-reference table holds the
    // weakrefs, the offset field is never dereferenced.
    t.tp_weaklistoffset = 56;
    t.tp_alloc = Some(typeobject::PyType_GenericAlloc);
    t
}

#[unsafe(no_mangle)]
static mut NODE_TYPE: PyTypeObject = node_type();

fn new_node() -> *mut PyObject {
    // SAFETY: the type's alloc is the runtime's own generic allocator.
    unsafe { typeobject::PyType_GenericAlloc(&raw mut NODE_TYPE, 0) }
}

/// What the weakref callbacks observed: each entry is the weakref the
/// callback was called with.
static CALLBACK_LOG: Mutex<Vec<usize>> = Mutex::new(Vec::new());

unsafe extern "C" fn logging_call(
    _self: *mut PyObject,
    args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the collector calls back with a one-element tuple.
    let weak = unsafe { tuple::PyTuple_GetItem(args, 0) };
    CALLBACK_LOG.lock().unwrap().push(weak as usize);
    singletons::none()
}

const fn callback_type() -> PyTypeObject {
    let mut t = typeobject::zero_type();
    t.tp_name = c"gc_test_callback".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::object::SIZEOF as isize;
    t.tp_call = Some(logging_call);
    t
}

#[unsafe(no_mangle)]
static mut CALLBACK_TYPE: PyTypeObject = callback_type();

fn new_callback() -> *mut PyObject {
    // SAFETY: as `new_node`.
    unsafe { typeobject::PyType_GenericAlloc(&raw mut CALLBACK_TYPE, 0) }
}

/// A callback that resurrects the weakref it is called with, by storing it
/// into the module slot.
unsafe extern "C" fn resurrecting_call(
    _self: *mut PyObject,
    args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the collector calls back with a one-element tuple.
    let weak = unsafe { tuple::PyTuple_GetItem(args, 0) };
    store_into_resurrect_slot(weak);
    singletons::none()
}

const fn resurrecting_callback_type() -> PyTypeObject {
    let mut t = typeobject::zero_type();
    t.tp_name = c"gc_test_resurrector".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::object::SIZEOF as isize;
    t.tp_call = Some(resurrecting_call);
    t
}

#[unsafe(no_mangle)]
static mut RESURRECTOR_TYPE: PyTypeObject = resurrecting_callback_type();

fn new_resurrector() -> *mut PyObject {
    // SAFETY: as `new_node`.
    unsafe { typeobject::PyType_GenericAlloc(&raw mut RESURRECTOR_TYPE, 0) }
}

/// The sibling weakref the observing callback reads through the public
/// entry point during phase 6, and what that read reported: the GetRef
/// return code, and whether it handed back NULL.
static SIBLING_WEAK: Mutex<usize> = Mutex::new(0);
static SIBLING_SEEN: Mutex<(core::ffi::c_int, bool)> = Mutex::new((0, false));

unsafe extern "C" fn observing_call(
    _self: *mut PyObject,
    _args: *mut PyObject,
    _kwds: *mut PyObject,
) -> *mut PyObject {
    let sibling = *SIBLING_WEAK.lock().unwrap();
    if sibling != 0 {
        let mut got = core::ptr::null_mut();
        // SAFETY: the static holds a live weakref of this runtime.
        let rc = unsafe { weakref::PyWeakref_GetRef(sibling as *mut PyObject, &mut got) };
        let handed_null = got.is_null();
        if rc == 1 {
            // SAFETY: the strong reference the contract hands back.
            unsafe { (*got).ob_refcnt -= 1 };
        }
        *SIBLING_SEEN.lock().unwrap() = (rc, handed_null);
    }
    singletons::none()
}

const fn observing_callback_type() -> PyTypeObject {
    let mut t = typeobject::zero_type();
    t.tp_name = c"gc_test_observer".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::object::SIZEOF as isize;
    t.tp_call = Some(observing_call);
    t
}

#[unsafe(no_mangle)]
static mut OBSERVER_TYPE: PyTypeObject = observing_callback_type();

fn new_observer() -> *mut PyObject {
    // SAFETY: as `new_node`.
    unsafe { typeobject::PyType_GenericAlloc(&raw mut OBSERVER_TYPE, 0) }
}

// ------------------------------------------------- the tests' root mechanism

/// A one-slot module table, registered as a root: the tests' stand-in for
/// a compiled module global. The table is leaked — a module's table lives
/// for the process.
struct ModuleSlot {
    slots: &'static mut [*mut PyObject; 1],
}

impl ModuleSlot {
    fn new(object: *mut PyObject) -> ModuleSlot {
        let slots: &'static mut [*mut PyObject; 1] = Box::leak(Box::new([object]));
        // SAFETY: the table is leaked, so the registration outlives the
        // run and the collector reads one live pointer.
        unsafe {
            assert_eq!(
                roots::coil_rt_register_module_table(slots.as_mut_ptr(), 1),
                0
            );
        }
        ModuleSlot { slots }
    }

    fn set(&mut self, object: *mut PyObject) {
        self.slots[0] = object;
    }

    fn get(&self) -> *mut PyObject {
        self.slots[0]
    }

    /// The slot's address, for the finalizer's resurrection store.
    fn slot_address(&mut self) -> usize {
        &mut self.slots[0] as *mut _ as usize
    }
}

fn weakref_get(weak: *mut PyObject) -> *mut PyObject {
    let mut got = core::ptr::null_mut();
    // SAFETY: the tests only pass weakrefs.
    unsafe {
        match weakref::PyWeakref_GetRef(weak, &mut got) {
            // 1: the referent lives and the call handed back a strong
            // reference, which the tests return; 0: cleared, `got` NULL.
            1 => (*got).ob_refcnt -= 1,
            0 => {}
            other => panic!("PyWeakref_GetRef failed: {other}"),
        }
    }
    got
}

// ---------------------------------------------------------------- the tests

#[test]
fn rooted_objects_survive_and_unrooted_objects_die() {
    let _serial = init();
    let rooted = new_node();
    unpin(rooted);
    let mut table = ModuleSlot::new(rooted);
    let floating = new_node();
    unpin(floating);

    assert_eq!(gc::collect(), 1, "exactly the unrooted node");
    assert!(live(rooted), "the module-table root holds its node");
    assert!(!live(floating));

    table.set(core::ptr::null_mut());
    assert_eq!(gc::collect(), 1);
    assert!(!live(rooted));
}

#[test]
fn unreachable_cycles_are_collected() {
    let _serial = init();
    // A two-node cycle through tp_traverse slots. The nodes die as
    // compiled-code objects: no C references, lifetime by reachability.
    let a = new_node();
    let b = new_node();
    set_slot(a, SLOT0, b);
    set_slot(b, SLOT0, a);
    unpin(a);
    unpin(b);
    // A self-referential list. `PyList_SetItem` steals the caller's
    // reference, so the list's own count of one is the slot's; the
    // subtraction of internal references (spec 9.3 phase 3) is what
    // frees it, and its dealloc drops the slot's reference to itself.
    let list_object = list::PyList_New(1);
    // SAFETY: the list was built with one empty slot.
    unsafe {
        assert_eq!(list::PyList_SetItem(list_object, 0, list_object), 0);
    }

    assert_eq!(gc::collect(), 3);
    assert!(!live(a) && !live(b) && !live(list_object));
}

#[test]
fn c_references_pin_objects() {
    let _serial = init();
    let pinned = new_node();
    // The constructor's reference: ob_refcnt 1 keeps the node alive with
    // no other root (the pin rule of section 9.3).
    assert_eq!(gc::collect(), 0);
    assert!(live(pinned));
    unpin(pinned);
    assert_eq!(gc::collect(), 1);
    assert!(!live(pinned));
}

#[test]
fn finalizers_run_once_and_resurrection_remarks() {
    let _serial = init();
    FINALIZER_LOG.lock().unwrap().clear();
    RESURRECT_NODE.store(true, Ordering::Relaxed);
    let object = new_node();
    unpin(object);
    // The finalizer resurrects through the module slot.
    let mut table = ModuleSlot::new(core::ptr::null_mut());
    *RESURRECT_SLOT.lock().unwrap() = table.slot_address();

    assert_eq!(gc::collect(), 0, "the resurrected object survives");
    assert_eq!(FINALIZER_LOG.lock().unwrap().as_slice(), [object as usize]);
    assert!(live(object));
    assert_eq!(table.get() as usize, object as usize);

    // Drop the resurrection root: the next collection frees the object
    // without running the finalizer again (PEP 442: at most once, ever).
    table.set(core::ptr::null_mut());
    *RESURRECT_SLOT.lock().unwrap() = 0;
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    assert_eq!(gc::collect(), 1);
    assert!(!live(object));
    assert_eq!(
        FINALIZER_LOG.lock().unwrap().as_slice(),
        [object as usize],
        "the finalizer ran exactly once"
    );
}

#[test]
fn finalizers_run_with_the_exception_context_cleared() {
    let _serial = init();
    FINALIZER_LOG.lock().unwrap().clear();
    FINALIZER_SAW_EXCEPTION.store(false, Ordering::Relaxed);
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    let object = new_node();
    let held = int(987_654_321);
    set_slot(object, SLOT0, held);
    unpin(held);
    unpin(object);

    // A pending exception rides in the thread state while the collection
    // runs; the finalizer must see none (section 9.6), and the pending
    // exception must survive the collection for the caller.
    tstate::set_string(
        exceptions::runtime_error(),
        b"pending across the collection",
    );
    assert_eq!(gc::collect(), 2);
    assert!(
        !FINALIZER_SAW_EXCEPTION.load(Ordering::Relaxed),
        "the finalizer saw a cleared exception context"
    );
    assert!(core::ptr::eq(
        tstate::occurred(),
        exceptions::runtime_error()
    ));
    tstate::clear();
    assert_eq!(FINALIZER_LOG.lock().unwrap().as_slice(), [object as usize]);
}

#[test]
fn a_finalizer_exception_is_written_unraisably() {
    let _serial = init();
    FINALIZER_LOG.lock().unwrap().clear();
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    FINALIZER_RAISES.store(true, Ordering::Relaxed);
    let object = new_node();
    unpin(object);
    // The finalizer raises; the collector must write the exception
    // unraisably and complete the collection with a clean thread state.
    assert_eq!(gc::collect(), 1);
    assert!(
        tstate::occurred().is_null(),
        "the unraisable path cleared it"
    );
    FINALIZER_RAISES.store(false, Ordering::Relaxed);
    assert_eq!(FINALIZER_LOG.lock().unwrap().as_slice(), [object as usize]);
}

#[test]
fn weakrefs_clear_and_callbacks_run_in_creation_order() {
    let _serial = init();
    CALLBACK_LOG.lock().unwrap().clear();
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    let target = new_node();
    let second = new_node();
    let mut table = ModuleSlot::new(target);
    let mut second_table = ModuleSlot::new(second);
    let c1 = new_callback();
    let c2 = new_callback();
    let c3 = new_callback();
    unpin(c1);
    unpin(c2);
    unpin(c3);
    // SAFETY: the targets support weakrefs and the callbacks are callable.
    let w1 = unsafe { weakref::PyWeakref_NewRef(target, c1) };
    let w2 = unsafe { weakref::PyWeakref_NewRef(target, c2) };
    let plain = unsafe { weakref::PyWeakref_NewRef(target, core::ptr::null_mut()) };
    let w3 = unsafe { weakref::PyWeakref_NewRef(second, c3) };
    assert!(!w1.is_null() && !w2.is_null() && !plain.is_null() && !w3.is_null());

    // Rooted targets keep their weakrefs silent.
    assert_eq!(gc::collect(), 0);
    assert_eq!(CALLBACK_LOG.lock().unwrap().len(), 0);
    assert!(core::ptr::eq(weakref_get(w1), target));

    // Drop both targets: the callbacks run in creation order — the two on
    // the first target, then the one on the second — and every weakref
    // reads back dead. The tests keep the weakrefs' constructor
    // references, so only the two targets free.
    unpin(target);
    unpin(second);
    table.set(core::ptr::null_mut());
    second_table.set(core::ptr::null_mut());
    assert_eq!(gc::collect(), 2, "the two targets");
    let log = CALLBACK_LOG.lock().unwrap();
    assert_eq!(log.as_slice(), [w1 as usize, w2 as usize, w3 as usize]);
    drop(log);
    for weak in [w1, w2, w3, plain] {
        assert!(weakref_get(weak).is_null(), "cleared reads back dead");
    }
}

#[test]
fn a_weakref_callback_resurrects_the_weakref() {
    let _serial = init();
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    let target = new_node();
    unpin(target);
    let callback = new_resurrector();
    unpin(callback);
    // The resurrection slot starts empty; the callback fills it with the
    // weakref it is handed during phase 6.
    let mut table = ModuleSlot::new(core::ptr::null_mut());
    *RESURRECT_SLOT.lock().unwrap() = table.slot_address();
    // SAFETY: the target supports weakrefs; the callback is callable.
    let weak = unsafe { weakref::PyWeakref_NewRef(target, callback) };
    assert!(!weak.is_null());
    unpin(weak);

    // The target dies; the callback resurrects the weakref by storing it
    // into the module slot, and the re-mark after the callbacks keeps it.
    assert_eq!(gc::collect(), 1);
    assert!(live(weak), "the callback's store re-marked the weakref");
    assert_eq!(table.get() as usize, weak as usize);
    assert!(weakref_get(weak).is_null(), "cleared reads back dead");

    *RESURRECT_SLOT.lock().unwrap() = 0;
    table.set(core::ptr::null_mut());
    assert_eq!(gc::collect(), 2, "the weakref and its callback");
    assert!(!live(weak) && !live(callback) && !live(target));
}

#[test]
fn a_deallocated_weakref_leaves_no_stale_row() {
    let _serial = init();
    CALLBACK_LOG.lock().unwrap().clear();
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    let target = new_node();
    unpin(target);
    let callback = new_callback();
    unpin(callback);
    // SAFETY: the target supports weakrefs and the callback is callable.
    let weak = unsafe { weakref::PyWeakref_NewRef(target, callback) };
    assert!(!weak.is_null());

    // Deallocation through the public contract: the last reference drops
    // and the type's own dealloc runs, as it would from C. The row must
    // go with the block — a stale row survives into a later phase 6,
    // where the recycled address turns the clear and the callback call
    // into writes into a foreign object.
    unpin(weak);
    // SAFETY: the weakref is dead by reference count and its type's
    // dealloc is the runtime's own.
    unsafe {
        let dealloc = (*(*weak).ob_type).tp_dealloc.unwrap();
        dealloc(weak);
    }

    // The freed block is exactly a node's size class: the next same-class
    // allocation reuses it deterministically (the cache's free list is
    // LIFO and the suite is serialised).
    let node = new_node();
    assert_eq!(
        node as usize, weak as usize,
        "the freed weakref's block was reused"
    );
    let held = int(987_654_321);
    unpin(held);
    set_slot(node, SLOT0, held); // +16: where a stale row reads wr_object
    set_slot(node, SLOT1, callback); // +24: where a stale row reads wr_callback

    // The node keeps its constructor's reference, so it and everything
    // its slots reach survive; a stale row would overwrite SLOT0 with
    // None and call the callback with the node as its "weakref".
    assert_eq!(gc::collect(), 1, "only the dead target frees");
    assert!(
        core::ptr::eq(slot(node, SLOT0), held),
        "the slot was not cleared"
    );
    assert_eq!(CALLBACK_LOG.lock().unwrap().len(), 0, "no callback ran");
    assert!(live(node) && live(held) && live(callback));
}

#[test]
fn a_callback_sees_sibling_weakrefs_already_cleared() {
    let _serial = init();
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    let target = new_node();
    unpin(target);
    let callback = new_observer();
    unpin(callback);
    *SIBLING_WEAK.lock().unwrap() = 0;
    *SIBLING_SEEN.lock().unwrap() = (0, false);
    // The observer comes first, so its callback runs before the sibling
    // has fired and observes it mid-phase-6.
    // SAFETY: the target supports weakrefs and the callback is callable.
    let observing = unsafe { weakref::PyWeakref_NewRef(target, callback) };
    // SAFETY: a plain sibling weakref to the same target.
    let sibling = unsafe { weakref::PyWeakref_NewRef(target, core::ptr::null_mut()) };
    assert!(!observing.is_null() && !sibling.is_null());
    *SIBLING_WEAK.lock().unwrap() = sibling as usize;

    // Phase 6 clears every weakref to a dead object before any callback
    // runs (CPython's two passes): the sibling must read back dead, not
    // hand its referent out for the sweep to free under the caller.
    assert_eq!(gc::collect(), 1, "only the target frees");
    *SIBLING_WEAK.lock().unwrap() = 0;
    assert_eq!(
        *SIBLING_SEEN.lock().unwrap(),
        (0, true),
        "the sibling was cleared before its elder's callback ran"
    );
}

#[test]
fn an_unreferenced_weakref_fires_and_dies_with_its_callback() {
    let _serial = init();
    CALLBACK_LOG.lock().unwrap().clear();
    RESURRECT_NODE.store(false, Ordering::Relaxed);
    let target = new_node();
    unpin(target);
    let callback = new_callback();
    unpin(callback);
    // SAFETY: the target supports weakrefs and the callback is callable.
    let weak = unsafe { weakref::PyWeakref_NewRef(target, callback) };
    assert!(!weak.is_null());
    unpin(weak);

    // Nothing references the weakref — the table's rows link, they do not
    // own — yet phase 6 still owes it a call: the pair lives exactly
    // through the callback, then frees with the target.
    assert_eq!(gc::collect(), 3, "the target, the weakref and its callback");
    assert_eq!(CALLBACK_LOG.lock().unwrap().as_slice(), [weak as usize]);
    assert!(!live(target) && !live(callback));
}

#[test]
fn counted_regime_objects_join_the_heap_walk() {
    let _serial = init();
    // A counted-regime block's object sits one granule in, behind its
    // pre-header: the heap walk must visit it there, or it is never
    // marked, swept or reported.
    let dead = python312::alloc::alloc_counted(100) as *mut PyObject;
    let held = python312::alloc::alloc_counted(100) as *mut PyObject;
    assert!(!dead.is_null() && !held.is_null());
    // One heap reference in the pre-header's first word: the pin rule of
    // section 9.3.
    // SAFETY: the pre-header is the 16 bytes before the object.
    unsafe {
        core::ptr::write_unaligned(
            (held as usize - python312::alloc::PREHEADER) as *mut isize,
            1,
        )
    };

    assert!(live(held), "the held counted object is reported live");
    assert_eq!(gc::collect(), 1, "the unheld counted object frees");
    assert!(!live(dead));
    assert!(live(held));

    #[cfg(feature = "debug-heap")]
    {
        // The poisoning spans the freed block from its base: the
        // pre-header's last 8 bytes are inside the fill, whereas a fill
        // measured from the object address would leave them zeroed and
        // run 16 bytes past the block's end instead.
        let block = dead as usize - python312::alloc::PREHEADER;
        // SAFETY: the reads stay inside the dead block, which is
        // exclusively the collector's.
        unsafe {
            assert_eq!(
                core::ptr::read((block + 8) as *const u8),
                python312::alloc::page::POISON_BYTE
            );
        }
    }
}

#[test]
fn weakref_get_ref_follows_the_c_api_contract() {
    let _serial = init();
    let target = new_node();
    // SAFETY: the target supports weak references.
    let weak = unsafe { weakref::PyWeakref_NewRef(target, core::ptr::null_mut()) };
    assert!(!weak.is_null());

    // Alive: 1 and a new strong reference, which alone keeps the referent
    // alive across a collection.
    let mut got = core::ptr::null_mut();
    // SAFETY: a weakref of this runtime and the caller's out-parameter.
    unsafe {
        assert_eq!(weakref::PyWeakref_GetRef(weak, &mut got), 1);
        assert!(core::ptr::eq(got, target));
    }
    // Drop the constructor's reference but keep the one GetRef handed
    // back: the object lives by that alone.
    // SAFETY: the header write returns the constructor's reference.
    unsafe { (*target).ob_refcnt -= 1 };
    assert_eq!(gc::collect(), 0, "the handed-back reference pins");
    assert!(live(got));

    // Dead: 0 and NULL — never a borrowed `None`.
    // SAFETY: dropping the reference the entry point handed back.
    unsafe { (*got).ob_refcnt -= 1 };
    assert_eq!(gc::collect(), 1);
    assert!(!live(target));
    let mut got = core::ptr::null_mut();
    // SAFETY: the weakref still lives by the test's own reference.
    unsafe {
        assert_eq!(weakref::PyWeakref_GetRef(weak, &mut got), 0);
        assert!(got.is_null());
    }
}

#[test]
fn module_defs_and_sys_modules_hold_objects() {
    let _serial = init();
    // A PyModuleDef whose m_copy is the module's dict stand-in. The def is
    // leaked: the registry holds it for the process, as CPython's module
    // registry does.
    let def: &'static mut PyModuleDef = Box::leak(Box::new(unsafe { core::mem::zeroed() }));
    def.m_name = c"gc_test_module".as_ptr();
    let held = int(11_111_111);
    unpin(held);
    def.m_base.m_copy = held;
    roots::register_module_def(def);
    // The sys.modules placeholder root.
    let placeholder = new_node();
    unpin(placeholder);
    roots::set_sys_modules(placeholder);

    assert_eq!(gc::collect(), 0);
    assert!(live(held), "m_copy holds its object");
    assert!(
        live(placeholder),
        "the sys.modules placeholder holds its node"
    );

    roots::set_sys_modules(core::ptr::null_mut());
    assert_eq!(gc::collect(), 1);
    assert!(!live(placeholder));
    // `held` stays: the module def stays registered, as CPython's does.
    assert!(live(held));
}

#[test]
fn atexit_registrations_are_roots() {
    let _serial = init();
    let callback = new_callback();
    let args = tuple::PyTuple_New(1);
    let value = int(44_444_444);
    // SAFETY: the tuple was built with one empty slot; `SetItem` steals
    // the constructor's reference to `value`, so the slot now owns the
    // int's count of one and the tuple's death will drop it.
    unsafe {
        assert_eq!(tuple::PyTuple_SetItem(args, 0, value), 0);
    }
    unpin(args);
    unpin(callback);
    let handle = roots::atexit_register(callback, args);

    assert_eq!(gc::collect(), 0);
    assert!(live(callback) && live(args) && live(value));

    roots::atexit_unregister(handle);
    assert_eq!(gc::collect(), 3, "the callback, its args tuple and the int");
    assert!(!live(callback) && !live(args) && !live(value));
}

#[test]
fn static_types_root_their_type_graphs() {
    let _serial = init();
    // Every readied static type is registered; the tuple type's `tp_mro`
    // tuple is reachable only through it. The MRO tuple was born pinned
    // (the allocation's reference); dropping that pin leaves the type
    // registration as its only root.
    let mro = {
        // SAFETY: a readied static type's slot.
        unsafe { tuple::PyTuple_Type.tp_mro }
    };
    assert!(!mro.is_null());
    // SAFETY: the header write is the same `unpin` simulation.
    unsafe { (*mro).ob_refcnt = 0 };
    assert_eq!(gc::collect(), 0);
    assert!(live(mro), "the registered static type holds its MRO tuple");
}

#[test]
fn registered_frames_mark_their_slots() {
    let _serial = init();
    // A synthetic code unit: a leaked buffer whose addresses the stack
    // maps name, registered like a compiled image registers its own.
    let code: &'static [u8] = Box::leak(vec![0xCC; 64].into_boxed_slice());
    let held = new_node();
    let also_held = int(3_141_592);
    unpin(held);
    unpin(also_held);

    // The frame: slot 0 at rbp-16 holds `held`, slot 1 at rbp-24 holds
    // `also_held`; the return address sits in the code buffer, the caller
    // rbp is null and ends the walk.
    let frame: &'static mut [usize; 8] = Box::leak(Box::new([0; 8]));
    frame[5] = code.as_ptr() as usize + 16; // [rbp+8]: return address
    frame[2] = held as usize; // [rbp-16]: slot 0
    frame[1] = also_held as usize; // [rbp-24]: slot 1
    let rbp = frame.as_ptr() as usize + 4 * 8;

    // The stack-map table: one entry covering both slots.
    let mut maps: Vec<u8> = Vec::new();
    maps.extend_from_slice(&0x504D_5343u32.to_le_bytes()); // 'CSMP'
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&1u64.to_le_bytes());
    maps.extend_from_slice(&((code.as_ptr() as usize + 16) as u64).to_le_bytes());
    maps.extend_from_slice(&2u32.to_le_bytes()); // two slots
    maps.extend_from_slice(&1u32.to_le_bytes()); // one bitmap word
    maps.extend_from_slice(&0b11u64.to_le_bytes()); // both live
    let maps: &'static [u8] = Box::leak(maps.into_boxed_slice());
    // SAFETY: the buffers are leaked and the table follows the format.
    unsafe {
        assert_eq!(
            stack::coil_rt_register_code(
                code.as_ptr() as *const core::ffi::c_void,
                code.as_ptr().add(code.len()) as *const core::ffi::c_void,
                maps.as_ptr() as *const core::ffi::c_void,
            ),
            0
        );
        stack::coil_rt_push_frame(rbp as *mut core::ffi::c_void);
    }

    assert_eq!(gc::collect(), 0);
    assert!(live(held) && live(also_held));

    // Without the frame, the same objects are unreachable.
    // SAFETY: the matching pop drops the thread's anchor.
    unsafe { stack::coil_rt_pop_frame() };
    assert_eq!(gc::collect(), 2);
    assert!(!live(held) && !live(also_held));
}

#[test]
fn nested_frame_anchors_cover_reentry_through_c() {
    let _serial = init();
    // Two synthetic frames of one code unit: the outer segment called
    // into C, which re-entered compiled code — the inner frame. The
    // C return address between them is uncovered, so a single-anchor
    // walk stops at it; every anchor on the chain must be walked.
    let code: &'static [u8] = Box::leak(vec![0xCC; 64].into_boxed_slice());
    let outer_held = new_node();
    let inner_held = new_node();
    unpin(outer_held);
    unpin(inner_held);

    // Each frame: slot 0 at rbp-16 holds its object, the return address
    // sits in the code buffer, and a null caller rbp ends that anchor's
    // own walk.
    let outer: &'static mut [usize; 8] = Box::leak(Box::new([0; 8]));
    outer[5] = code.as_ptr() as usize + 8; // [rbp+8]: return address
    outer[2] = outer_held as usize; // [rbp-16]: slot 0
    let inner: &'static mut [usize; 8] = Box::leak(Box::new([0; 8]));
    inner[5] = code.as_ptr() as usize + 24;
    inner[2] = inner_held as usize;
    let outer_rbp = outer.as_ptr() as usize + 4 * 8;
    let inner_rbp = inner.as_ptr() as usize + 4 * 8;

    // Two stack-map entries, ascending by ret_addr as the format requires.
    let mut maps: Vec<u8> = Vec::new();
    maps.extend_from_slice(&0x504D_5343u32.to_le_bytes()); // 'CSMP'
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&2u64.to_le_bytes());
    maps.extend_from_slice(&((code.as_ptr() as usize + 8) as u64).to_le_bytes());
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&0b1u64.to_le_bytes());
    maps.extend_from_slice(&((code.as_ptr() as usize + 24) as u64).to_le_bytes());
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&0b1u64.to_le_bytes());
    let maps: &'static [u8] = Box::leak(maps.into_boxed_slice());
    // SAFETY: the buffers are leaked and the table follows the format.
    unsafe {
        assert_eq!(
            stack::coil_rt_register_code(
                code.as_ptr() as *const core::ffi::c_void,
                code.as_ptr().add(code.len()) as *const core::ffi::c_void,
                maps.as_ptr() as *const core::ffi::c_void,
            ),
            0
        );
        stack::coil_rt_push_frame(outer_rbp as *mut core::ffi::c_void);
        stack::coil_rt_push_frame(inner_rbp as *mut core::ffi::c_void);
    }

    assert_eq!(gc::collect(), 0);
    assert!(live(outer_held) && live(inner_held));

    // The inner segment returned through C: its anchor pops and only its
    // object dies.
    // SAFETY: the matching pop of the inner push.
    unsafe { stack::coil_rt_pop_frame() };
    assert_eq!(gc::collect(), 1);
    assert!(live(outer_held));
    assert!(!live(inner_held));

    // SAFETY: the matching pop of the outer push.
    unsafe { stack::coil_rt_pop_frame() };
    assert_eq!(gc::collect(), 1);
    assert!(!live(outer_held));
}

#[test]
fn get_objects_and_get_referents_report_the_heap() {
    let _serial = init();
    let a = int(1_000_000);
    let b = int(2_000_000);
    let pair = tuple::PyTuple_New(2);
    // SAFETY: the tuple was built with two empty slots; `SetItem` steals
    // the constructor's reference to each item, so the slots own the
    // counts of one.
    unsafe {
        assert_eq!(tuple::PyTuple_SetItem(pair, 0, a), 0);
        assert_eq!(tuple::PyTuple_SetItem(pair, 1, b), 0);
    }

    let objects = gc::get_objects();
    // SAFETY: the tuple of live objects.
    assert!(unsafe { tuple::PyTuple_Size(objects) } >= 3);
    assert!(live(pair) && live(a) && live(b));

    // SAFETY: `pair` is a tuple whose traverse visits its items.
    let referents = unsafe { gc::get_referents(pair) };
    // SAFETY: the fresh referents tuple.
    unsafe {
        assert_eq!(tuple::PyTuple_Size(referents), 2);
        assert!(core::ptr::eq(tuple::PyTuple_GetItem(referents, 0), a));
        assert!(core::ptr::eq(tuple::PyTuple_GetItem(referents, 1), b));
        // Drop both tuples' references before the collection counts.
        (*objects).ob_refcnt = 0;
        (*referents).ob_refcnt = 0;
    }
    unpin(pair);
    assert_eq!(
        gc::collect(),
        5,
        "the pair, its items and both probes' tuples"
    );
}

#[test]
fn collect_runs_explicitly_when_pacing_is_disabled() {
    let _serial = init();
    assert!(!gc::is_enabled(), "the suite runs with pacing off");
    let object = new_node();
    unpin(object);
    gc::enable();
    assert!(gc::is_enabled());
    gc::disable();
    assert!(!gc::is_enabled());
    // An explicit collection still runs with pacing disabled.
    assert_eq!(gc::collect(), 1);
    assert!(!live(object));
}

#[test]
fn pacing_starts_a_collection_past_the_threshold() {
    let _serial = init();
    // A baseline collection zeroes the growth accounting: the loop's
    // bytes are then the whole of what pacing sees.
    gc::collect();
    gc::enable();
    let before = gc::pacing::collections();
    // Allocate past max(8 MiB, live): dead 1 KiB objects, each born
    // unpinned, freed by the collection pacing starts.
    let mut allocated = 0usize;
    while gc::pacing::collections() == before {
        // SAFETY: raw object allocation; nothing retained.
        let block = python312::alloc::alloc_object(1024);
        assert!(!block.is_null());
        allocated += 1024;
        assert!(allocated < 64 * 1024 * 1024, "pacing never collected");
    }
    gc::disable();
    assert!(
        allocated > gc::pacing::MIN_THRESHOLD as usize,
        "the collection waited for the threshold: {allocated} bytes"
    );
    assert_eq!(gc::pacing::collections(), before + 1);
    // The in-flight block the trigger's collection could not see.
    gc::collect();
}

#[test]
fn pacing_decisions_follow_the_spec() {
    let _serial = init();
    // Go's GOGC=100 rule with the 8 MiB floor.
    assert!(!gc::pacing::threshold_exceeded(8 * 1024 * 1024, 0));
    assert!(gc::pacing::threshold_exceeded(8 * 1024 * 1024 + 1, 0));
    assert!(!gc::pacing::threshold_exceeded(
        16 * 1024 * 1024,
        16 * 1024 * 1024
    ));
    assert!(gc::pacing::threshold_exceeded(
        16 * 1024 * 1024 + 1,
        16 * 1024 * 1024
    ));
    // The private-bytes backstop: growth beyond the live heap.
    assert!(!gc::pacing::backstop_exceeded(1024, 4096));
    assert!(gc::pacing::backstop_exceeded(4097, 4096));
    // The sampler reports a value on the hosts this suite runs on: the
    // CI legs' Linux and Windows. macOS, the development host, has
    // neither counter source and reports none.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    assert!(gc::pacing::private_bytes().is_some());
}

#[test]
fn threads_allocate_and_collect_concurrently() {
    let _serial = init();
    let before = gc::pacing::collections();
    let threads: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                for round in 0..200 {
                    // SAFETY: the GIL serialises attached Python work; the
                    // objects are this thread's own.
                    unsafe {
                        let state = python312::gil::PyGILState_Ensure();
                        // A list cycle per round.
                        let list_object = list::PyList_New(2);
                        let inner = list::PyList_New(0);
                        assert_eq!(list::PyList_SetItem(list_object, 0, inner), 0);
                        assert_eq!(list::PyList_SetItem(list_object, 1, list_object), 0);
                        // `SetItem` steals: the outer list's slots own
                        // `inner`'s and its own counts of one now.
                        unpin(list_object);
                        if round % 25 == 0 {
                            gc::collect();
                        }
                        python312::gil::PyGILState_Release(state);
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("allocator thread panicked");
    }
    assert!(gc::pacing::collections() > before);
    // The heap settles: everything the threads left is gone.
    assert!(gc::collect() > 0);
}

#[cfg(feature = "debug-heap")]
#[test]
fn freed_blocks_are_poisoned() {
    let _serial = init();
    let object = new_node();
    unpin(object);
    // SAFETY: the object is the test's own.
    let capacity = unsafe { python312::alloc::block_capacity(object as *const u8) }.unwrap();
    assert!(capacity > 8);
    assert!(gc::collect() >= 1);
    // The freed body carries the poison byte; the first word is the
    // allocator's free-list link.
    // SAFETY: the block is freed and exclusively the test's.
    let body =
        unsafe { core::slice::from_raw_parts((object as usize + 8) as *const u8, capacity - 8) };
    assert!(
        body.iter()
            .all(|byte| *byte == python312::alloc::page::POISON_BYTE),
        "the freed block's body is poisoned"
    );
}

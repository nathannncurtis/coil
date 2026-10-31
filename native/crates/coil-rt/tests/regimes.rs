//! The two regimes of spec section 9.3, asserted with synthetic extension
//! types whose `tp_dealloc` and `tp_finalize` count their runs: every row
//! of the release-timing table, the store barrier entry points, the
//! zero-count table's three processing triggers, the counted-container
//! and escape rules for C-created objects, the foreign side map, and the
//! `--features debug-heap` checks — poisoning, quarantine, both underflow
//! detections, the heap-walk barrier verification and the attachment
//! assertions at the API entry points.

use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};

use coil_capi_sys::{PyObject, PyTypeObject, visitproc};
use python312::abi_generated;
use python312::gc::{self, counted, stack};
use python312::gil;
use python312::object::{singletons, typeobject};
use python312::types::{dict, long, tuple};

static INIT: Once = Once::new();

/// The tests mutate process-global collector state — the type registry,
/// the ZCT, the registered code ranges — so they serialize, like the gc
/// tests do.
static SERIAL: Mutex<()> = Mutex::new(());

/// What `init` hands out: the serialization hold plus the thread's
/// `PyGILState_Ensure` reference, given back when the test ends. The
/// `--features debug-heap` entry-point assertions make attachment part
/// of every test's setup.
struct Serial {
    _lock: MutexGuard<'static, ()>,
    gil: core::ffi::c_int,
}

impl Drop for Serial {
    fn drop(&mut self) {
        // The state is this thread's own, attached by `init`.
        gil::PyGILState_Release(self.gil);
    }
}

fn init() -> Serial {
    INIT.call_once(|| {
        assert_eq!(typeobject::init_static_types(), 0);
        // Explicit collections only: the ZCT's own triggers have their
        // tests, which run with the pacing switch off.
        gc::disable();
    });
    Serial {
        _lock: SERIAL.lock().unwrap(),
        gil: gil::PyGILState_Ensure(),
    }
}

/// The process-global setup without the attach: the attachment fault's
/// child must stay detached for the fault under test.
#[cfg(feature = "debug-heap")]
fn init_detached() {
    INIT.call_once(|| {
        assert_eq!(typeobject::init_static_types(), 0);
        gc::disable();
    });
}

/// Whether this run is a fault test's child, and which fault it plays.
#[cfg(feature = "debug-heap")]
fn fault_child_name() -> Option<String> {
    std::env::var("COIL_REGIMES_FAULT").ok()
}

/// Run `test`'s child that performs `fault` and hand back its output.
/// The faults abort rather than unwind — the panic crosses the
/// `extern "C"` boundary of the entry point under test — so the parent
/// asserts the abort and its message. The child's harness captures the
/// check's panic message into the test's stdout while the abort notice
/// goes to stderr, so both streams are returned.
#[cfg(feature = "debug-heap")]
fn fault_child(test: &str, fault: &str) -> String {
    let exe = std::env::current_exe().expect("the test binary's path");
    let output = std::process::Command::new(exe)
        .args(["--exact", test, "--test-threads=1"])
        .env("COIL_REGIMES_FAULT", fault)
        .output()
        .expect("spawn the fault child");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.code().is_none(),
        "the fault must abort the child, not return: {stdout}{stderr}"
    );
    format!("{stdout}{stderr}")
}

// -------------------------------------------------- the tests' object types

/// Slot offsets of the synthetic extension object: two reference slots
/// after the header, standing in for an extension struct's members.
const SLOT0: usize = 16;
const SLOT1: usize = 24;

/// Offset of a var-object's inline `ob_item` array (a tuple's) and of a
/// list's `ob_item` pointer: the header plus `ob_size`.
const VAR_ITEMS: usize = 24;

/// How many times the synthetic `tp_dealloc` ran.
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
/// How many times the synthetic `tp_finalize` ran.
static FINALIZES: AtomicUsize = AtomicUsize::new(0);
/// The finalize/dealloc order one object produced.
static EVENTS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn slot(object: *mut PyObject, offset: usize) -> *mut PyObject {
    // SAFETY: the object's slots are within its allocation and written
    // by the tests and the type's callbacks alone.
    unsafe { core::ptr::read_unaligned((object as *const u8).add(offset) as *const *mut PyObject) }
}

fn set_slot(object: *mut PyObject, offset: usize, value: *mut PyObject) {
    // SAFETY: as `slot`, and the object is not shared across threads.
    unsafe {
        core::ptr::write_unaligned((object as *mut u8).add(offset) as *mut *mut PyObject, value)
    }
}

/// The extension's dealloc: count the run, drop the members the way a
/// coil-aware extension must inside a sweep (a collection may already
/// have destroyed them), and free the block.
unsafe extern "C" fn ext_dealloc(self_: *mut PyObject) {
    DEALLOCS.fetch_add(1, Ordering::Relaxed);
    EVENTS.lock().unwrap().push("dealloc");
    for offset in [SLOT0, SLOT1] {
        let member = slot(self_, offset);
        if !member.is_null() {
            set_slot(self_, offset, core::ptr::null_mut());
            // SAFETY: the member is a counted reference this object
            // owns; `_Py_DecRef` itself steps over a member this
            // collection has already destroyed.
            unsafe { counted::_Py_DecRef(member) };
        }
    }
    // SAFETY: the object's block is this allocator's (a foreign object's
    // block answers no page table, and the free is a no-op there).
    unsafe { python312::alloc::free_block(self_.cast()) };
}

unsafe extern "C" fn ext_traverse(
    object: *mut PyObject,
    visit: visitproc,
    arg: *mut core::ffi::c_void,
) -> core::ffi::c_int {
    for offset in [SLOT0, SLOT1] {
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

/// The extension's clear, `Py_CLEAR` on both members: null the slot
/// first, so a member's inline dealloc reentering this object finds the
/// slot already empty.
unsafe extern "C" fn ext_clear(self_: *mut PyObject) -> core::ffi::c_int {
    for offset in [SLOT0, SLOT1] {
        let member = slot(self_, offset);
        if !member.is_null() {
            set_slot(self_, offset, core::ptr::null_mut());
            // SAFETY: the member is a counted reference this object
            // owns; `_Py_DecRef` itself steps over a member this
            // collection has already destroyed.
            unsafe { counted::_Py_DecRef(member) };
        }
    }
    0
}

unsafe extern "C" fn ext_finalize(self_: *mut PyObject) {
    FINALIZES.fetch_add(1, Ordering::Relaxed);
    EVENTS.lock().unwrap().push("finalize");
    // The object stays dead: a finalizer that revived it would turn this
    // into a resurrection test, which the gc suite covers.
    let _ = self_;
}

/// A counted-regime type: `tp_dealloc` is this file's own function, not
/// the runtime's, which is the whole of the regime rule.
const fn ext_type(name: *const core::ffi::c_char, finalize: bool) -> PyTypeObject {
    let mut t = typeobject::zero_type();
    t.tp_name = name;
    t.tp_basicsize = 48;
    t.tp_flags = coil_capi_sys::Py_TPFLAGS_DEFAULT | coil_capi_sys::Py_TPFLAGS_HAVE_GC;
    t.tp_dealloc = Some(ext_dealloc);
    t.tp_traverse = Some(ext_traverse);
    t.tp_clear = Some(ext_clear);
    if finalize {
        t.tp_finalize = Some(ext_finalize);
    }
    t
}

static mut EXT_TYPE: PyTypeObject = ext_type(c"regimes_ext".as_ptr(), false);
static mut EXT_FINAL_TYPE: PyTypeObject = ext_type(c"regimes_ext_final".as_ptr(), true);

fn new_ext() -> *mut PyObject {
    // SAFETY: the type is readied and its instances are freed by the
    // dealloc above.
    unsafe { counted::_PyObject_New(&raw mut EXT_TYPE) }
}

fn new_final_ext() -> *mut PyObject {
    // SAFETY: as `new_ext`.
    unsafe { counted::_PyObject_New(&raw mut EXT_FINAL_TYPE) }
}

fn deallocs() -> usize {
    DEALLOCS.load(Ordering::Relaxed)
}

fn finalizes() -> usize {
    FINALIZES.load(Ordering::Relaxed)
}

/// Hold `value` in a synthetic compiled frame's slot 0, the way the gc
/// suite's stack-map test does, and return the frame pointer the test
/// passes to `clear_frame`. The frame stays reachable through the thread
/// state until cleared.
fn hold_in_frame(value: *mut PyObject) -> usize {
    let code: &'static [u8] = Box::leak(vec![0xCC; 64].into_boxed_slice());
    let frame: &'static mut [usize; 8] = Box::leak(Box::new([0; 8]));
    frame[5] = code.as_ptr() as usize + 16; // [rbp+8]: return address
    frame[2] = value as usize; // [rbp-16]: slot 0
    let rbp = frame.as_ptr() as usize + 4 * 8;

    let mut maps: Vec<u8> = Vec::new();
    maps.extend_from_slice(&0x504D_5343u32.to_le_bytes()); // 'CSMP'
    maps.extend_from_slice(&1u32.to_le_bytes());
    maps.extend_from_slice(&1u64.to_le_bytes());
    maps.extend_from_slice(&((code.as_ptr() as usize + 16) as u64).to_le_bytes());
    maps.extend_from_slice(&1u32.to_le_bytes()); // one slot
    maps.extend_from_slice(&1u32.to_le_bytes()); // one bitmap word
    maps.extend_from_slice(&0b1u64.to_le_bytes()); // slot 0 live
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
    rbp
}

fn clear_frame(rbp: usize) {
    // SAFETY: the matching pop drops the thread's anchor.
    unsafe { stack::coil_rt_pop_frame() };
    let _ = rbp;
}

/// A heap int for the value side of container stores.
fn int(value: i64) -> *mut PyObject {
    long::PyLong_FromLongLong(value)
}

// ------------------------------------------------------- the timing table

/// Row 1: C drops its last reference, compiled code never saw the
/// object — `tp_dealloc` runs immediately, inside `_Py_Dealloc`.
#[test]
fn last_c_decref_frees_immediately() {
    let _serial = init();
    let object = new_ext();
    let before = deallocs();
    // SAFETY: the constructor's reference is the only one.
    unsafe { counted::_Py_DecRef(object) };
    assert_eq!(deallocs(), before + 1);
    // And no ZCT processing finds anything after the fact.
    counted::process_zct();
    assert_eq!(deallocs(), before + 1);
}

/// Row 2: C drops its last reference, compiled code holds the object
/// only in frame slots — the first ZCT processing that no longer sees
/// the frame frees it.
#[test]
fn frame_slots_defer_to_the_zct() {
    let _serial = init();
    let object = new_ext();
    let before = deallocs();
    // The pointer crossing into compiled code: the boundary machinery
    // the compiler emits will call this at every C-to-compiled handoff.
    counted::note_escape(object);
    let rbp = hold_in_frame(object);
    // SAFETY: the constructor's reference is the only counted one.
    unsafe { counted::_Py_DecRef(object) };
    assert_eq!(deallocs(), before);
    // The frame holds it: processing keeps the entry.
    counted::process_zct();
    assert_eq!(deallocs(), before);
    // The frame returned: the next processing frees it.
    clear_frame(rbp);
    counted::process_zct();
    assert_eq!(deallocs(), before + 1);
}

/// Row 3, the overwrite half: the last heap holder overwrites its
/// reference — the barrier decrements `heap_refs` to zero, the object
/// joins the ZCT, the next processing frees it.
#[test]
fn overwrite_of_last_heap_holder_frees_at_next_zct() {
    let _serial = init();
    let object = new_ext();
    let holder = dict::PyDict_New();
    // SAFETY: both objects are freshly created.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(holder, singletons::none(), object), 0);
    }
    assert_eq!(unsafe { counted::heap_refs_of(object) }, 1);
    let before = deallocs();
    // SAFETY: C drops its reference; the dict's heap reference remains.
    unsafe { counted::_Py_DecRef(object) };
    assert_eq!(deallocs(), before);
    counted::process_zct();
    assert_eq!(deallocs(), before);
    // The overwrite: the barrier reaches zero with `ob_refcnt == 0` and
    // enqueues; the free waits for the processing.
    // SAFETY: the dict is alive and the key present.
    unsafe {
        assert_eq!(
            dict::PyDict_SetItem(holder, singletons::none(), singletons::none()),
            0
        );
    }
    assert_eq!(deallocs(), before);
    counted::process_zct();
    assert_eq!(deallocs(), before + 1);
    // SAFETY: the holder's own reference.
    unsafe { counted::_Py_DecRef(holder) };
}

/// Row 3, the collected half: the last heap holder is itself collected —
/// the sweep's barrier decrement frees the R object in the same
/// collection.
#[test]
fn collected_last_heap_holder_frees_in_the_same_sweep() {
    let _serial = init();
    let object = new_ext();
    let holder = dict::PyDict_New();
    // SAFETY: both objects are freshly created.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(holder, singletons::none(), object), 0);
    }
    // SAFETY: C drops its reference; the heap reference remains.
    unsafe { counted::_Py_DecRef(object) };
    // The holder escaped into compiled code, so its own death belongs to
    // the collector.
    counted::note_escape(holder);
    // SAFETY: the constructor's reference.
    unsafe { counted::_Py_DecRef(holder) };
    let before = deallocs();
    // The dict dies at the collection; its barrier decrement brings the
    // extension object to zero, and the collection frees both.
    assert_eq!(gc::collect(), 2);
    assert_eq!(deallocs(), before + 1);
}

/// Row 4: compiled code drops its references first, C still holds some —
/// C's last `Py_DECREF` reaching zero frees the object immediately.
#[test]
fn compiled_drop_first_c_decref_frees_immediately() {
    let _serial = init();
    let object = new_ext();
    let holder = dict::PyDict_New();
    // SAFETY: both objects are freshly created.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(holder, singletons::none(), object), 0);
    }
    assert_eq!(unsafe { counted::heap_refs_of(object) }, 1);
    let before = deallocs();
    // Compiled code's store goes away first: `heap_refs` drops, but C's
    // reference keeps the object alive — no ZCT entry, no processing.
    // SAFETY: the dict is alive and the key present.
    unsafe {
        assert_eq!(
            dict::PyDict_SetItem(holder, singletons::none(), singletons::none()),
            0
        );
    }
    assert_eq!(unsafe { counted::heap_refs_of(object) }, 0);
    counted::process_zct();
    assert_eq!(deallocs(), before);
    // C's last reference: immediate, as in CPython.
    // SAFETY: the reference the constructor handed out.
    unsafe { counted::_Py_DecRef(object) };
    assert_eq!(deallocs(), before + 1);
    // SAFETY: the holder's own reference.
    unsafe { counted::_Py_DecRef(holder) };
}

/// Row 5: reachable from both C and compiled code — freed exactly once,
/// by whichever contribution goes last.
#[test]
fn both_sides_hold_until_both_drop() {
    let _serial = init();
    let object = new_ext();
    let holder = dict::PyDict_New();
    // SAFETY: both objects are freshly created.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(holder, singletons::none(), object), 0);
    }
    let before = deallocs();
    // C's side goes first: the heap reference holds the object.
    // SAFETY: the constructor's reference.
    unsafe { counted::_Py_DecRef(object) };
    counted::process_zct();
    assert_eq!(deallocs(), before);
    // The heap side goes second: the barrier enqueues at zero.
    // SAFETY: the dict is alive and the key present.
    unsafe {
        assert_eq!(
            dict::PyDict_SetItem(holder, singletons::none(), singletons::none()),
            0
        );
    }
    counted::process_zct();
    assert_eq!(deallocs(), before + 1);
    counted::process_zct();
    assert_eq!(deallocs(), before + 1, "the dealloc must run exactly once");
    // SAFETY: the holder's own reference.
    unsafe { counted::_Py_DecRef(holder) };
}

/// Row 6, the tracing-regime side of the escape rule: a C-created T
/// object read out of a counted container escaped, so its refcount death
/// belongs to the collector, not to `_Py_Dealloc`.
#[test]
fn escaped_c_object_waits_for_the_collection() {
    let _serial = init();
    // SAFETY: the list and the int are freshly created; `SetItem`
    // steals the reference the constructors handed out.
    unsafe {
        let list_object = python312::types::list::PyList_New(1);
        let value = int(1_000_000);
        assert_eq!(
            python312::types::list::PyList_SetItem(list_object, 0, value),
            0
        );
        // The borrowed pointer crossing into compiled code escapes it.
        assert_eq!(
            python312::types::list::PyList_GetItem(list_object, 0),
            value
        );
        counted::_Py_DecRef(list_object);
    }
    // The list was never seen by compiled code: it and its slot dropped
    // together, but the escaped int is the collector's to free.
    assert_eq!(gc::collect(), 1);

    // The contrast: an int compiled code never read dies with its
    // container, at the refcount.
    // SAFETY: as above, without the read.
    unsafe {
        let list_object = python312::types::list::PyList_New(1);
        let value = int(1_000_001);
        assert_eq!(
            python312::types::list::PyList_SetItem(list_object, 0, value),
            0
        );
        counted::_Py_DecRef(list_object);
    }
    assert_eq!(gc::collect(), 0);
}

// ------------------------------------------------------ counted containers

/// The counted-container rule of 9.3: a C-created tuple's slots hold
/// counted references — `PyTuple_SET_ITEM`'s inline store is a raw slot
/// write that steals, no barrier, no `heap_refs` — so the item's release
/// follows the rows above: at once with an unescaped container, through
/// the ZCT for an escaped one.
#[test]
fn counted_tuple_slots_hold_counted_references() {
    let _serial = init();
    let tuple_object = tuple::PyTuple_New(2);
    assert!(!tuple_object.is_null());
    let immediate = new_ext();
    let deferred = new_ext();
    counted::note_escape(deferred);
    // The macro's expansion, as the extension's C compiles it: two raw
    // slot writes that steal the constructor references.
    set_slot(tuple_object, VAR_ITEMS, immediate); // ob_item[0]
    set_slot(tuple_object, VAR_ITEMS + 8, deferred); // ob_item[1]
    // The slots are counted references, not heap references.
    assert_eq!(unsafe { counted::heap_refs_of(immediate) }, 0);
    assert_eq!(unsafe { counted::heap_refs_of(deferred) }, 0);
    let before = deallocs();
    // The tuple never escaped into compiled code: C's last reference
    // tears it down at once, and the unescaped item dies with it, as in
    // CPython.
    // SAFETY: the constructor's reference to the tuple.
    unsafe { counted::_Py_DecRef(tuple_object) };
    assert_eq!(deallocs(), before + 1);
    // The escaped item's release belongs to the next ZCT processing.
    counted::process_zct();
    assert_eq!(deallocs(), before + 2);
}

/// The collected-holder row through a counted container: the tuple
/// escaped, so the collector owns its death; the subtraction counts the
/// slot's counted reference as internal, and the sweep frees item and
/// holder in one collection.
#[test]
fn collected_counted_tuple_frees_its_item() {
    let _serial = init();
    let tuple_object = tuple::PyTuple_New(1);
    let object = new_ext();
    set_slot(tuple_object, VAR_ITEMS, object);
    counted::note_escape(tuple_object);
    let before = deallocs();
    // SAFETY: the constructors' references; the item's is the slot's
    // now, the holder's death belongs to the collector.
    unsafe { counted::_Py_DecRef(tuple_object) };
    assert_eq!(deallocs(), before);
    assert_eq!(gc::collect(), 2);
    assert_eq!(deallocs(), before + 1);
}

/// The list side of the counted-container rule: `PyList_SET_ITEM`'s
/// inline store writes through the item array, `PyList_Append` on a
/// counted list takes the slow path and increfs (spec 9.3), and the
/// container's death drops what its slots hold.
#[test]
fn counted_list_slots_hold_counted_references() {
    let _serial = init();
    let built = python312::types::list::PyList_New(1);
    let object = new_ext();
    // The macro's expansion: the write lands in the item array the list
    // owns, stealing the constructor's reference.
    let items = slot(built, VAR_ITEMS);
    set_slot(items, 0, object);
    assert_eq!(unsafe { counted::heap_refs_of(object) }, 0);
    // A read borrows; for an R item nothing changes.
    // SAFETY: the list holds the item.
    unsafe {
        assert_eq!(python312::types::list::PyList_GetItem(built, 0), object);
    }
    let appended = python312::types::list::PyList_New(0);
    // SAFETY: the list is fresh; CPython's append takes a reference.
    unsafe {
        assert_eq!(python312::types::list::PyList_Append(appended, object), 0);
    }
    assert_eq!(unsafe { (*object).ob_refcnt }, 2);
    let before = deallocs();
    // SAFETY: the constructors' references to the containers; each
    // death drops its slot's counted reference.
    unsafe {
        counted::_Py_DecRef(built);
        counted::_Py_DecRef(appended);
    }
    assert_eq!(
        deallocs(),
        before + 1,
        "the second container's drop frees it"
    );
}

/// Row 7, R↔R: a cycle through C objects — the full collection's
/// subtraction finds no external holder, `tp_clear` breaks the cycle,
/// `tp_dealloc` runs once per object.
#[test]
fn rr_cycle_freed_by_the_collection() {
    let _serial = init();
    let a = new_ext();
    let b = new_ext();
    // SAFETY: both objects are alive with their constructor's
    // reference; the cycle is two counted references.
    unsafe {
        set_slot(a, SLOT0, b);
        counted::_Py_IncRef(b);
        set_slot(b, SLOT0, a);
        counted::_Py_IncRef(a);
        counted::_Py_DecRef(a);
        counted::_Py_DecRef(b);
    }
    let before = deallocs();
    assert_eq!(gc::collect(), 2);
    assert_eq!(deallocs(), before + 2);
    assert_eq!(gc::collect(), 0);
    assert_eq!(deallocs(), before + 2, "each dealloc exactly once");
}

/// Row 7, R↔T: an extension object and a compiled-code object keep each
/// other alive — the collection's subtraction unties it, the dead T
/// object's dealloc drops its `heap_refs`, and both free in one
/// collection.
#[test]
fn rt_cycle_freed_by_the_collection() {
    let _serial = init();
    let object = new_ext();
    let holder = dict::PyDict_New();
    // SAFETY: both objects are freshly created; the dict gains a heap
    // reference to the extension object, the extension object a counted
    // reference to the dict.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(holder, singletons::none(), object), 0);
        set_slot(object, SLOT0, holder);
        counted::_Py_IncRef(holder);
        // C drops both of its references.
        counted::_Py_DecRef(object);
        counted::_Py_DecRef(holder);
    }
    let before = deallocs();
    assert_eq!(gc::collect(), 2);
    assert_eq!(deallocs(), before + 1);
    assert_eq!(gc::collect(), 0);
}

/// Row 8, the refcount path: an R object with a finalizer —
/// `tp_finalize` then `tp_dealloc`, CPython's `subtype_dealloc` order,
/// each exactly once.
#[test]
fn finalizer_runs_before_dealloc_on_the_zct_path() {
    let _serial = init();
    EVENTS.lock().unwrap().clear();
    let object = new_final_ext();
    let before_deallocs = deallocs();
    let before_finalizes = finalizes();
    counted::note_escape(object);
    // SAFETY: the constructor's reference is the only one; the object
    // defers to the ZCT, whose processing runs the finalizer first.
    unsafe { counted::_Py_DecRef(object) };
    assert_eq!(before_finalizes, finalizes());
    counted::process_zct();
    assert_eq!(finalizes(), before_finalizes + 1);
    assert_eq!(deallocs(), before_deallocs + 1);
    assert_eq!(*EVENTS.lock().unwrap(), ["finalize", "dealloc"]);
    counted::process_zct();
    assert_eq!(finalizes(), before_finalizes + 1, "once, ever");
}

/// Row 8, the cycle path: the collection finalises the dead finalizable
/// R objects (PEP 442) and deallocates them once each; the pending
/// finalizer does not run twice on the dealloc path.
#[test]
fn finalizer_cycle_runs_once_by_the_collection() {
    let _serial = init();
    EVENTS.lock().unwrap().clear();
    let a = new_final_ext();
    let b = new_final_ext();
    // SAFETY: as the R↔R cycle.
    unsafe {
        set_slot(a, SLOT0, b);
        counted::_Py_IncRef(b);
        set_slot(b, SLOT0, a);
        counted::_Py_IncRef(a);
        counted::_Py_DecRef(a);
        counted::_Py_DecRef(b);
    }
    let before_deallocs = deallocs();
    let before_finalizes = finalizes();
    assert_eq!(gc::collect(), 2);
    assert_eq!(finalizes(), before_finalizes + 2);
    assert_eq!(deallocs(), before_deallocs + 2);
    // PEP 442's order at collection scale: every finalizer runs before
    // any dealloc of the same cycle.
    let events = EVENTS.lock().unwrap().clone();
    assert_eq!(&events[..2], &["finalize", "finalize"]);
    assert_eq!(&events[2..], &["dealloc", "dealloc"]);
    assert_eq!(gc::collect(), 0);
    assert_eq!(finalizes(), before_finalizes + 2, "once, ever");
}

// ------------------------------------------------------- barrier and ZCT

/// The abi.toml entry points: the ext barrier adjusts unconditionally,
/// the dynamic barrier after the regime bit, and both are in the
/// published table.
#[test]
fn barrier_entry_points_adjust_heap_refs() {
    let _serial = init();
    let names: Vec<&str> = abi_generated::ABI_FUNCTIONS
        .iter()
        .map(|(name, _, _)| *name)
        .collect();
    assert!(names.contains(&"coil_rt_barrier_ext"));
    assert!(names.contains(&"coil_rt_barrier_dynamic"));

    let object = new_ext();
    let before = deallocs();
    // SAFETY: the object is alive with its constructor's reference.
    unsafe {
        // The ext barrier adjusts unconditionally.
        counted::coil_rt_barrier_ext(core::ptr::null_mut(), object);
        assert_eq!(counted::heap_refs_of(object), 1);
        // The dynamic barrier adjusts after the regime bit.
        counted::coil_rt_barrier_dynamic(core::ptr::null_mut(), object);
        assert_eq!(counted::heap_refs_of(object), 2);
        counted::coil_rt_barrier_dynamic(object, core::ptr::null_mut());
        assert_eq!(counted::heap_refs_of(object), 1);
        // A T value through either barrier changes nothing.
        counted::coil_rt_barrier_dynamic(core::ptr::null_mut(), singletons::none());
        counted::coil_rt_barrier_ext(core::ptr::null_mut(), singletons::none());
        assert_eq!(counted::heap_refs_of(object), 1);
        // C drops its reference while the heap reference holds the
        // object: the ZCT defers, the barrier's decrement frees.
        counted::_Py_DecRef(object);
    }
    assert_eq!(deallocs(), before);
    counted::process_zct();
    assert_eq!(deallocs(), before);
    // SAFETY: the store that overwrites the last heap reference.
    unsafe { counted::coil_rt_barrier_ext(object, core::ptr::null_mut()) };
    assert_eq!(deallocs(), before);
    counted::process_zct();
    assert_eq!(deallocs(), before + 1);
}

/// The safepoint trigger: the 64th waiting entry makes the next heap
/// request process the table first.
#[test]
fn zct_safepoint_at_sixty_four_entries() {
    let _serial = init();
    let before = deallocs();
    // SAFETY: each object escapes and loses its last counted reference,
    // which enqueues it.
    unsafe {
        for _ in 0..63 {
            let object = new_ext();
            counted::note_escape(object);
            counted::_Py_DecRef(object);
        }
    }
    // 63 entries: an allocation is not a safepoint yet.
    let probe = python312::alloc::alloc_object(16);
    assert!(!probe.is_null());
    assert_eq!(deallocs(), before);
    // SAFETY: the 64th entry tips the table over.
    unsafe {
        let object = new_ext();
        counted::note_escape(object);
        counted::_Py_DecRef(object);
    }
    let trip = python312::alloc::alloc_object(16);
    assert!(!trip.is_null());
    assert_eq!(deallocs(), before + 64);
    // SAFETY: the raw probe blocks would otherwise sit dead for the
    // next collection to free.
    unsafe {
        assert!(python312::alloc::free_block(probe));
        assert!(python312::alloc::free_block(trip));
    }
}

/// The eval-breaker tick: a thread attaching processes the waiting
/// table, whatever thread enqueued it.
#[test]
fn zct_tick_on_thread_attach() {
    let serial = init();
    let before = deallocs();
    // SAFETY: three objects escape and lose their counted references.
    unsafe {
        for _ in 0..3 {
            let object = new_ext();
            counted::note_escape(object);
            counted::_Py_DecRef(object);
        }
    }
    assert_eq!(deallocs(), before);
    // Give the GIL up for the worker's attach: joining while holding it
    // would leave both threads waiting for each other. The state is this
    // thread's own, attached by `init`.
    gil::PyGILState_Release(serial.gil);
    let worker = std::thread::spawn(|| {
        // The GIL attaches the fresh thread; the attach's tick is the
        // trigger under test.
        let state = gil::PyGILState_Ensure();
        gil::PyGILState_Release(state);
    });
    worker.join().unwrap();
    gil::PyGILState_Ensure();
    assert_eq!(deallocs(), before + 3);
}

/// Foreign R objects: `PyObject_Init` on extension-owned memory — the
/// side hash map carries the counts, the ZCT still runs `tp_dealloc`,
/// and the collector never frees the memory.
#[test]
fn foreign_r_object_uses_the_side_map() {
    let _serial = init();
    let block = python312::alloc::pymem::plain_malloc(64);
    assert!(!block.is_null());
    let object = block.cast::<PyObject>();
    let before = deallocs();
    // SAFETY: the block addresses a fresh 64-byte, 16-aligned buffer.
    unsafe {
        assert_eq!(counted::PyObject_Init(object, &raw mut EXT_TYPE), object);
        assert_eq!(counted::heap_refs_of(object), 0);
        counted::coil_rt_barrier_ext(core::ptr::null_mut(), object);
        assert_eq!(counted::heap_refs_of(object), 1);
        counted::coil_rt_barrier_dynamic(object, core::ptr::null_mut());
        assert_eq!(counted::heap_refs_of(object), 0);
        // No page table answers this address: the collection must leave
        // it alone.
        assert_eq!(gc::collect(), 0);
        assert_eq!(deallocs(), before);
        counted::note_escape(object);
        counted::_Py_DecRef(object);
    }
    assert_eq!(deallocs(), before);
    counted::process_zct();
    assert_eq!(deallocs(), before + 1);
    // The extension's memory is its own: the block is still there, and
    // the test frees it the way the extension would have.
    // SAFETY: the plain-allocator block from above.
    unsafe { python312::alloc::pymem::plain_free(block) };
}

// --------------------------------------------------------- creation and rc

/// Counted-regime objects carry their 16-byte pre-header from either
/// creation path, and the page's regime bit sits at the object; a plain
/// tracing block has neither.
#[test]
fn counted_objects_carry_a_preheader() {
    let _serial = init();
    let before = deallocs();
    // SAFETY: both creation paths hand out a live object with one
    // reference.
    let object = new_ext();
    let allocated = unsafe { typeobject::PyType_GenericAlloc(&raw mut EXT_TYPE, 0) };
    assert!(!allocated.is_null());
    // SAFETY: the pre-header probe reads page bits and the pointer
    // arithmetic stays inside the block.
    unsafe {
        assert!(python312::alloc::preheader(object.cast()).is_some());
        assert!(python312::alloc::preheader(allocated.cast()).is_some());
    }
    let page = python312::alloc::page_of(object as usize).unwrap();
    let granule = page.granule_of(page.base(), object as usize);
    assert!(page.regime(granule));
    assert!(page.has_prehdr(granule - 1));
    // A tracing block: no pre-header, no regime bit.
    let plain = python312::alloc::alloc_object(48);
    assert!(!plain.is_null());
    // SAFETY: as above, on the plain block.
    unsafe {
        assert!(python312::alloc::preheader(plain.cast::<PyObject>().cast()).is_none());
        // The raw block would otherwise sit dead for the next
        // collection to free.
        assert!(python312::alloc::free_block(plain));
    }
    // SAFETY: the two counted references.
    unsafe {
        counted::_Py_DecRef(object);
        counted::_Py_DecRef(allocated);
    }
    assert_eq!(deallocs(), before + 2);
}

/// The four reference-count entry points agree, and immortal singletons
/// ignore them all.
#[test]
fn inc_dec_ref_accounting() {
    let _serial = init();
    let object = new_ext();
    // SAFETY: the object is alive with its constructor's reference.
    unsafe {
        counted::Py_IncRef(object);
        assert_eq!((*object).ob_refcnt, 2);
        counted::_Py_IncRef(object);
        assert_eq!((*object).ob_refcnt, 3);
        counted::Py_DecRef(object);
        assert_eq!((*object).ob_refcnt, 2);
        counted::_Py_DecRef(object);
        assert_eq!((*object).ob_refcnt, 1);
        // Null and the immortals are no-ops.
        counted::Py_IncRef(core::ptr::null_mut());
        counted::Py_DecRef(core::ptr::null_mut());
        counted::Py_IncRef(singletons::none());
        counted::Py_DecRef(singletons::none());
        assert_eq!(
            (*singletons::none()).ob_refcnt,
            python312::object::header::IMMORTAL_REFCNT
        );
        let before = deallocs();
        counted::_Py_DecRef(object);
        assert_eq!(deallocs(), before + 1);
    }
}

/// `_Py_Dealloc` itself: the exported entry point routes by regime the
/// same way `_Py_DecRef` at zero does.
#[test]
fn py_dealloc_routes_by_regime() {
    let _serial = init();
    let immediate = new_ext();
    let before = deallocs();
    // SAFETY: an extension's inline decrement to zero, then the entry
    // point the `.pyd` imports; the object never escaped, so the
    // dealloc is immediate.
    unsafe {
        (*immediate).ob_refcnt = 0;
        counted::_Py_Dealloc(immediate);
    }
    assert_eq!(deallocs(), before + 1);

    let deferred = new_ext();
    counted::note_escape(deferred);
    // SAFETY: as above, on the escaped object: the ZCT defers it.
    unsafe {
        (*deferred).ob_refcnt = 0;
        counted::_Py_Dealloc(deferred);
    }
    assert_eq!(deallocs(), before + 1);
    counted::process_zct();
    assert_eq!(deallocs(), before + 2);
}

/// The `--features debug-heap` quarantine: a freed R object keeps its
/// block for one cycle — hidden from the walks, poisoned, unreusable —
/// and the next collection's start frees it for real.
#[test]
#[cfg(feature = "debug-heap")]
fn freed_counted_object_is_quarantined() {
    let _serial = init();
    let object = new_ext();
    let page = python312::alloc::page_of(object as usize).unwrap();
    let granule = page.granule_of(page.base(), object as usize);
    let block = object as usize - python312::alloc::PREHEADER;
    let block_granule = page.granule_of(page.base(), block);
    // SAFETY: the constructor's reference.
    unsafe { counted::_Py_DecRef(object) };
    // The block is kept, the object hidden from every walk.
    assert!(page.allocated(block_granule));
    assert!(!page.is_object(granule));
    assert!(!page.regime(granule));
    // And poisoned from the block's base: the pre-header's last bytes
    // sit inside the fill, whereas a fill measured from the object
    // address would leave them zeroed and run past the block's end.
    // SAFETY: the read stays inside the quarantined block, which is
    // exclusively the runtime's.
    unsafe {
        assert_eq!(
            core::ptr::read((block + 8) as *const u8),
            python312::alloc::page::POISON_BYTE
        );
    }
    // Nothing reuses the quarantined block.
    let fresh = new_ext();
    assert_ne!(fresh as usize, object as usize);
    // SAFETY: the fresh constructor reference.
    unsafe { counted::_Py_DecRef(fresh) };
    // The next collection releases the quarantine.
    gc::collect();
    assert!(!page.allocated(block_granule));
}

/// The message a caught panic carried, whatever form it was formatted
/// into.
#[cfg(feature = "debug-heap")]
fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = panic.downcast_ref::<&'static str>() {
        return (*text).to_string();
    }
    panic.downcast_ref::<String>().cloned().unwrap_or_default()
}

/// The `--features debug-heap` underflow detection on `heap_refs`: a
/// barrier decrement below zero — the pairing bug, one increment against
/// two decrements — aborts at the entry point's boundary.
#[test]
#[cfg(feature = "debug-heap")]
fn heap_refs_underflow_is_detected() {
    if let Some(fault) = fault_child_name() {
        assert_eq!(fault, "heap-refs-underflow");
        let _serial = init();
        let object = new_ext();
        // SAFETY: the object is alive; the call is the fault under
        // test, a decrement with no matching increment.
        unsafe { counted::coil_rt_barrier_ext(object, core::ptr::null_mut()) };
        unreachable!("the underflow aborts the child");
    }
    let output = fault_child("heap_refs_underflow_is_detected", "heap-refs-underflow");
    assert!(
        output.contains("heap_refs underflow"),
        "the abort names the check: {output}"
    );
}

/// The `--features debug-heap` underflow detection on `ob_refcnt`: a
/// decrement of a count already at zero — the double decrement — aborts
/// at the entry point's boundary.
#[test]
#[cfg(feature = "debug-heap")]
fn ob_refcnt_underflow_is_detected() {
    if let Some(fault) = fault_child_name() {
        assert_eq!(fault, "ob-refcnt-underflow");
        let _serial = init();
        let object = new_ext();
        // SAFETY: the header write simulates the extension's bug: one
        // reference too many already dropped; the call is the fault.
        unsafe {
            (*object).ob_refcnt = 0;
            counted::_Py_DecRef(object);
        }
        unreachable!("the underflow aborts the child");
    }
    let output = fault_child("ob_refcnt_underflow_is_detected", "ob-refcnt-underflow");
    assert!(
        output.contains("ob_refcnt underflow"),
        "the abort names the check: {output}"
    );
}

/// The `--features debug-heap` barrier verification: every collection
/// recomputes `heap_refs` from a heap walk. Real edges agree; a heap
/// reference with no edge behind it — what a store path that skipped
/// the barrier leaves — fails the recomputation.
#[test]
#[cfg(feature = "debug-heap")]
fn heap_walk_verification_catches_a_skipped_barrier() {
    let _serial = init();
    let object = new_ext();
    let holder = dict::PyDict_New();
    // SAFETY: both objects are freshly created.
    unsafe {
        assert_eq!(dict::PyDict_SetItem(holder, singletons::none(), object), 0);
    }
    assert_eq!(unsafe { counted::heap_refs_of(object) }, 1);
    // The real edge: the recomputation at this collection counts the
    // dict's reference and agrees.
    let before = deallocs();
    gc::collect();
    assert_eq!(deallocs(), before, "the held object survives");
    // The fault: one heap reference with no edge behind it.
    // SAFETY: the barrier entry point is the injector.
    unsafe { counted::coil_rt_barrier_ext(core::ptr::null_mut(), object) };
    assert_eq!(unsafe { counted::heap_refs_of(object) }, 2);
    // The collection is the fault's witness; the panic unwinds through
    // Rust frames only, so the test catches it.
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(gc::collect))
        .expect_err("the mismatch must be detected");
    assert!(
        panic_message(panic).contains("heap_refs mismatch"),
        "the panic names the check"
    );
    // Repair: drop the fake reference; the recomputation agrees again.
    // SAFETY: as the injecting call.
    unsafe { counted::coil_rt_barrier_ext(object, core::ptr::null_mut()) };
    gc::collect();
    assert_eq!(deallocs(), before);
    // Tear down: the overwrite releases the last heap reference, C's
    // last reference frees the object, the holder dies at the refcount.
    // SAFETY: the dict is alive and the key present; the object holds
    // the constructor's reference still.
    unsafe {
        assert_eq!(
            dict::PyDict_SetItem(holder, singletons::none(), singletons::none()),
            0
        );
        counted::_Py_DecRef(object);
    }
    assert_eq!(deallocs(), before + 1);
    // SAFETY: the holder's own reference.
    unsafe { counted::_Py_DecRef(holder) };
}

/// The `--features debug-heap` attachment assertions: the API entry
/// points a `.pyd` reaches must run on a thread that holds the GIL; a
/// detached caller fails before anything is touched.
#[test]
#[cfg(feature = "debug-heap")]
fn api_entries_assert_attachment() {
    const FAULTS: &[(&str, &str)] = &[
        ("decref", "_Py_DecRef"),
        ("dealloc", "_Py_Dealloc"),
        ("barrier-ext", "coil_rt_barrier_ext"),
        ("barrier-dynamic", "coil_rt_barrier_dynamic"),
    ];
    if let Some(fault) = fault_child_name() {
        let entry = FAULTS
            .iter()
            .find(|(name, _)| *name == fault)
            .expect("a named fault")
            .1;
        init_detached();
        // A stand-in object the detached calls can point at; the entry
        // points fail before they touch it.
        // SAFETY: the raw block is this child's own, and the header
        // write initialises it as its one reference.
        let object = unsafe {
            let block = python312::alloc::alloc_object(48);
            let object = block.cast::<PyObject>();
            python312::object::header::c_init(object, &raw mut EXT_TYPE);
            object
        };
        // SAFETY: the caller is deliberately detached; each entry point
        // is the fault under test.
        unsafe {
            match fault.as_str() {
                "decref" => counted::_Py_DecRef(object),
                "dealloc" => counted::_Py_Dealloc(object),
                "barrier-ext" => counted::coil_rt_barrier_ext(core::ptr::null_mut(), object),
                "barrier-dynamic" => {
                    counted::coil_rt_barrier_dynamic(core::ptr::null_mut(), object)
                }
                _ => unreachable!(),
            }
        }
        unreachable!("the {entry} entry aborts the child");
    }
    for (fault, entry) in FAULTS {
        let output = fault_child("api_entries_assert_attachment", fault);
        assert!(
            output.contains(&format!("{entry} entered without the GIL attached")),
            "the abort names {entry}: {output}"
        );
    }
}

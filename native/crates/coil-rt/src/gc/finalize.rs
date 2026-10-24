//! PEP 442 finalisation, section 9.3 phase 5 and section 9.6: every dead
//! object with `tp_finalize` and its `finalized` bitmap bit clear runs its
//! finalizer exactly once, on the collecting thread, with the exception
//! context cleared; resurrection is detected by re-marking, never inside
//! the finalizer. The finalizer queue is a root while it holds objects.

use coil_capi_sys::PyObject;

use crate::gc::mark::MarkState;
use crate::object::header;
use crate::tstate;

/// The finalizer queue: objects found dead with a pending finalizer.
/// Drained by phase 5 in the collection that filled it; declared a root
/// of section 9.2 so an aborted collection cannot lose them.
fn queue() -> &'static std::sync::Mutex<Vec<usize>> {
    static QUEUE: std::sync::OnceLock<std::sync::Mutex<Vec<usize>>> = std::sync::OnceLock::new();
    QUEUE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Mark the queued objects — the queue root of section 9.2.
pub(crate) fn mark_queue(marks: &mut MarkState) {
    for addr in queue().lock().expect("finalizer queue poisoned").iter() {
        marks.mark(*addr as *mut PyObject);
    }
}

/// Phase 5 over the dead candidates of this collection: enqueue those with
/// `tp_finalize` whose `finalized` bit is clear, run each finalizer once
/// with the pending exception saved and cleared, and write an exception a
/// finalizer leaves behind unraisably. The caller re-marks afterwards;
/// anything a finalizer stored into a root comes back to life there, and
/// everything else stays dead (PEP 442).
pub(crate) fn finalize_dead(dead: &[usize]) {
    let mut pending = queue().lock().expect("finalizer queue poisoned");
    for addr in dead {
        let object = *addr as *mut PyObject;
        // SAFETY: the header read is within a dead-but-valid object.
        let ty = unsafe { header::type_of(object) };
        if ty.is_null() {
            continue;
        }
        // SAFETY: the slot read is within the type object.
        if unsafe { (*ty).tp_finalize.is_none() } {
            continue;
        }
        let Some(page) = crate::alloc::page_of(*addr) else {
            continue;
        };
        let granule = page.granule_of(page.base(), *addr);
        if page.finalized(granule) {
            // PEP 442: at most once, ever.
            continue;
        }
        page.set_finalized(granule);
        pending.push(*addr);
    }
    // The queue's objects stay valid for the finalizers' duration: they
    // are dead but the sweep has not run, so nothing reuses them.
    for addr in pending.iter() {
        run_finalizer(*addr as *mut PyObject);
    }
    pending.clear();
}

/// One finalizer call: `tp_finalize(object)` on the collecting thread with
/// the thread's exception state cleared around it (section 9.6), an
/// exception it raises written unraisably.
fn run_finalizer(object: *mut PyObject) {
    let state = tstate::current();
    // SAFETY: the state is this thread's own, and the collection holds the
    // world stopped, so the save and restore race nothing.
    unsafe {
        let saved_exception = (*state).current_exception;
        let saved_traceback = (*state).current_traceback;
        let saved_context = (*state).context;
        (*state).current_exception = core::ptr::null_mut();
        (*state).current_traceback = core::ptr::null_mut();
        (*state).context = core::ptr::null_mut();

        // SAFETY: the header read is within the object.
        let ty = header::type_of(object);
        // SAFETY: phase 5 checked the slot exists.
        if let Some(finalize) = (*ty).tp_finalize {
            finalize(object);
        }
        if !tstate::occurred().is_null() {
            // SAFETY: the object names the finalizer's failure.
            crate::types::exc::PyErr_WriteUnraisable(object);
        }
        tstate::clear();

        (*state).current_exception = saved_exception;
        (*state).current_traceback = saved_traceback;
        (*state).context = saved_context;
    }
}

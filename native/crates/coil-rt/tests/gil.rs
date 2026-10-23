//! The GIL's v1 semantics (spec section 10.3): one attached thread, the
//! switch interval, drop requests through the eval-breaker word, and
//! `PyGILState_Ensure` for foreign threads.

use std::ptr;
use std::sync::mpsc;
use std::sync::{Mutex, Once};
use std::thread;
use std::time::{Duration, Instant};

use python312::gil::{self, PyGILState_Check, PyGILState_Ensure, PyGILState_Release};
use python312::types::exc;
use python312::{eval_breaker, tstate};

/// The GIL is one process-wide word; these tests serialize on it.
static SERIAL: Mutex<()> = Mutex::new(());
static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(python312::object::typeobject::init_static_types(), 0));
}

#[test]
fn handoff_sets_the_drop_request_bit_after_the_interval() {
    init();
    let _serial = SERIAL.lock().unwrap();
    assert_eq!(gil::switch_interval_us(), gil::DEFAULT_SWITCH_INTERVAL_US);
    assert_eq!(gil::DEFAULT_SWITCH_INTERVAL_US, 5_000);

    let holder = tstate::current();
    // SAFETY: `holder` is the current thread's own state.
    unsafe { gil::attach(holder) };
    assert!(unsafe { (*holder).attached });
    assert_eq!(PyGILState_Check(), 1);

    let (ready_tx, ready_rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let state = tstate::current();
        ready_tx.send(()).expect("holder is listening");
        let started = Instant::now();
        // SAFETY: `state` is this thread's own state.
        unsafe {
            gil::attach(state);
            assert!((*state).attached);
            gil::detach(state);
        }
        started.elapsed()
    });

    ready_rx.recv().expect("waiter is running");
    // Holding the GIL, spin until the waiter's timeout sets the
    // drop-request bit in this thread's eval-breaker word — the fairness
    // mechanism that forces the switch.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observed = false;
    while Instant::now() < deadline {
        // SAFETY: `holder` is this thread's own state.
        if unsafe { eval_breaker::pending(holder) } {
            observed = true;
            break;
        }
        thread::yield_now();
    }
    assert!(observed, "the waiter never requested a GIL drop");

    // SAFETY: `holder` is this thread's own state.
    unsafe { gil::detach(holder) };
    let waited = waiter.join().expect("waiter thread");
    // The waiter only acquires after timing out for a full interval.
    let interval = Duration::from_micros(gil::switch_interval_us());
    assert!(
        waited + Duration::from_millis(1) >= interval,
        "waiter attached after {waited:?}, faster than the {interval:?} interval"
    );
    // Acquiring cleared the previous holder's bit.
    // SAFETY: `holder` is this thread's own state.
    assert!(!unsafe { eval_breaker::pending(holder) });
    // SAFETY: `holder` is the current thread's own state.
    assert!(!unsafe { (*holder).attached });
    assert_eq!(PyGILState_Check(), 0);

    // The switch-interval API round trips.
    gil::PyEval_SetSwitchInterval(0.002);
    assert_eq!(gil::switch_interval_us(), 2_000);
    assert!((gil::PyEval_GetSwitchInterval() - 0.002).abs() < 1e-12);
    gil::PyEval_SetSwitchInterval(0.005);

    // The allow-threads pair hands the same state back.
    // SAFETY: `holder` is this thread's own state.
    unsafe { gil::attach(holder) };
    let saved = gil::PyEval_SaveThread();
    assert!(ptr::eq(saved, holder));
    // SAFETY: `holder` is the current thread's own state.
    assert!(!unsafe { (*holder).attached });
    // SAFETY: the state `PyEval_SaveThread` returned for this thread.
    unsafe {
        gil::PyEval_RestoreThread(saved);
        assert!((*holder).attached);
        gil::detach(holder);
    }
}

#[test]
fn gilstate_ensure_attaches_foreign_threads() {
    init();
    let _serial = SERIAL.lock().unwrap();

    let foreign = thread::spawn(|| {
        // A foreign thread has no state until Ensure creates one.
        assert!(tstate::try_current().is_null());
        let first = PyGILState_Ensure();
        assert_eq!(first, gil::PyGILState_UNLOCKED);
        assert_eq!(PyGILState_Check(), 1);

        // Nesting keeps the GIL held.
        let second = PyGILState_Ensure();
        assert_eq!(second, gil::PyGILState_LOCKED);

        // The error state works on the foreign thread while attached.
        tstate::set_string(exc::value_error(), b"foreign thread");
        assert!(ptr::eq(tstate::occurred(), exc::value_error()));
        assert_eq!(tstate::message(), b"foreign thread");
        tstate::clear();
        assert!(tstate::occurred().is_null());

        PyGILState_Release(second);
        assert_eq!(PyGILState_Check(), 1);
        PyGILState_Release(first);
        assert_eq!(PyGILState_Check(), 0);

        // The state stays bound to the thread for reuse (3.12 keeps it);
        // it is only detached.
        let state = tstate::try_current();
        assert!(!state.is_null());
        // SAFETY: this thread's own state.
        assert!(!unsafe { (*state).attached });
        // Re-Ensure finds it and reattaches.
        let again = PyGILState_Ensure();
        assert_eq!(again, gil::PyGILState_UNLOCKED);
        PyGILState_Release(again);
    });
    foreign.join().expect("foreign thread");

    // The state carries its allocator cache pointer (spec section 10.2).
    let state = tstate::current();
    // SAFETY: the current thread's own state.
    assert!(!unsafe { (*state).alloc_cache.is_null() });
    // Recurring reads return the same state.
    assert!(ptr::eq(tstate::current(), state));
}

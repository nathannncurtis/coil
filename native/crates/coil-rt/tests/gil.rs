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

#[test]
fn an_unlocked_release_detaches_between_save_and_restore() {
    init();
    let _serial = SERIAL.lock().unwrap();

    // pystate.c's PyGILState_Release detaches on every UNLOCKED release,
    // not only when the counter runs out: a thread that Ensured, released
    // the GIL with Py_BEGIN_ALLOW_THREADS and Ensured again leaves the GIL
    // held for exactly the window before PyEval_RestoreThread.
    let first = PyGILState_Ensure();
    assert_eq!(first, gil::PyGILState_UNLOCKED);
    assert_eq!(PyGILState_Check(), 1);
    let saved = gil::PyEval_SaveThread();
    assert_eq!(PyGILState_Check(), 0);
    let second = PyGILState_Ensure();
    assert_eq!(second, gil::PyGILState_UNLOCKED);
    PyGILState_Release(second);
    assert_eq!(PyGILState_Check(), 0, "an UNLOCKED release must detach");
    // SAFETY: the state PyEval_SaveThread returned for this thread.
    unsafe { gil::PyEval_RestoreThread(saved) };
    assert_eq!(PyGILState_Check(), 1);
    PyGILState_Release(first);
    assert_eq!(PyGILState_Check(), 0);
}

#[test]
fn a_drop_request_does_not_survive_a_gil_switch() {
    init();
    let _serial = SERIAL.lock().unwrap();

    let a = tstate::current();
    // SAFETY: `a` is the current thread's own state.
    unsafe {
        gil::attach(a);
        // A waiter's timeout writes this bit in the holder's word; stage
        // one directly, exactly as take_gil's timeout path does.
        eval_breaker::set_drop_request(a);
        assert!(eval_breaker::pending(a));
    }

    let (took_tx, took_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let other = thread::spawn(move || {
        let c = tstate::current();
        // SAFETY: `c` is this thread's own state.
        unsafe {
            gil::attach(c);
            // The acquire reset the request: neither the new holder's
            // word nor the one it took over from holds it.
            assert!(!eval_breaker::pending(c));
        }
        took_tx.send(()).expect("main is listening");
        done_rx.recv().expect("main answers");
        // SAFETY: `c` is this thread's own state.
        unsafe { gil::detach(c) };
    });
    // Hand the GIL over so the other thread can take it.
    // SAFETY: `a` is the current thread's own state.
    unsafe { gil::detach(a) };
    took_rx.recv().expect("other thread attached");
    // SAFETY: `a` is the current thread's own state.
    assert!(!unsafe { eval_breaker::pending(a) });
    done_tx.send(()).expect("other thread is listening");
    other.join().expect("other thread");

    // A switch back: re-attaching clears this state's own word too, so a
    // request raised against an earlier hold cannot stick and force a
    // spurious drop at every back-edge poll.
    // SAFETY: `a` is the current thread's own state.
    unsafe {
        gil::attach(a);
        assert!(
            !eval_breaker::pending(a),
            "drop-request bit went stale across a GIL switch"
        );
        gil::detach(a);
    }
}

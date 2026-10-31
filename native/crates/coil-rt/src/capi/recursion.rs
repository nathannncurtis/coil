//! CPython's `_Py_EnterRecursiveCall`/`_Py_LeaveRecursiveCall` pair for
//! the C API entry points this crate implements (spec section 10.5): a
//! per-thread depth counter that turns runaway C-level recursion into
//! `RecursionError` instead of a stack overflow. The interpreter's own
//! frame recursion count lives on the thread state and is owned by the
//! compiled-frame machinery, so the C API keeps its own.

use std::cell::Cell;

use crate::tstate;
use crate::types::exc as exceptions;

/// CPython's default `sys.setrecursionlimit` value.
const RECURSION_LIMIT: usize = 1000;

thread_local! {
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Enter a C-level recursion; on success the caller must pair this with
/// [`leave`]. On overflow a `RecursionError` carrying CPython's message
/// (`"maximum recursion depth exceeded"` plus `where`, e.g. `" while
/// calling a Python object"`) is pending and false is returned.
pub(crate) fn enter(where_: &str) -> bool {
    let ok = DEPTH.with(|depth| {
        let current = depth.get();
        if current >= RECURSION_LIMIT {
            false
        } else {
            depth.set(current + 1);
            true
        }
    });
    if !ok {
        tstate::set_string(
            exceptions::recursion_error(),
            format!("maximum recursion depth exceeded{where_}").as_bytes(),
        );
    }
    ok
}

/// Leave a recursion entered (successfully) with [`enter`].
pub(crate) fn leave() {
    DEPTH.with(|depth| {
        let current = depth.get();
        if current > 0 {
            depth.set(current - 1);
        }
    });
}

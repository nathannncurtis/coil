//! The foreign-object table: objects `PyObject_Init` initialises on memory
//! outside the GC heap (static types, `PyModuleDef`, extension-owned
//! storage) are recorded here so the collector recognises them and never
//! frees them (spec section 9.1).

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

static FOREIGN: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();

fn table() -> &'static Mutex<HashSet<usize>> {
    FOREIGN.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Record `ptr` as a foreign object. Idempotent.
pub fn register(ptr: usize) {
    table()
        .lock()
        .expect("foreign-object table poisoned")
        .insert(ptr);
}

/// Remove the record for `ptr`; returns true when it was present.
pub fn unregister(ptr: usize) -> bool {
    table()
        .lock()
        .expect("foreign-object table poisoned")
        .remove(&ptr)
}

/// Whether `ptr` was registered as a foreign object.
pub fn contains(ptr: usize) -> bool {
    table()
        .lock()
        .expect("foreign-object table poisoned")
        .contains(&ptr)
}

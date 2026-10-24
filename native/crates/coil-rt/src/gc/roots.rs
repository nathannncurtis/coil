//! The collector's root registries (section 9.2): registered static types
//! and `PyModuleDef`s, compiled code's module tables, the `sys.modules`
//! placeholder, the `atexit` list, the foreign-object table, and the
//! thread states with their pending exceptions, `exc_info` stacks and
//! recorded compiled frames. The weak-reference table and the finalizer
//! queue are roots too and live beside their phases (`weakref.rs`,
//! `finalize.rs`).

use core::ffi::{c_int, c_void};

use coil_capi_sys::{PyModuleDef, PyObject};

use crate::gc::mark::MarkState;
use crate::gc::stack;
use crate::tstate;

/// The registered static types: every type `PyType_Ready` readied without
/// `Py_TPFLAGS_HEAPTYPE` (immortal objects outside the GC heap whose
/// `tp_dict`, `tp_mro`, `tp_bases`, `tp_base` and `tp_subclasses` keep
/// whole graphs alive). `PyModuleDef_Init`'s registrations arrive with the
/// C API layer; `register_module_def` is the hook it will call.
fn static_types() -> &'static std::sync::Mutex<Vec<usize>> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<Vec<usize>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Record `t` as a static type root. Called from `PyType_Ready`; foreign
/// and immortal, so never unregistered.
pub fn register_static_type(t: *mut PyObject) {
    if t.is_null() {
        return;
    }
    let mut table = static_types().lock().expect("static-type table poisoned");
    if !table.contains(&(t as usize)) {
        table.push(t as usize);
    }
}

/// The registered `PyModuleDef`s, whose `m_copy` (the module's `__dict__`)
/// and `m_traverse` reference graphs must survive.
fn module_defs() -> &'static std::sync::Mutex<Vec<usize>> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<Vec<usize>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Record `def` as a module-definition root: the `PyModuleDef_Init` hook.
pub fn register_module_def(def: *mut PyModuleDef) {
    if def.is_null() {
        return;
    }
    let mut table = module_defs().lock().expect("module-def table poisoned");
    if !table.contains(&(def as usize)) {
        table.push(def as usize);
    }
}

/// A compiled module table: `count` contiguous `PyObject*` slots of
/// globals, constants and interned strings, registered by
/// `coil_rt_register_module_table` at image startup. The slot base is an
/// address, not a typed pointer, so the registry is `Send`.
struct ModuleTable {
    slots: usize,
    count: usize,
}

fn module_tables() -> &'static std::sync::Mutex<Vec<ModuleTable>> {
    static TABLE: std::sync::OnceLock<std::sync::Mutex<Vec<ModuleTable>>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// `coil_rt_register_module_table` (abi.toml `[functions]`): register a
/// compiled image's module table as a root set. Zero on success, -1 with
/// `ValueError` set on an empty table.
///
/// # Safety
///
/// `table` must address `count` contiguous `PyObject*` slots the compiled
/// image keeps for the process lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn coil_rt_register_module_table(
    table: *mut *mut PyObject,
    count: usize,
) -> c_int {
    if table.is_null() || count == 0 {
        crate::tstate::set_string(
            crate::types::exc::value_error(),
            b"coil_rt_register_module_table: empty module table",
        );
        return -1;
    }
    module_tables()
        .lock()
        .expect("module-table registry poisoned")
        .push(ModuleTable {
            slots: table as usize,
            count,
        });
    0
}

/// The `sys.modules` placeholder (section 9.2): the import system's
/// mapping object, whatever the import task installs; until then a single
/// registered root the tests can hold objects in.
fn sys_modules() -> &'static std::sync::Mutex<usize> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<usize>> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(0))
}

/// Install `object` as the `sys.modules` placeholder root.
pub fn set_sys_modules(object: *mut PyObject) {
    *sys_modules().lock().expect("sys.modules slot poisoned") = object as usize;
}

/// The `atexit` list: `(callback, args)` pairs the `atexit` module
/// registers; shutdown runs them, the collector only keeps them alive.
fn atexit_list() -> &'static std::sync::Mutex<Vec<(usize, usize)>> {
    static LIST: std::sync::OnceLock<std::sync::Mutex<Vec<(usize, usize)>>> =
        std::sync::OnceLock::new();
    LIST.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Register an `atexit` callback with its argument tuple (null allowed);
/// returns the registration's index, `atexit_unregister`'s handle.
pub fn atexit_register(callback: *mut PyObject, args: *mut PyObject) -> usize {
    let mut list = atexit_list().lock().expect("atexit list poisoned");
    list.push((callback as usize, args as usize));
    list.len() - 1
}

/// Drop the `atexit` registration with handle `index`, if it is current.
pub fn atexit_unregister(index: usize) {
    let mut list = atexit_list().lock().expect("atexit list poisoned");
    if let Some(pair) = list.get_mut(index) {
        *pair = (0, 0);
    }
}

/// Mark every root of section 9.2 except the weak-reference table and the
/// finalizer queue, whose phases mark them; called once before phase 5
/// and again after each phase that can resurrect (marking is monotone, so
/// re-running only adds).
pub(crate) fn mark_all_roots(marks: &mut MarkState) {
    for addr in static_types()
        .lock()
        .expect("static-type table poisoned")
        .iter()
    {
        marks.mark(*addr as *mut PyObject);
    }
    for addr in module_defs()
        .lock()
        .expect("module-def table poisoned")
        .iter()
    {
        mark_module_def(marks, *addr as *mut PyModuleDef);
    }
    for table in module_tables()
        .lock()
        .expect("module-table registry poisoned")
        .iter()
    {
        // SAFETY: the compiled image keeps its module table for the
        // process lifetime; the collector holds the GIL, so no compiled
        // store into it races the read.
        let slots = unsafe {
            core::slice::from_raw_parts(table.slots as *const *mut PyObject, table.count)
        };
        marks.mark_all(slots);
    }
    let modules = *sys_modules().lock().expect("sys.modules slot poisoned");
    if modules != 0 {
        marks.mark(modules as *mut PyObject);
    }
    for (callback, args) in atexit_list().lock().expect("atexit list poisoned").iter() {
        marks.mark(*callback as *mut PyObject);
        marks.mark(*args as *mut PyObject);
    }
    // Foreign objects: `PyObject_Init` recorded them; C holds them, so
    // their referents are roots for as long as they stay registered.
    let mut mark_foreign = |addr: usize| marks.mark(addr as *mut PyObject);
    crate::alloc::foreign::each(&mut mark_foreign);
    // Thread states: the pending exception and traceback, the `exc_info`
    // stack, the context, and every recorded compiled frame's walk.
    for state in tstate::registry::all() {
        // SAFETY: registered states live until the process ends.
        unsafe {
            let state = &*state;
            marks.mark(state.current_exception);
            marks.mark(state.current_traceback);
            marks.mark(state.context);
            for item in &state.exc_info {
                marks.mark(item.exc_type);
                marks.mark(item.exc_value);
                marks.mark(item.exc_traceback);
            }
            // Every anchor on the chain gets its own walk: re-entry through
            // C nests segments whose frames the top anchor's chain cannot
            // reach (the C return address between them is uncovered).
            if !state.frame.is_null() {
                stack::mark_frames(state.frame as usize, marks);
            }
            for parent in &state.frame_parents {
                stack::mark_frames(*parent, marks);
            }
        }
    }
}

/// Mark through one `PyModuleDef`: `m_copy` and, when the def supplies
/// `m_traverse`, its referents.
fn mark_module_def(marks: &mut MarkState, def: *mut PyModuleDef) {
    // SAFETY: the def is registered and its owner keeps it for the
    // process lifetime.
    unsafe {
        marks.mark((*def).m_base.m_copy);
        if let Some(traverse) = (*def).m_traverse {
            // SAFETY: the def's own traverse visits its referents with the
            // marking visitor.
            traverse(
                def.cast(),
                crate::gc::mark::visit_mark,
                marks as *mut MarkState as *mut c_void,
            );
        }
    }
}

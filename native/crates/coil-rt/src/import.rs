//! The import system: `sys.modules`, the module-index table, the native
//! (Rust) module table and `.pyd` loading (spec section 10.4).

use core::ffi::{c_char, c_int, c_void};
use std::collections::HashMap;
use std::sync::Mutex;

use coil_capi_sys::{PyModuleDef, PyObject};

use crate::gc::{counted, roots};
use crate::modules;
use crate::tstate;
use crate::types::dict;
use crate::types::exc::{self as exceptions};
use crate::types::unicode;

// ---------------------------------------------------------------- the state

/// The import system's process-wide state. Object references are stored as
/// addresses so the struct is `Send`; the collector keeps the referents
/// alive through `sys.modules` (a root) and the module-def table.
struct ImportState {
    /// The `sys.modules` mapping, or null before `init`.
    modules: usize,
    /// True once `init` has run.
    initialized: bool,
    /// The next module index to hand out (1, 2, 3, ...).
    next_index: isize,
    /// The `modules_by_index` table, padded with nulls (0).
    modules_by_index: Vec<usize>,
    /// The loaded-extension cache: `(filename, name) -> def address`.
    extensions: HashMap<(String, String), usize>,
}

fn state() -> &'static Mutex<ImportState> {
    static STATE: std::sync::OnceLock<Mutex<ImportState>> = std::sync::OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(ImportState {
            modules: 0,
            initialized: false,
            next_index: 0,
            modules_by_index: Vec::new(),
            extensions: HashMap::new(),
        })
    })
}

// --------------------------------------------------- the module-index table

/// `_PyImport_GetNextModuleIndex`: the next index (1, 2, 3, ...).
pub fn next_module_index() -> isize {
    let mut state = state().lock().expect("import state poisoned");
    state.next_index += 1;
    state.next_index
}

/// True once `init` has run.
pub fn is_initialized() -> bool {
    state().lock().expect("import state poisoned").initialized
}

/// `_modules_by_index_set`: store `module` at `def`'s index, padding with
/// nulls.
///
/// # Safety
///
/// `def` must be a valid module definition; `module` must be a live object or
/// null.
pub unsafe fn modules_by_index_set(def: *mut PyModuleDef, module: *mut PyObject) -> c_int {
    // SAFETY: the contracts above.
    let index = unsafe { (*def).m_base.m_index };
    if index == 0 {
        tstate::set_string(exceptions::system_error(), b"invalid module index");
        return -1;
    }
    let mut state = state().lock().expect("import state poisoned");
    if index as usize >= state.modules_by_index.len() {
        state.modules_by_index.resize(index as usize + 1, 0);
    }
    let old = state.modules_by_index[index as usize];
    if old != 0 {
        // SAFETY: `old` is a live object.
        unsafe { counted::Py_DecRef(old as *mut PyObject) };
    }
    if !module.is_null() {
        // SAFETY: `module` is a live object.
        unsafe { counted::Py_IncRef(module) };
    }
    state.modules_by_index[index as usize] = module as usize;
    0
}

/// `_modules_by_index_clear_one`: clear `def`'s index.
///
/// # Safety
///
/// `def` must be a valid module definition.
pub unsafe fn modules_by_index_clear(def: *mut PyModuleDef) -> c_int {
    // SAFETY: the contract above.
    let index = unsafe { (*def).m_base.m_index };
    if index == 0 {
        tstate::set_string(exceptions::system_error(), b"invalid module index");
        return -1;
    }
    let mut state = state().lock().expect("import state poisoned");
    if index as usize >= state.modules_by_index.len() {
        return 0;
    }
    let old = state.modules_by_index[index as usize];
    if old != 0 {
        // SAFETY: `old` is a live object.
        unsafe { counted::Py_DecRef(old as *mut PyObject) };
    }
    state.modules_by_index[index as usize] = 0;
    0
}

/// `_modules_by_index_get`: the module at `def`'s index, or null.
///
/// # Safety
///
/// `def` must be a valid module definition.
pub unsafe fn modules_by_index_get(def: *mut PyModuleDef) -> *mut PyObject {
    // SAFETY: the contract above.
    let index = unsafe { (*def).m_base.m_index };
    if index == 0 {
        tstate::set_string(exceptions::system_error(), b"invalid module index");
        return core::ptr::null_mut();
    }
    let state = state().lock().expect("import state poisoned");
    if index as usize >= state.modules_by_index.len() {
        tstate::set_string(exceptions::system_error(), b"Module index out of bounds.");
        return core::ptr::null_mut();
    }
    let value = state.modules_by_index[index as usize];
    if value == 0 {
        return core::ptr::null_mut();
    }
    // SAFETY: `value` is a live object.
    unsafe { counted::Py_IncRef(value as *mut PyObject) };
    value as *mut PyObject
}

// --------------------------------------------------------- the C API surface

/// See the CPython 3.12 contract for this entry point; the `sys.modules`
/// mapping, borrowed.
///
/// # Safety
///
/// The import machinery must be initialized.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_GetModuleDict() -> *mut PyObject {
    let modules = state().lock().expect("import state poisoned").modules as *mut PyObject;
    if modules.is_null() {
        fatal("PyImport_GetModuleDict: sys.modules not initialized");
    }
    modules
}

/// `_PyImport_SetModule`: set `name` to `m` in `sys.modules`.
///
/// # Safety
///
/// `name` and `m` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyImport_SetModule(name: *mut PyObject, m: *mut PyObject) -> c_int {
    // SAFETY: the dict and the operands are valid.
    unsafe { dict::PyDict_SetItem(PyImport_GetModuleDict(), name, m) }
}

/// `_PyImport_SetModuleString`: set `name` to `m` in `sys.modules`.
///
/// # Safety
///
/// `name` must be a NUL-terminated string and `m` a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyImport_SetModuleString(name: *const c_char, m: *mut PyObject) -> c_int {
    // SAFETY: the dict, the key and the value are valid.
    unsafe { dict::PyDict_SetItemString(PyImport_GetModuleDict(), name, m) }
}

/// `import_add_module`: the module for `name`, creating it if needed; a new
/// reference.
///
/// # Safety
///
/// `name` must be a valid object.
unsafe fn import_add_module(name: *mut PyObject) -> *mut PyObject {
    // SAFETY: the import machinery is initialized.
    let modules = unsafe { PyImport_GetModuleDict() };
    // SAFETY: `modules` is a dict and `name` a valid object.
    let existing = unsafe { dict::PyDict_GetItemWithError(modules, name) };
    if existing.is_null() {
        if tstate::occurred().is_null() {
            tstate::clear();
        }
        return core::ptr::null_mut();
    }
    // SAFETY: `existing` is a valid object.
    if unsafe { modules::is_module(existing) } {
        // SAFETY: `existing` is a live object.
        unsafe { counted::Py_IncRef(existing) };
        return existing;
    }
    // SAFETY: `existing` is a live object we are replacing.
    unsafe { counted::Py_DecRef(existing) };
    // SAFETY: `name` is a valid object.
    let module = unsafe { modules::PyModule_NewObject(name) };
    if module.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the dict, the key and the value are valid.
    if unsafe { dict::PyDict_SetItem(modules, name, module) } < 0 {
        unsafe { counted::Py_DecRef(module) };
        return core::ptr::null_mut();
    }
    module
}

/// See the CPython 3.12 contract for this entry point; a borrowed
/// reference.
///
/// # Safety
///
/// `name` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AddModuleObject(name: *mut PyObject) -> *mut PyObject {
    // SAFETY: `name` is a valid object.
    let module = unsafe { import_add_module(name) };
    if !module.is_null() {
        // Convert the new reference to a borrowed one.
        unsafe { counted::Py_DecRef(module) };
    }
    module
}

/// See the CPython 3.12 contract for this entry point; a borrowed
/// reference.
///
/// # Safety
///
/// `name` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyImport_AddModule(name: *const c_char) -> *mut PyObject {
    // SAFETY: `name` is a NUL-terminated string.
    let nameobj = unsafe { unicode::PyUnicode_FromString(name) };
    if nameobj.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `nameobj` is a valid object.
    let module = unsafe { PyImport_AddModuleObject(nameobj) };
    unsafe { counted::Py_DecRef(nameobj) };
    module
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must be a module, `name` and `filename` valid objects, `modules`
/// the `sys.modules` mapping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyImport_FixupExtensionObject(
    module: *mut PyObject,
    name: *mut PyObject,
    filename: *mut PyObject,
    modules: *mut PyObject,
) -> c_int {
    // SAFETY: the operands are valid.
    if unsafe { dict::PyDict_SetItem(modules, name, module) } < 0 {
        return -1;
    }
    // SAFETY: `module` is a module and `name`/`filename` valid objects.
    if unsafe { fix_up_extension(module, name, filename) } < 0 {
        if unsafe { dict::PyDict_DelItem(modules, name) } < 0 {
            tstate::clear();
        }
        return -1;
    }
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `module` must be a module, `name` a NUL-terminated string, `modules` the
/// `sys.modules` mapping.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyImport_FixupBuiltin(
    module: *mut PyObject,
    name: *const c_char,
    modules: *mut PyObject,
) -> c_int {
    // SAFETY: `name` is a NUL-terminated string.
    let nameobj = unsafe { unicode::PyUnicode_InternFromString(name) };
    if nameobj.is_null() {
        return -1;
    }
    let res = unsafe { _PyImport_FixupExtensionObject(module, nameobj, nameobj, modules) };
    unsafe { counted::Py_DecRef(nameobj) };
    res
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _PyImport_IsInitialized() -> c_int {
    if is_initialized() { 1 } else { 0 }
}

// ---------------------------------------------------------- extension fixing

/// `is_core_module`: `sys` and `builtins` are core modules whose `m_copy`
/// is not set.
///
/// # Safety
///
/// `name` and `filename` must be valid objects.
unsafe fn is_core_module(name: *mut PyObject, filename: *mut PyObject) -> bool {
    // SAFETY: `name` and `filename` are valid objects.
    unsafe {
        core::ptr::eq(name, filename)
            && (unicode::_PyUnicode_EqualToASCIIString(name, c"sys".as_ptr()) != 0
                || unicode::_PyUnicode_EqualToASCIIString(name, c"builtins".as_ptr()) != 0)
    }
}

/// `fix_up_extension`: register `module` in the module-index table and, for a
/// single-phase module, snapshot its dict into `m_copy`.
///
/// # Safety
///
/// `module` must be a module, `name` and `filename` valid objects.
unsafe fn fix_up_extension(
    module: *mut PyObject,
    name: *mut PyObject,
    filename: *mut PyObject,
) -> c_int {
    // SAFETY: `module` is a valid object.
    if !unsafe { modules::is_module(module) } {
        tstate::set_string(
            exceptions::system_error(),
            b"fix_up_extension: not a module",
        );
        return -1;
    }
    // SAFETY: `module` is a module.
    let def = unsafe { modules::PyModule_GetDef(module) };
    if def.is_null() {
        tstate::set_string(exceptions::system_error(), b"PyModule_GetDef() failed");
        return -1;
    }
    // SAFETY: `def` is a valid module definition and `module` a live module.
    if unsafe { modules_by_index_set(def, module) } < 0 {
        return -1;
    }
    // SAFETY: `def` is a valid module definition.
    if unsafe { (*def).m_size == -1 } && !unsafe { is_core_module(name, filename) } {
        // SAFETY: `def` is a valid module definition.
        if !unsafe { (*def).m_base.m_copy.is_null() } {
            // SAFETY: `m_copy` is a live object.
            unsafe { counted::Py_DecRef((*def).m_base.m_copy) };
            // SAFETY: `def` is a valid module definition.
            unsafe { (*def).m_base.m_copy = core::ptr::null_mut() };
        }
        // SAFETY: `module` is a module.
        let dict = unsafe { modules::PyModule_GetDict(module) };
        if dict.is_null() {
            return -1;
        }
        // SAFETY: `dict` is a valid dict.
        let copy = unsafe { dict::PyDict_Copy(dict) };
        if copy.is_null() {
            return -1;
        }
        // SAFETY: `def` is a valid module definition.
        unsafe { (*def).m_base.m_copy = copy };
    }
    extensions_cache_set(filename, name, def);
    0
}

/// The loaded-extension cache: `(filename, name) -> def`.
fn extensions_cache_set(filename: *mut PyObject, name: *mut PyObject, def: *mut PyModuleDef) {
    let key = (object_to_string(filename), object_to_string(name));
    state()
        .lock()
        .expect("import state poisoned")
        .extensions
        .insert(key, def as usize);
}

fn extensions_cache_get(filename: *mut PyObject, name: *mut PyObject) -> *mut PyModuleDef {
    let key = (object_to_string(filename), object_to_string(name));
    state()
        .lock()
        .expect("import state poisoned")
        .extensions
        .get(&key)
        .map(|addr| *addr as *mut PyModuleDef)
        .unwrap_or(core::ptr::null_mut())
}

/// A best-effort string copy of a module name or path, for cache keys.
fn object_to_string(obj: *mut PyObject) -> String {
    if obj.is_null() {
        return String::new();
    }
    // SAFETY: `obj` is a valid object.
    let utf8 = unsafe { unicode::PyUnicode_AsUTF8(obj) };
    if utf8.is_null() {
        return String::new();
    }
    // SAFETY: `utf8` is a NUL-terminated UTF-8 string.
    unsafe { core::ffi::CStr::from_ptr(utf8) }
        .to_string_lossy()
        .into_owned()
}

/// `import_find_extension`: the module for a previously loaded extension,
/// or null; a new reference.
///
/// # Safety
///
/// `name` and `filename` must be valid objects.
unsafe fn import_find_extension(name: *mut PyObject, filename: *mut PyObject) -> *mut PyObject {
    let def = extensions_cache_get(filename, name);
    if def.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `def` is a valid module definition.
    let modules = unsafe { PyImport_GetModuleDict() };
    if unsafe { (*def).m_size == -1 } {
        // SAFETY: `def` is a valid module definition.
        let m_copy = unsafe { (*def).m_base.m_copy };
        let m_copy = if m_copy.is_null() {
            // A core module (sys/builtins) has no `m_copy`.
            return core::ptr::null_mut();
        } else {
            m_copy
        };
        // SAFETY: `name` is a valid object.
        let module = unsafe { import_add_module(name) };
        if module.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: `module` is a module.
        let mdict = unsafe { modules::PyModule_GetDict(module) };
        if mdict.is_null() {
            unsafe { counted::Py_DecRef(module) };
            return core::ptr::null_mut();
        }
        // SAFETY: the dicts are valid.
        if unsafe { dict::PyDict_Update(mdict, m_copy) } < 0 {
            unsafe { counted::Py_DecRef(module) };
            return core::ptr::null_mut();
        }
        // SAFETY: `def` is a valid module definition and `module` a live module.
        if unsafe { modules_by_index_set(def, module) } < 0 {
            if unsafe { dict::PyDict_DelItem(modules, name) } < 0 {
                tstate::clear();
            }
            unsafe { counted::Py_DecRef(module) };
            return core::ptr::null_mut();
        }
        module
    } else {
        // SAFETY: `def` is a valid module definition.
        let init = unsafe { (*def).m_base.m_init };
        if init.is_none() {
            return core::ptr::null_mut();
        }
        // SAFETY: `init` is a valid module-init function.
        let module = unsafe { (init.unwrap())() };
        if module.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: the dict, the key and the value are valid.
        if unsafe { dict::PyDict_SetItem(modules, name, module) } < 0 {
            unsafe { counted::Py_DecRef(module) };
            return core::ptr::null_mut();
        }
        // SAFETY: `def` is a valid module definition and `module` a live module.
        if unsafe { modules_by_index_set(def, module) } < 0 {
            if unsafe { dict::PyDict_DelItem(modules, name) } < 0 {
                tstate::clear();
            }
            unsafe { counted::Py_DecRef(module) };
            return core::ptr::null_mut();
        }
        module
    }
}

// ----------------------------------------------------------- dynamic loading

/// The platform's shared-library handle.
#[cfg(unix)]
type Library = *mut c_void;

#[cfg(windows)]
type Library = *mut c_void;

/// Load `filename` as a shared library, or null with `ImportError` set.
///
/// # Safety
///
/// `filename` must be a valid object holding the path.
unsafe fn load_library(filename: *mut PyObject) -> Library {
    // SAFETY: `filename` is a valid object.
    let path = unsafe { unicode::PyUnicode_AsUTF8(filename) };
    if path.is_null() {
        tstate::set_string(exceptions::import_error(), b"dynamic module has no origin");
        return core::ptr::null_mut();
    }
    // SAFETY: `path` is a NUL-terminated string.
    let handle = unsafe { open_library(path) };
    if handle.is_null() {
        // SAFETY: `path` is a NUL-terminated string.
        let message = unsafe { format_load_error(path) };
        tstate::set_string(exceptions::import_error(), message.as_bytes());
        return core::ptr::null_mut();
    }
    handle
}

/// The `PyInit_<name>` symbol for `name`, or null.
///
/// # Safety
///
/// `handle` must be a loaded library and `name` a valid object.
unsafe fn find_init_function(
    handle: Library,
    name: *mut PyObject,
) -> Option<unsafe extern "C" fn() -> *mut PyObject> {
    // SAFETY: `name` is a valid object.
    let encoded = unsafe { encoded_init_name(name) };
    if encoded.is_null() {
        return None;
    }
    // SAFETY: `handle` is a loaded library and `encoded` a NUL-terminated
    // symbol name.
    let symbol = unsafe { find_symbol(handle, encoded) };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: `symbol` is the address of a `PyInit_*` function.
    Some(unsafe {
        core::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> *mut PyObject>(symbol)
    })
}

/// `PyInit_<name>`: the module name's last component, `-` mapped to `_`.
///
/// # Safety
///
/// `name` must be a valid object.
unsafe fn encoded_init_name(name: *mut PyObject) -> *mut c_char {
    // SAFETY: `name` is a valid object.
    let utf8 = unsafe { unicode::PyUnicode_AsUTF8(name) };
    if utf8.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `utf8` is a NUL-terminated string.
    let bytes = unsafe { core::ffi::CStr::from_ptr(utf8) }.to_bytes();
    let tail = match bytes.iter().rposition(|&b| b == b'.') {
        Some(pos) => &bytes[pos + 1..],
        None => bytes,
    };
    let mut encoded = Vec::with_capacity(tail.len() + 6);
    encoded.extend_from_slice(b"PyInit_");
    for &b in tail {
        encoded.push(if b == b'-' { b'_' } else { b });
    }
    encoded.push(0);
    let ptr = encoded.leak().as_mut_ptr();
    // SAFETY: `encoded` is a NUL-terminated string that lives for the
    // process.
    unsafe { core::mem::transmute::<*mut u8, *mut c_char>(ptr) }
}

/// The platform's `dlopen` equivalent.
///
/// # Safety
///
/// `path` must be a NUL-terminated path.
unsafe fn open_library(path: *const c_char) -> Library {
    #[cfg(unix)]
    {
        // SAFETY: `path` is a NUL-terminated path; the flags are the
        // standard immediate, local-load flags.
        unsafe { extern_dlopen(path, RTLD_NOW | RTLD_LOCAL) }
    }
    #[cfg(windows)]
    {
        // SAFETY: `path` is a NUL-terminated path; the flags match the
        // spec's search set.
        unsafe { extern_load_library(path) }
    }
}

/// The platform's `dlsym` equivalent.
///
/// # Safety
///
/// `handle` must be a loaded library and `symbol` a NUL-terminated name.
unsafe fn find_symbol(handle: Library, symbol: *const c_char) -> *mut c_void {
    #[cfg(unix)]
    {
        // SAFETY: `handle` is a loaded library and `symbol` a NUL-terminated
        // name.
        unsafe { extern_dlsym(handle, symbol) }
    }
    #[cfg(windows)]
    {
        // SAFETY: `handle` is a loaded library and `symbol` a NUL-terminated
        // name.
        unsafe { extern_get_proc_address(handle, symbol) }
    }
}

/// A human-readable load error for `path`.
///
/// # Safety
///
/// `path` must be a NUL-terminated path.
unsafe fn format_load_error(path: *const c_char) -> String {
    #[cfg(unix)]
    {
        // SAFETY: `dlerror` returns a NUL-terminated message or null.
        let err = unsafe { extern_dlerror() };
        if err.is_null() {
            return format!(
                "cannot load module at {}",
                unsafe { core::ffi::CStr::from_ptr(path) }.to_string_lossy()
            );
        }
        // SAFETY: `err` is a NUL-terminated message.
        format!(
            "cannot load module at {}: {}",
            unsafe { core::ffi::CStr::from_ptr(path) }.to_string_lossy(),
            unsafe { core::ffi::CStr::from_ptr(err) }.to_string_lossy()
        )
    }
    #[cfg(windows)]
    {
        // SAFETY: `GetLastError` reads the calling thread's last-error code,
        // set by the failed `LoadLibraryExW` in `open_library`.
        let code = unsafe { GetLastError() };
        format!(
            "cannot load module at {} (Windows error {})",
            unsafe { core::ffi::CStr::from_ptr(path) }.to_string_lossy(),
            code
        )
    }
}

/// `_PyImport_LoadDynamicModuleWithSpec`: load the extension named by
/// `spec` and return its module, or null with the error set.
///
/// # Safety
///
/// `spec` must be a valid module spec.
pub unsafe fn load_dynamic(spec: *mut PyObject) -> *mut PyObject {
    // SAFETY: `spec` is a valid object.
    let name = unsafe { crate::object::attr::get_attr_string(spec, c"name".as_ptr()) };
    if name.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `spec` is a valid object.
    let path = unsafe { crate::object::attr::get_attr_string(spec, c"origin".as_ptr()) };
    if path.is_null() {
        // SAFETY: `name` is a live object.
        unsafe { counted::Py_DecRef(name) };
        return core::ptr::null_mut();
    }
    // SAFETY: `path` is a valid object.
    let handle = unsafe { load_library(path) };
    if handle.is_null() {
        // SAFETY: `name` and `path` are live objects.
        unsafe {
            counted::Py_DecRef(name);
            counted::Py_DecRef(path);
        };
        return core::ptr::null_mut();
    }
    // SAFETY: `handle` is a loaded library and `name` a valid object.
    let init = unsafe { find_init_function(handle, name) };
    let init = match init {
        Some(init) => init,
        None => {
            tstate::set_string(
                exceptions::import_error(),
                b"dynamic module does not define module export function",
            );
            // SAFETY: `name` and `path` are live objects.
            unsafe {
                counted::Py_DecRef(name);
                counted::Py_DecRef(path);
            };
            return core::ptr::null_mut();
        }
    };
    // SAFETY: `init` is a valid module-init function.
    let result = unsafe { init() };
    if result.is_null() {
        // SAFETY: `name` and `path` are live objects.
        unsafe {
            counted::Py_DecRef(name);
            counted::Py_DecRef(path);
        };
        return core::ptr::null_mut();
    }
    // A multi-phase init returns a `PyModuleDef`.
    // SAFETY: `result` is a valid object.
    if unsafe { modules::is_module(result) } {
        // SAFETY: `result` is a module.
        let def = unsafe { modules::PyModule_GetDef(result) };
        if def.is_null() {
            tstate::set_string(
                exceptions::system_error(),
                b"did not return an extension module",
            );
            // SAFETY: `result`, `name` and `path` are live objects.
            unsafe {
                counted::Py_DecRef(result);
                counted::Py_DecRef(name);
                counted::Py_DecRef(path);
            };
            return core::ptr::null_mut();
        }
        // SAFETY: `def` is the module's def and `init` its init function.
        unsafe { (*def).m_base.m_init = Some(init) };
        // SAFETY: the operands are valid.
        let res =
            unsafe { _PyImport_FixupExtensionObject(result, name, path, PyImport_GetModuleDict()) };
        if res < 0 {
            // SAFETY: `result` is a live object.
            unsafe { counted::Py_DecRef(result) };
        }
        // SAFETY: `name` and `path` are live objects.
        unsafe {
            counted::Py_DecRef(name);
            counted::Py_DecRef(path);
        };
        if res < 0 {
            return core::ptr::null_mut();
        }
        result
    } else if unsafe { header_type_is_module_def(result) } {
        // SAFETY: `result` is a `PyModuleDef` object; the cast is a pointer
        // reinterpretation.
        let def = result.cast::<PyModuleDef>();
        // SAFETY: `def` is a valid module definition and `spec` a valid
        // module spec.
        let module = unsafe { modules::PyModule_FromDefAndSpec2(def, spec, 0) };
        // SAFETY: `result`, `name` and `path` are live objects.
        unsafe {
            counted::Py_DecRef(result);
            counted::Py_DecRef(name);
            counted::Py_DecRef(path);
        };
        module
    } else {
        tstate::set_string(
            exceptions::system_error(),
            b"module init function returned an unexpected type",
        );
        // SAFETY: `result`, `name` and `path` are live objects.
        unsafe {
            counted::Py_DecRef(result);
            counted::Py_DecRef(name);
            counted::Py_DecRef(path);
        };
        core::ptr::null_mut()
    }
}

/// True if `obj` is a `PyModuleDef` object (a multi-phase init result).
///
/// # Safety
///
/// `obj` must be a valid object.
unsafe fn header_type_is_module_def(obj: *mut PyObject) -> bool {
    // SAFETY: a header read within the object.
    unsafe {
        core::ptr::eq(
            crate::object::header::type_of(obj),
            modules::module_def_type(),
        )
    }
}

// ------------------------------------------------------------- native table

/// A native (Rust) module: its name and the function that creates it.
#[derive(Clone)]
pub struct NativeModule {
    /// The module's name.
    pub name: &'static str,
    /// Create the module; a new reference, or null with the error set.
    pub create: fn() -> *mut PyObject,
}

/// The native module table, registered at `init`.
fn native_modules() -> &'static Mutex<Vec<NativeModule>> {
    static TABLE: std::sync::OnceLock<Mutex<Vec<NativeModule>>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register the native module table.
pub fn register_native_modules(table: &[NativeModule]) {
    let mut native = native_modules().lock().expect("native table poisoned");
    native.clear();
    native.extend_from_slice(table);
}

/// `is_builtin`: 1 if `name` is a native module, -1 if it is a core module
/// (`sys`/`builtins`), 0 otherwise.
///
/// # Safety
///
/// `name` must be a valid object.
pub unsafe fn is_builtin(name: *mut PyObject) -> c_int {
    // SAFETY: `name` is a valid object.
    let utf8 = unsafe { unicode::PyUnicode_AsUTF8(name) };
    if utf8.is_null() {
        return 0;
    }
    // SAFETY: `utf8` is a NUL-terminated string.
    let name = unsafe { core::ffi::CStr::from_ptr(utf8) }.to_string_lossy();
    if name == "sys" || name == "builtins" {
        return -1;
    }
    let native = native_modules().lock().expect("native table poisoned");
    if native.iter().any(|m| m.name == name) {
        1
    } else {
        0
    }
}

// ------------------------------------------------------------ the _imp core

/// `_imp.create_dynamic`: create the module for the dynamic module named by
/// `spec`; a new reference.
///
/// # Safety
///
/// `spec` must be a valid module spec.
pub unsafe fn create_dynamic(spec: *mut PyObject) -> *mut PyObject {
    // SAFETY: `spec` is a valid object.
    let name = unsafe { crate::object::attr::get_attr_string(spec, c"name".as_ptr()) };
    if name.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `spec` is a valid object.
    let path = unsafe { crate::object::attr::get_attr_string(spec, c"origin".as_ptr()) };
    if path.is_null() {
        // SAFETY: `name` is a live object.
        unsafe { counted::Py_DecRef(name) };
        return core::ptr::null_mut();
    }
    // SAFETY: `name` and `path` are valid objects.
    let module = unsafe { import_find_extension(name, path) };
    let found_or_error = !module.is_null() || !tstate::occurred().is_null();
    let module = if found_or_error {
        module
    } else {
        // SAFETY: `spec` is a valid module spec.
        unsafe { load_dynamic(spec) }
    };
    // SAFETY: `name` and `path` are live objects.
    unsafe {
        counted::Py_DecRef(name);
        counted::Py_DecRef(path);
    };
    module
}

/// `_imp.exec_dynamic`: execute the module `module`.
///
/// # Safety
///
/// `module` must be a valid object.
pub unsafe fn exec_dynamic(module: *mut PyObject) -> c_int {
    // SAFETY: `module` is a valid object.
    if !unsafe { modules::is_module(module) } {
        return 0;
    }
    // SAFETY: `module` is a module.
    let def = unsafe { modules::PyModule_GetDef(module) };
    if def.is_null() {
        return 0;
    }
    // SAFETY: `module` is a module.
    let state = unsafe { modules::PyModule_GetState(module) };
    if !state.is_null() {
        return 0;
    }
    // SAFETY: `module` is a module and `def` its def.
    unsafe { modules::PyModule_ExecDef(module, def) }
}

// ----------------------------------------------------------------- bootstrap

/// `init`: create `sys.modules`, the `sys`, `builtins` and `_imp` modules,
/// and register the native module table. Idempotent.
pub fn init() -> c_int {
    let st = state().lock().expect("import state poisoned");
    if st.initialized {
        return 0;
    }
    drop(st);

    // The `sys.modules` mapping.
    let modules = dict::PyDict_New();
    if modules.is_null() {
        return -1;
    }

    // The `builtins` module.
    let builtins = crate::modules::builtins::create();
    if builtins.is_null() {
        // SAFETY: `modules` is a live object.
        unsafe { counted::Py_DecRef(modules) };
        return -1;
    }
    // The `sys` module, which owns the `sys.modules` mapping.
    // SAFETY: `modules` is the `sys.modules` mapping.
    let sys = unsafe { crate::modules::sys::create(modules) };
    if sys.is_null() {
        // SAFETY: the objects are live.
        unsafe {
            counted::Py_DecRef(builtins);
            counted::Py_DecRef(modules);
        };
        return -1;
    }
    // The `_imp` module.
    let imp = crate::modules::imp::create();
    if imp.is_null() {
        // SAFETY: the objects are live.
        unsafe {
            counted::Py_DecRef(sys);
            counted::Py_DecRef(builtins);
            counted::Py_DecRef(modules);
        };
        return -1;
    }

    // Register the native module table before the modules are placed in
    // `sys.modules`, so `is_builtin` sees them.
    register_native_modules(&[
        NativeModule {
            name: "builtins",
            create: crate::modules::builtins::create,
        },
        NativeModule {
            name: "sys",
            create: || unsafe {
                // SAFETY: the import machinery is initialized.
                crate::modules::sys::create(PyImport_GetModuleDict())
            },
        },
        NativeModule {
            name: "_imp",
            create: crate::modules::imp::create,
        },
    ]);

    // Record the `sys.modules` root and mark the system initialized.
    {
        let mut st = state().lock().expect("import state poisoned");
        st.modules = modules as usize;
        st.initialized = true;
    }
    roots::set_sys_modules(modules);

    // Place the core modules in `sys.modules`.
    // SAFETY: the operands are valid.
    if unsafe { dict::PyDict_SetItemString(modules, c"builtins".as_ptr(), builtins) } < 0
        || unsafe { dict::PyDict_SetItemString(modules, c"sys".as_ptr(), sys) } < 0
        || unsafe { dict::PyDict_SetItemString(modules, c"_imp".as_ptr(), imp) } < 0
    {
        // SAFETY: the objects are live.
        unsafe {
            counted::Py_DecRef(imp);
            counted::Py_DecRef(sys);
            counted::Py_DecRef(builtins);
            counted::Py_DecRef(modules);
        };
        return -1;
    }
    // SAFETY: the objects are live.
    unsafe {
        counted::Py_DecRef(imp);
        counted::Py_DecRef(sys);
        counted::Py_DecRef(builtins);
        counted::Py_DecRef(modules);
    };
    0
}

/// `fatal`: report an unrecoverable import error and abort.
fn fatal(message: &str) -> ! {
    eprintln!("coil-rt: {message}");
    std::process::abort();
}

// ---------------------------------------------------- the platform bindings

#[cfg(unix)]
const RTLD_NOW: c_int = 2;
#[cfg(unix)]
const RTLD_LOCAL: c_int = 0;

#[cfg(unix)]
unsafe extern "C" {
    fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

#[cfg(unix)]
unsafe fn extern_dlopen(path: *const c_char, flags: c_int) -> Library {
    // SAFETY: `path` is a NUL-terminated path.
    unsafe { dlopen(path, flags) }
}

#[cfg(unix)]
unsafe fn extern_dlsym(handle: Library, symbol: *const c_char) -> *mut c_void {
    // SAFETY: `handle` is a loaded library and `symbol` a NUL-terminated
    // name.
    unsafe { dlsym(handle, symbol) }
}

#[cfg(unix)]
unsafe fn extern_dlerror() -> *mut c_char {
    // SAFETY: `dlerror` returns a NUL-terminated message or null.
    unsafe { dlerror() }
}

#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR: u32 = 0x0000_0010;
// A single bit that enables the application, system32 and user directories.
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_DEFAULT_DIRS: u32 = 0x0000_1000;
#[cfg(windows)]
const LOAD_LIBRARY_SEARCH_USER_DIRS: u32 = 0x0000_0008;

#[cfg(windows)]
unsafe extern "C" {
    fn LoadLibraryExW(
        libfilename: *const u16,
        modulehandle: *mut c_void,
        dwflags: u32,
    ) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, procname: *const c_char) -> *mut c_void;
    fn AddDllDirectory(directory: *const u16) -> *mut c_void;
    fn GetModuleFileNameW(module: *mut c_void, filename: *mut u16, size: u32) -> u32;
}

// TEMP CI diagnostic: render a wide buffer up to its first NUL.
#[cfg(windows)]
fn diag_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

// Matches the `extern "system"` declaration in `types/exc.rs`; a `#[link]`
// attribute is required on the block, so it cannot join the `extern "C"` one.
#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLastError() -> u32;
}

#[cfg(windows)]
unsafe fn extern_load_library(path: *const c_char) -> Library {
    // SAFETY: `path` is a NUL-terminated UTF-8 path.
    let wide = unsafe { utf8_to_wide(path) };
    // SAFETY: `wide` is a NUL-terminated wide string; the flags match the
    // spec's search set.
    let handle = unsafe {
        LoadLibraryExW(
            wide,
            core::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR
                | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS
                | LOAD_LIBRARY_SEARCH_USER_DIRS,
        )
    };
    // TEMP CI diagnostic: identify what module the handle actually points to.
    let mut name = [0u16; 32768];
    let n = if handle.is_null() {
        0
    } else {
        // SAFETY: `handle` is non-null and `name` is a writable buffer of
        // `name.len()` wide chars.
        unsafe { GetModuleFileNameW(handle, name.as_mut_ptr(), name.len() as u32) }
    };
    eprintln!(
        "coil-rt-diag: LoadLibraryExW({}) -> handle {:p}, module file {:?} (len {})",
        unsafe { core::ffi::CStr::from_ptr(path) }.to_string_lossy(),
        handle,
        diag_wide(&name[..n as usize]),
        n
    );
    handle
}

#[cfg(windows)]
unsafe fn extern_get_proc_address(handle: Library, symbol: *const c_char) -> *mut c_void {
    // SAFETY: `handle` is a loaded library and `symbol` a NUL-terminated
    // name.
    let result = unsafe { GetProcAddress(handle, symbol) };
    // TEMP CI diagnostic.
    eprintln!(
        "coil-rt-diag: GetProcAddress(handle {:p}, {}) -> {:p} (err {})",
        handle,
        unsafe { core::ffi::CStr::from_ptr(symbol) }.to_string_lossy(),
        result,
        unsafe { GetLastError() }
    );
    result
}

#[cfg(windows)]
/// Register `directory` for DLL search; the spec's `AddDllDirectory` hook.
///
/// # Safety
///
/// `directory` must be a NUL-terminated UTF-8 path.
pub unsafe fn add_dll_directory(directory: *const c_char) {
    // SAFETY: `directory` is a NUL-terminated UTF-8 path.
    let wide = unsafe { utf8_to_wide(directory) };
    // SAFETY: `wide` is a NUL-terminated wide string.
    unsafe { AddDllDirectory(wide) };
}

#[cfg(windows)]
unsafe fn utf8_to_wide(path: *const c_char) -> *mut u16 {
    // SAFETY: `path` is a NUL-terminated UTF-8 string.
    let bytes = unsafe { core::ffi::CStr::from_ptr(path) }.to_bytes();
    let mut wide = Vec::with_capacity(bytes.len() * 2 + 2);
    for &b in bytes {
        wide.push(b as u16);
    }
    wide.push(0);
    wide.leak().as_mut_ptr()
}

//! Dynamic module loading (spec sections 10.4 and 10.6).
//!
//! Builds a tiny C extension with the cc crate and loads it through both PEP
//! 489 init styles. The test drives the runtime through the cdylib (the DLL
//! the extension links against) rather than the rlib the test binary is
//! linked against, so the extension's `python312` imports resolve to the same
//! runtime copy that loads it. On Windows, also loads CPython's `_socket.pyd`
//! from the embeddable zip far enough for `PyInit__socket` to return a module.

use std::ffi::CStr;
use std::ffi::CString;
use std::ffi::c_char;
use std::ffi::c_int;
use std::ffi::c_void;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

type PyObj = *mut c_void;

type FnInit = unsafe extern "C" fn() -> c_int;
type FnModuleDict = unsafe extern "C" fn() -> PyObj;
type FnDictGet = unsafe extern "C" fn(PyObj, *const c_char) -> PyObj;
type FnGetAttr = unsafe extern "C" fn(PyObj, *const c_char) -> PyObj;
type FnSetAttr = unsafe extern "C" fn(PyObj, *const c_char, PyObj) -> c_int;
type FnCFuncGet = unsafe extern "C" fn(PyObj) -> *mut c_void;
type FnModuleNew = unsafe extern "C" fn(*const c_char) -> PyObj;
type FnFromStr = unsafe extern "C" fn(*const c_char) -> PyObj;
type FnAsUtf8 = unsafe extern "C" fn(PyObj) -> *const c_char;
type FnGetDef = unsafe extern "C" fn(PyObj) -> *mut c_void;
type FnGetState = unsafe extern "C" fn(PyObj) -> *mut c_void;
type FnGetDict = unsafe extern "C" fn(PyObj) -> PyObj;
type FnDecRef = unsafe extern "C" fn(PyObj);
type FnErrOccurred = unsafe extern "C" fn() -> PyObj;
type FnErrFetch = unsafe extern "C" fn(*mut PyObj, *mut PyObj, *mut PyObj);
type CFunc = unsafe extern "C" fn(PyObj, PyObj) -> PyObj;

/// The first five fields of `PyModuleDef` (its `m_base`), enough to read
/// `m_copy` at a fixed offset without pulling in the full layout.
#[repr(C)]
struct ModuleDefBase {
    ob_refcnt: i64,
    ob_type: *mut c_void,
    m_init: *mut c_void,
    m_index: i64,
    m_copy: PyObj,
}

#[cfg(unix)]
mod dl {
    use std::ffi::CStr;
    use std::ffi::CString;
    use std::ffi::c_char;
    use std::ffi::c_int;
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const RTLD_NOW: c_int = 0x0002;
    // `RTLD_GLOBAL` differs by platform: `0x8` on Darwin, `0x100` on glibc
    // (where `0x8` is `RTLD_DEEPBIND`). Loading the cdylib without it keeps
    // its symbols out of the global scope, so the extension's imports fail.
    #[cfg(target_os = "macos")]
    const RTLD_GLOBAL: c_int = 0x0008;
    #[cfg(not(target_os = "macos"))]
    const RTLD_GLOBAL: c_int = 0x0100;

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlerror() -> *const c_char;
    }

    pub fn open_global(path: &Path) -> *mut c_void {
        let cpath = CString::new(path.as_os_str().as_bytes()).expect("path has NUL byte");
        let handle = unsafe { dlopen(cpath.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
        if handle.is_null() {
            let err = unsafe { dlerror() };
            let msg = if err.is_null() {
                String::from("dlopen failed")
            } else {
                unsafe { CStr::from_ptr(err) }
                    .to_string_lossy()
                    .into_owned()
            };
            panic!("dlopen({}): {msg}", path.display());
        }
        handle
    }

    pub fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
        let cname = CString::new(name).expect("symbol has NUL byte");
        unsafe { dlsym(handle, cname.as_ptr()) }
    }
}

#[cfg(windows)]
mod dl {
    use std::ffi::CString;
    use std::ffi::c_char;
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    unsafe extern "C" {
        fn LoadLibraryW(libname: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, procname: *const c_char) -> *mut c_void;
    }

    fn to_wide(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn open_global(path: &Path) -> *mut c_void {
        let wide = to_wide(path);
        let handle = unsafe { LoadLibraryW(wide.as_ptr()) };
        if handle.is_null() {
            panic!("LoadLibrary({}) failed", path.display());
        }
        handle
    }

    pub fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
        let cname = CString::new(name).expect("symbol has NUL byte");
        unsafe { GetProcAddress(handle, cname.as_ptr()) }
    }
}

/// Resolve a symbol from `handle` and cast it to the concrete C function type
/// `$ty`. The transmute is size-checked because `$ty` is concrete.
macro_rules! resolve {
    ($handle:expr, $name:expr => $ty:ty) => {{
        let raw = dl::sym($handle, $name);
        assert!(!raw.is_null(), "missing symbol {}", $name);
        // SAFETY: the resolved symbol is a function of type `$ty`, the same
        // size as the raw pointer.
        unsafe { std::mem::transmute::<*mut c_void, $ty>(raw) }
    }};
}

fn target_triple() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        _ => format!("{arch}-unknown-linux-gnu"),
    }
}

fn get_tool() -> cc::Tool {
    let triple = target_triple();
    // SAFETY: the value is constant for the process and only the cc crate
    // reads HOST; tests that call this do so before spawning threads.
    unsafe { std::env::set_var("HOST", &triple) };
    let mut build = cc::Build::new();
    build.target(&triple).opt_level(0).debug(true);
    build.try_get_compiler().expect("cc compiler")
}

/// Ensure the coil-rt cdylib is built and return its path.
fn ensure_cdylib() -> PathBuf {
    let exe = std::env::current_exe().expect("current exe");
    let target_dir = exe
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("target dir");
    let cdylib = if cfg!(windows) {
        target_dir.join("debug").join("python312.dll")
    } else if cfg!(target_os = "macos") {
        target_dir.join("debug").join("libpython312.dylib")
    } else {
        target_dir.join("debug").join("libpython312.so")
    };
    if !cdylib.exists() {
        let mut cmd = Command::new("cargo");
        cmd.arg("build")
            .arg("--lib")
            .arg("-p")
            .arg("coil-rt")
            .env("CARGO_TARGET_DIR", target_dir);
        let out = cmd.output().expect("cargo build");
        assert!(
            out.status.success(),
            "cargo build --lib -p coil-rt failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    cdylib
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd.output().expect(what);
    assert!(
        out.status.success(),
        "{what} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Compile `src` into a shared library named `name` in `out_dir` and return
/// the library path.
fn build_extension(name: &str, src: &str, out_dir: &Path) -> PathBuf {
    let src_path = out_dir.join(format!("{name}.c"));
    std::fs::write(&src_path, src).expect("write C source");
    let out_path = if cfg!(windows) {
        out_dir.join(format!("{name}.pyd"))
    } else {
        out_dir.join(format!("{name}.so"))
    };
    let tool = get_tool();
    #[cfg(windows)]
    {
        build_extension_windows(&tool, &src_path, &out_path, out_dir, name);
    }
    #[cfg(not(windows))]
    {
        build_extension_unix(&tool, &src_path, &out_path);
    }
    out_path
}

#[cfg(not(windows))]
fn build_extension_unix(tool: &cc::Tool, src_path: &Path, out_path: &Path) {
    let mut cmd = tool.to_command();
    cmd.arg(src_path);
    if cfg!(target_os = "macos") {
        cmd.arg("-shared").arg("-undefined").arg("dynamic_lookup");
    } else {
        cmd.arg("-shared").arg("-fPIC");
    }
    cmd.arg("-o").arg(out_path);
    run(&mut cmd, "build extension");
}

#[cfg(windows)]
fn build_extension_windows(
    tool: &cc::Tool,
    src_path: &Path,
    out_path: &Path,
    out_dir: &Path,
    name: &str,
) {
    let obj_path = out_dir.join(format!("{name}.obj"));
    let mut c = tool.to_command();
    c.arg("/c")
        .arg(src_path)
        .arg(format!("/Fo:{}", obj_path.display()));
    run(&mut c, "compile extension");

    let def_path = out_dir.join("python312.def");
    std::fs::write(
        &def_path,
        "LIBRARY python312\nEXPORTS\n    PyModule_Create2\n    PyModule_AddStringConstant\n    PyModuleDef_Init\n    PyModule_AddIntConstant\n",
    )
    .expect("write def file");
    let lib_path = out_dir.join("python312.lib");
    let mut l = Command::new("lib");
    l.arg("/nologo")
        .arg(format!("/def:{}", def_path.display()))
        .arg(format!("/out:{}", lib_path.display()));
    run(&mut l, "generate import library");

    let mut k = tool.to_command();
    k.arg("/nologo")
        .arg("/DLL")
        .arg("/ENTRY:DllMain")
        .arg(&obj_path)
        .arg(&lib_path)
        .arg(format!("/Fe:{}", out_path.display()));
    run(&mut k, "link extension");
}

/// Build a stand-in module spec: a module carrying `name` and `origin`
/// attributes, the two the loader reads.
fn make_spec(
    module_new: FnModuleNew,
    set_attr: FnSetAttr,
    from_str: FnFromStr,
    dec_ref: FnDecRef,
    name: &str,
    path: &str,
) -> PyObj {
    let spec = unsafe { module_new(c"spec".as_ptr()) };
    assert!(!spec.is_null(), "PyModule_New failed");
    // `&str` literals are not NUL-terminated; `PyUnicode_FromString` scans
    // for the terminator, so hand it an owned NUL-terminated copy.
    let name_c = CString::new(name).expect("name has NUL byte");
    let name_obj = unsafe { from_str(name_c.as_ptr()) };
    assert!(!name_obj.is_null(), "PyUnicode_FromString(name) failed");
    assert_eq!(
        unsafe { set_attr(spec, c"name".as_ptr(), name_obj) },
        0,
        "set spec.name failed"
    );
    unsafe { dec_ref(name_obj) };
    let path_c = CString::new(path).expect("path has NUL byte");
    let origin_obj = unsafe { from_str(path_c.as_ptr()) };
    assert!(!origin_obj.is_null(), "PyUnicode_FromString(origin) failed");
    assert_eq!(
        unsafe { set_attr(spec, c"origin".as_ptr(), origin_obj) },
        0,
        "set spec.origin failed"
    );
    unsafe { dec_ref(origin_obj) };
    spec
}

/// Print the current exception (if any) to stderr and clear it.
fn report_error(
    err_occurred: FnErrOccurred,
    err_fetch: FnErrFetch,
    as_utf8: FnAsUtf8,
    dec_ref: FnDecRef,
) {
    let exc = unsafe { err_occurred() };
    if exc.is_null() {
        return;
    }
    let mut t = std::ptr::null_mut();
    let mut v = std::ptr::null_mut();
    let mut tb = std::ptr::null_mut();
    unsafe { err_fetch(&mut t, &mut v, &mut tb) };
    let msg = if !v.is_null() {
        let utf8 = unsafe { as_utf8(v) };
        if utf8.is_null() {
            String::from("<non-string error>")
        } else {
            unsafe { CStr::from_ptr(utf8) }
                .to_string_lossy()
                .into_owned()
        }
    } else {
        String::from("<no error value>")
    };
    eprintln!("error: {msg}");
    for p in [t, v, tb] {
        if !p.is_null() {
            unsafe { dec_ref(p) };
        }
    }
}

const SINGLE_SRC: &str = r#"
#include <stdint.h>
#if defined(_WIN32)
#include <windows.h>
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance; (void)reason; (void)reserved;
    return TRUE;
}
#endif
typedef struct { int64_t ob_refcnt; void *ob_type; } PyObject;
typedef struct { PyObject ob_base; void *m_init; int64_t m_index; PyObject *m_copy; } PyModuleDef_Base;
typedef struct { int slot; void *value; } PyModuleDef_Slot;
typedef struct PyMethodDef PyMethodDef;
typedef struct { PyModuleDef_Base m_base; const char *m_name; const char *m_doc; int64_t m_size; PyMethodDef *m_methods; PyModuleDef_Slot *m_slots; void *m_traverse; void *m_clear; void *m_free; } PyModuleDef;
extern PyObject *PyModule_Create2(PyModuleDef *m, int module_api_version);
extern int PyModule_AddStringConstant(PyObject *m, const char *name, const char *value);
#if defined(_WIN32)
__declspec(dllexport) PyObject *PyInit_expext_single(void) {
#else
PyObject *PyInit_expext_single(void) {
#endif
    static PyModuleDef def = { { { 1, 0 }, 0, 0, 0 }, "expext_single", "single-phase test module", -1, 0, 0, 0, 0, 0 };
    PyObject *m = PyModule_Create2(&def, 0);
    if (m != 0) { PyModule_AddStringConstant(m, "kind", "single"); }
    return m;
}
"#;

const MULTI_SRC: &str = r#"
#include <stdint.h>
#if defined(_WIN32)
#include <windows.h>
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
    (void)instance; (void)reason; (void)reserved;
    return TRUE;
}
#endif
typedef struct { int64_t ob_refcnt; void *ob_type; } PyObject;
typedef struct { PyObject ob_base; void *m_init; int64_t m_index; PyObject *m_copy; } PyModuleDef_Base;
typedef struct { int slot; void *value; } PyModuleDef_Slot;
typedef struct PyMethodDef PyMethodDef;
typedef struct { PyModuleDef_Base m_base; const char *m_name; const char *m_doc; int64_t m_size; PyMethodDef *m_methods; PyModuleDef_Slot *m_slots; void *m_traverse; void *m_clear; void *m_free; } PyModuleDef;
extern PyObject *PyModuleDef_Init(PyModuleDef *def);
extern int PyModule_AddIntConstant(PyObject *m, const char *name, int value);
typedef struct { int ran; } multi_state;
static int multi_exec(PyObject *module) { PyModule_AddIntConstant(module, "exec_ran", 1); return 0; }
static PyModuleDef_Slot multi_slots[] = { { 2, (void *)multi_exec }, { 0, 0 } };
#if defined(_WIN32)
__declspec(dllexport) PyObject *PyInit_expext_multi(void) {
#else
PyObject *PyInit_expext_multi(void) {
#endif
    static PyModuleDef def = { { { 1, 0 }, 0, 0, 0 }, "expext_multi", "multi-phase test module", sizeof(multi_state), 0, multi_slots, 0, 0, 0 };
    PyModuleDef_Init(&def);
    return (PyObject *)&def;
}
"#;

#[test]
fn loads_extension_through_both_init_styles() {
    let cdylib = ensure_cdylib();
    let handle = dl::open_global(&cdylib);

    let init: FnInit = resolve!(handle, "coil_rt_init" => FnInit);
    let module_dict: FnModuleDict = resolve!(handle, "PyImport_GetModuleDict" => FnModuleDict);
    let dict_get: FnDictGet = resolve!(handle, "PyDict_GetItemString" => FnDictGet);
    let get_attr: FnGetAttr = resolve!(handle, "PyObject_GetAttrString" => FnGetAttr);
    let set_attr: FnSetAttr = resolve!(handle, "PyObject_SetAttrString" => FnSetAttr);
    let cfunc_get: FnCFuncGet = resolve!(handle, "PyCFunction_GetFunction" => FnCFuncGet);
    let module_new: FnModuleNew = resolve!(handle, "PyModule_New" => FnModuleNew);
    let from_str: FnFromStr = resolve!(handle, "PyUnicode_FromString" => FnFromStr);
    let as_utf8: FnAsUtf8 = resolve!(handle, "PyUnicode_AsUTF8" => FnAsUtf8);
    let get_def: FnGetDef = resolve!(handle, "PyModule_GetDef" => FnGetDef);
    let get_state: FnGetState = resolve!(handle, "PyModule_GetState" => FnGetState);
    let get_dict: FnGetDict = resolve!(handle, "PyModule_GetDict" => FnGetDict);
    let dec_ref: FnDecRef = resolve!(handle, "Py_DecRef" => FnDecRef);
    let err_occurred: FnErrOccurred = resolve!(handle, "PyErr_Occurred" => FnErrOccurred);
    let err_fetch: FnErrFetch = resolve!(handle, "PyErr_Fetch" => FnErrFetch);

    assert_eq!(unsafe { init() }, 0, "coil_rt_init failed");

    let modules = unsafe { module_dict() };
    assert!(!modules.is_null(), "PyImport_GetModuleDict failed");
    let imp = unsafe { dict_get(modules, c"_imp".as_ptr()) };
    assert!(!imp.is_null(), "_imp not in sys.modules");
    let create_attr = unsafe { get_attr(imp, c"create_dynamic".as_ptr()) };
    assert!(!create_attr.is_null(), "create_dynamic not found");
    let create_raw = unsafe { cfunc_get(create_attr) };
    unsafe { dec_ref(create_attr) };
    assert!(!create_raw.is_null(), "create_dynamic cfunc is null");
    let create_dyn: CFunc = unsafe { std::mem::transmute(create_raw) };
    let exec_attr = unsafe { get_attr(imp, c"exec_dynamic".as_ptr()) };
    assert!(!exec_attr.is_null(), "exec_dynamic not found");
    let exec_raw = unsafe { cfunc_get(exec_attr) };
    unsafe { dec_ref(exec_attr) };
    assert!(!exec_raw.is_null(), "exec_dynamic cfunc is null");
    let exec_dyn: CFunc = unsafe { std::mem::transmute(exec_raw) };

    let out_dir = std::env::temp_dir().join(format!("coil-dynload-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let single_so = build_extension("expext_single", SINGLE_SRC, &out_dir);
    let multi_so = build_extension("expext_multi", MULTI_SRC, &out_dir);

    // Single-phase: init returns a ready module.
    let spec = make_spec(
        module_new,
        set_attr,
        from_str,
        dec_ref,
        "expext_single",
        single_so.to_str().expect("utf8 path"),
    );
    let module = unsafe { create_dyn(imp, spec) };
    if module.is_null() {
        report_error(err_occurred, err_fetch, as_utf8, dec_ref);
    }
    assert!(!module.is_null(), "single-phase create_dynamic failed");
    let def = unsafe { get_def(module) };
    assert!(!def.is_null(), "PyModule_GetDef failed");
    let m_copy = unsafe { (*def.cast::<ModuleDefBase>()).m_copy };
    assert!(!m_copy.is_null(), "m_copy is null for single-phase");
    let dict = unsafe { get_dict(module) };
    assert!(!dict.is_null(), "PyModule_GetDict failed");
    let kind = unsafe { dict_get(dict, c"kind".as_ptr()) };
    assert!(!kind.is_null(), "kind not in dict");
    let kind_utf8 = unsafe { as_utf8(kind) };
    assert!(!kind_utf8.is_null(), "kind is not a string");
    let kind_str = unsafe { CStr::from_ptr(kind_utf8) }.to_str().expect("utf8");
    assert_eq!(kind_str, "single");
    unsafe { dec_ref(module) };
    unsafe { dec_ref(spec) };

    // Multi-phase: init returns a def, exec runs the Py_mod_exec slot.
    let spec = make_spec(
        module_new,
        set_attr,
        from_str,
        dec_ref,
        "expext_multi",
        multi_so.to_str().expect("utf8 path"),
    );
    let module = unsafe { create_dyn(imp, spec) };
    if module.is_null() {
        report_error(err_occurred, err_fetch, as_utf8, dec_ref);
    }
    assert!(!module.is_null(), "multi-phase create_dynamic failed");
    let state_before = unsafe { get_state(module) };
    assert!(state_before.is_null(), "state is not null before exec");
    let exec_result = unsafe { exec_dyn(imp, module) };
    if exec_result.is_null() {
        report_error(err_occurred, err_fetch, as_utf8, dec_ref);
    }
    assert!(!exec_result.is_null(), "exec_dynamic failed");
    unsafe { dec_ref(exec_result) };
    let state_after = unsafe { get_state(module) };
    assert!(!state_after.is_null(), "state is null after exec");
    let dict = unsafe { get_dict(module) };
    let exec_ran = unsafe { dict_get(dict, c"exec_ran".as_ptr()) };
    assert!(!exec_ran.is_null(), "exec_ran not in dict");
    unsafe { dec_ref(module) };
    unsafe { dec_ref(spec) };
}

#[cfg(windows)]
const SOCKET_CHILD_SRC: &str = r#"
#include <windows.h>
#include <stdio.h>
#include <string.h>

typedef void* (*PyInitSocketFn)(void);
typedef int (*PyInitFn)(void);

int main(int argc, char** argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: socket_child <dir>\n");
        return 2;
    }
    char dir[MAX_PATH];
    strncpy(dir, argv[1], MAX_PATH - 1);
    dir[MAX_PATH - 1] = 0;

    char pydll[MAX_PATH], pyd[MAX_PATH];
    snprintf(pydll, MAX_PATH, "%s\\python312.dll", dir);
    snprintf(pyd, MAX_PATH, "%s\\_socket.pyd", dir);

    HMODULE pyh = LoadLibraryA(pydll);
    if (!pyh) {
        fprintf(stderr, "LoadLibrary(python312.dll) failed: %lu\n", GetLastError());
        return 1;
    }
    PyInitFn py_init = (PyInitFn)GetProcAddress(pyh, "Py_Initialize");
    if (!py_init) {
        fprintf(stderr, "GetProcAddress(Py_Initialize) failed\n");
        return 1;
    }
    py_init();

    HMODULE sh = LoadLibraryA(pyd);
    if (!sh) {
        fprintf(stderr, "LoadLibrary(_socket.pyd) failed: %lu\n", GetLastError());
        return 1;
    }
    PyInitSocketFn init = (PyInitSocketFn)GetProcAddress(sh, "PyInit__socket");
    if (!init) {
        fprintf(stderr, "GetProcAddress(PyInit__socket) failed\n");
        return 1;
    }
    void* module = init();
    if (!module) {
        fprintf(stderr, "PyInit__socket returned null\n");
        return 1;
    }
    fprintf(stderr, "PyInit__socket returned a module\n");
    return 0;
}
"#;

#[cfg(windows)]
#[test]
fn loads_socket_extension_from_embeddable_zip() {
    let home = std::env::var("USERPROFILE").expect("USERPROFILE");
    let zip = PathBuf::from(home)
        .join(".coil")
        .join("cache")
        .join("runtimes")
        .join("python-3.12.10-embed-amd64.zip");
    assert!(
        zip.exists(),
        "embeddable zip not found at {}",
        zip.display()
    );

    let out_dir = std::env::temp_dir().join(format!("coil-socket-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let ps = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(format!(
            "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
            zip.display(),
            out_dir.display()
        ))
        .output()
        .expect("powershell");
    assert!(
        ps.status.success(),
        "Expand-Archive failed: {}",
        String::from_utf8_lossy(&ps.stderr)
    );

    let child_src = out_dir.join("socket_child.c");
    std::fs::write(&child_src, SOCKET_CHILD_SRC).expect("write child source");
    let child_exe = out_dir.join("socket_child.exe");
    let tool = get_tool();
    let mut c = tool.to_command();
    c.arg(&child_src)
        .arg(format!("/Fe:{}", child_exe.display()));
    run(&mut c, "compile socket child");

    let out = Command::new(&child_exe)
        .arg(&out_dir)
        .output()
        .expect("run socket child");
    assert!(
        out.status.success(),
        "socket child failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

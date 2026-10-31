//! _testcapi early conformance check (spec section 14 step 1, decision 14).
//!
//! Loads the _testcapi extension built by tools/build_testcapi.py against
//! coil's python312 cdylib, calls PyInit__testcapi, and runs the test_*
//! functions that are pure C API exercises (no interpreter loop). Each test
//! runs in a separate child process so a crashing C API call cannot take down
//! the harness. The failing set must match expected_failures.txt exactly:
//! every listed test fails, and every failing test is listed.
//!
//! The suite runs only when COIL_TESTCAPI_PYD points at a built _testcapi
//! extension; otherwise it is skipped (the Windows CI job builds it).

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

/// The _testcapi test_* functions that are pure C API exercises: no
/// interpreter loop, no subinterpreter, and no platform-specific build
/// guard. Driven through the C API from the child process.
const TESTS: &[&str] = &[
    "test_alignof_max_align_t",
    "test_buildvalue_issue38913",
    "test_buildvalue_N",
    "test_capsule",
    "test_code_api",
    "test_code_extra",
    "test_config",
    "test_current_tstate_matches",
    "test_datetime_capi",
    "test_decref_doesnt_leak",
    "test_dict_iteration",
    "test_empty_argparse",
    "test_from_contiguous",
    "test_from_spec_invalid_metatype_inheritance",
    "test_from_spec_metatype_inheritance",
    "test_frozenset_add_in_capi",
    "test_gc_control",
    "test_gc_visit_objects_basic",
    "test_gc_visit_objects_exit_early",
    "test_get_statictype_slots",
    "test_get_type_dict",
    "test_get_type_name",
    "test_get_type_qualname",
    "test_immortal_builtins",
    "test_immortal_small_ints",
    "test_incref_decref_API",
    "test_incref_doesnt_leak",
    "test_k_code",
    "test_L_code",
    "test_lazy_hash_inheritance",
    "test_list_api",
    "test_long_and_overflow",
    "test_long_api",
    "test_long_as_double",
    "test_long_as_size_t",
    "test_long_as_unsigned_long_long_mask",
    "test_long_long_and_overflow",
    "test_long_numbits",
    "test_longlong_api",
    "test_macros",
    "test_py_clear",
    "test_py_is_funcs",
    "test_py_is_macros",
    "test_py_setref",
    "test_pymem_alloc0",
    "test_pymem_setallocators",
    "test_pymem_setrawallocators",
    "test_pyobject_new",
    "test_pyobject_setallocators",
    "test_PyOS_mystricmp",
    "test_PyOS_mystrnicmp",
    "test_refcount_funcs",
    "test_refcount_macros",
    "test_s_code",
    "test_set_type_size",
    "test_sizeof_c_types",
    "test_string_from_format",
    "test_string_to_double",
    "test_structseq_newtype_doesnt_leak",
    "test_structseq_newtype_null_descr_doc",
    "test_tstate_capi",
    "test_type_from_ephemeral_spec",
    "test_unicode_compare_with_ascii",
    "test_widechar",
    "test_with_docstring",
    "test_xdecref_doesnt_leak",
    "test_xincref_doesnt_leak",
];

/// The child process: loads the cdylib and the extension, inits the runtime,
/// calls PyInit__testcapi, and runs one test. Exits 0 on pass, nonzero on
/// fail or crash.
const CHILD_SRC: &str = r#"
#include <stdio.h>
#include <stdlib.h>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <dlfcn.h>
#endif

typedef void *PyObject;
typedef int (*coil_rt_init_fn)(void);
typedef PyObject *(*pyinit_fn)(void);
typedef PyObject *(*get_attr_fn)(PyObject *, const char *);
typedef PyObject *(*cfunc_get_fn)(PyObject *);
typedef PyObject *(*noargs_fn)(PyObject *);

#ifdef _WIN32
static void *load_lib(const char *path) {
    int len = MultiByteToWideChar(CP_UTF8, 0, path, -1, NULL, 0);
    wchar_t *w = (wchar_t *)malloc((size_t)len * sizeof(wchar_t));
    if (!w) return NULL;
    MultiByteToWideChar(CP_UTF8, 0, path, -1, w, len);
    void *h = (void *)LoadLibraryW(w);
    free(w);
    return h;
}
static void *get_sym(void *h, const char *name) {
    return (void *)GetProcAddress((HMODULE)h, name);
}
#else
static void *load_lib(const char *path) {
    return dlopen(path, RTLD_NOW | RTLD_GLOBAL);
}
static void *get_sym(void *h, const char *name) {
    return dlsym(h, name);
}
#endif

int main(int argc, char **argv) {
    if (argc < 4) {
        fprintf(stderr, "usage: %s <cdylib> <pyd> <test_name>\n", argv[0]);
        return 2;
    }
    void *cdylib = load_lib(argv[1]);
    if (!cdylib) { fprintf(stderr, "load cdylib %s failed\n", argv[1]); return 3; }
    void *pyd = load_lib(argv[2]);
    if (!pyd) { fprintf(stderr, "load pyd %s failed\n", argv[2]); return 4; }

    coil_rt_init_fn init = (coil_rt_init_fn)get_sym(cdylib, "coil_rt_init");
    if (!init) { fprintf(stderr, "coil_rt_init not found\n"); return 5; }
    if (init() != 0) { fprintf(stderr, "coil_rt_init failed\n"); return 6; }

    pyinit_fn pyinit = (pyinit_fn)get_sym(pyd, "PyInit__testcapi");
    if (!pyinit) { fprintf(stderr, "PyInit__testcapi not found\n"); return 7; }
    PyObject *module = pyinit();
    if (!module) { fprintf(stderr, "PyInit__testcapi failed\n"); return 8; }

    get_attr_fn get_attr = (get_attr_fn)get_sym(cdylib, "PyObject_GetAttrString");
    if (!get_attr) { fprintf(stderr, "PyObject_GetAttrString not found\n"); return 9; }
    PyObject *func = get_attr(module, argv[3]);
    if (!func) { fprintf(stderr, "getattr %s failed\n", argv[3]); return 10; }

    cfunc_get_fn cfunc_get = (cfunc_get_fn)get_sym(cdylib, "PyCFunction_GetFunction");
    if (!cfunc_get) { fprintf(stderr, "PyCFunction_GetFunction not found\n"); return 11; }
    noargs_fn fn = (noargs_fn)cfunc_get(func);
    if (!fn) { fprintf(stderr, "PyCFunction_GetFunction %s failed\n", argv[3]); return 12; }

    PyObject *result = fn(module);
    if (!result) {
        fprintf(stderr, "FAIL %s\n", argv[3]);
        return 13;
    }
    printf("PASS %s\n", argv[3]);
    return 0;
}
"#;

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
    // reads HOST; this runs before any other thread is spawned.
    unsafe { std::env::set_var("HOST", &triple) };
    let mut build = cc::Build::new();
    build.target(&triple).opt_level(1);
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

/// Compile the child into an executable in `out_dir` and return its path.
fn build_child(tool: &cc::Tool, out_dir: &Path) -> PathBuf {
    let src_path = out_dir.join("testcapi_child.c");
    std::fs::write(&src_path, CHILD_SRC).expect("write C source");
    let out_path = if cfg!(windows) {
        out_dir.join("testcapi_child.exe")
    } else {
        out_dir.join("testcapi_child")
    };
    let mut cmd = tool.to_command();
    cmd.arg(&src_path);
    if cfg!(windows) {
        cmd.arg(format!("/Fe:{}", out_path.display()));
        cmd.arg("/link").arg("/SUBSYSTEM:CONSOLE");
    } else {
        cmd.arg("-o").arg(&out_path);
    }
    run(&mut cmd, "build child");
    out_path
}

/// Run one test in the child process. Returns (passed, stderr).
fn run_child(child: &Path, cdylib: &Path, pyd: &Path, test: &str) -> (bool, String) {
    let start = std::time::Instant::now();
    let mut cmd = Command::new(child);
    cmd.arg(cdylib).arg(pyd).arg(test);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());
    let mut proc = cmd.spawn().expect("spawn child");
    let mut stderr = proc.stderr.take().expect("stderr piped");
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let status = loop {
        match proc.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed().as_secs() >= 10 {
                    let _ = proc.kill();
                    let _ = proc.wait();
                    let _ = stderr_thread.join();
                    return (false, "timeout after 10s".to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                let _ = stderr_thread.join();
                return (false, format!("wait error: {e}"));
            }
        }
    };
    let stderr_bytes = stderr_thread.join().expect("stderr thread");
    (
        status.success(),
        String::from_utf8_lossy(&stderr_bytes).into_owned(),
    )
}

/// The expected failures: test name to the missing feature and the step that
/// implements it.
fn parse_expected(path: &Path) -> BTreeMap<String, String> {
    let content = std::fs::read_to_string(path).expect("read expected_failures.txt");
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, reason) = line.split_once(':').unwrap_or((line, ""));
            (name.trim().to_string(), reason.trim().to_string())
        })
        .collect()
}

#[test]
fn testcapi_suite() {
    let pyd = match std::env::var("COIL_TESTCAPI_PYD") {
        Ok(path) => PathBuf::from(path),
        Err(_) => {
            eprintln!("COIL_TESTCAPI_PYD not set; skipping _testcapi suite");
            return;
        }
    };
    assert!(pyd.is_file(), "COIL_TESTCAPI_PYD is not a file: {pyd:?}");
    let cdylib = ensure_cdylib();
    let out_dir = std::env::temp_dir().join("coil_testcapi_child");
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let tool = get_tool();
    let child = build_child(&tool, &out_dir);
    let expected = parse_expected(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/capi/expected_failures.txt"),
    );

    let mut passed = 0usize;
    let mut failures: BTreeMap<String, String> = BTreeMap::new();
    for test in TESTS {
        let (ok, stderr) = run_child(&child, &cdylib, &pyd, test);
        if ok {
            passed += 1;
        } else {
            failures.insert(test.to_string(), stderr.trim().to_string());
        }
    }

    let expected_names: BTreeSet<String> = expected.keys().cloned().collect();
    let actual_names: BTreeSet<String> = failures.keys().cloned().collect();
    let missing: Vec<&String> = expected_names.difference(&actual_names).collect();
    let unexpected: Vec<&String> = actual_names.difference(&expected_names).collect();

    println!(
        "_testcapi: {} passed, {} failed of {} tests",
        passed,
        failures.len(),
        TESTS.len()
    );
    for (name, stderr) in &failures {
        let reason = expected
            .get(name)
            .map(String::as_str)
            .unwrap_or("UNEXPECTED");
        println!("  FAIL {name}: {reason}");
        if let Some(line) = stderr.lines().next()
            && !line.is_empty()
        {
            println!("    stderr: {line}");
        }
    }

    assert!(
        missing.is_empty(),
        "expected failures that passed (remove from expected_failures.txt): {missing:?}"
    );
    assert!(
        unexpected.is_empty(),
        "unexpected failures (add to expected_failures.txt): {unexpected:?}"
    );
}

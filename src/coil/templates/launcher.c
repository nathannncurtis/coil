/* Coil's bundled application launcher. Rebuild with tools/build_launcher.py.
 * PyConfig is not part of CPython's stable ABI: compile once per Python minor.
 * No Python import library is linked; load only this bundle's runtime DLL.
 */
#define PY_SSIZE_T_CLEAN
#define Py_NO_ENABLE_SHARED
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <tlhelp32.h>
#include <Python.h>
#include <stdio.h>
#include <stdlib.h>
#include <wchar.h>

#define PATH_CAP 32768
#define WIDEN_(x) L##x
#define WIDEN(x) WIDEN_(x)
#define STR_(x) #x
#define STR(x) STR_(x)
#define PY_TAG STR(PY_MAJOR_VERSION) STR(PY_MINOR_VERSION)

/* Patched in-place by launcher.py, before PE resources or signatures are added.
 * Volatile is intentional: the compiler must read the patched values at runtime.
 */
static const volatile struct {
    char marker[32];
    wchar_t boot_script[256];
    int optimization;
    int portable;
    int error_logging;
} launcher_config = {"COIL_NATIVE_CONFIG_V1_7D39B28A", L"", 0, 0, 0};

typedef struct {
    wchar_t exe[PATH_CAP], root[PATH_CAP], internal[PATH_CAP];
    wchar_t dll[PATH_CAP], zip[PATH_CAP], app[PATH_CAP], lib[PATH_CAP];
    wchar_t boot[PATH_CAP], log[PATH_CAP], outer[PATH_CAP];
} Paths;

static int join(wchar_t *out, const wchar_t *directory, const wchar_t *name) {
    return _snwprintf_s(out, PATH_CAP, _TRUNCATE, L"%s\\%s", directory, name) >= 0;
}

static int has_stderr(void) {
    HANDLE handle = GetStdHandle(STD_ERROR_HANDLE);
    DWORD flags;
    return handle && handle != INVALID_HANDLE_VALUE && GetHandleInformation(handle, &flags);
}

static int portable_identity(wchar_t *outer) {
    wchar_t expected[PATH_CAP], pid_text[32], *end;
    DWORD length = GetEnvironmentVariableW(L"_COIL_PORTABLE_EXE", expected, PATH_CAP);
    if (!length || length >= PATH_CAP) return 0;
    length = GetEnvironmentVariableW(L"_COIL_PORTABLE_PARENT_PID", pid_text, 32);
    if (!length || length >= 32) return 0;
    unsigned long parent_pid = wcstoul(pid_text, &end, 10);
    if (!parent_pid || *end) return 0;
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot == INVALID_HANDLE_VALUE) return 0;
    PROCESSENTRY32W entry;
    entry.dwSize = sizeof(entry);
    int parent_matches = 0;
    if (Process32FirstW(snapshot, &entry)) do {
        if (entry.th32ProcessID == GetCurrentProcessId()) {
            parent_matches = entry.th32ParentProcessID == parent_pid;
            break;
        }
    } while (Process32NextW(snapshot, &entry));
    CloseHandle(snapshot);
    if (!parent_matches) return 0;
    HANDLE parent = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, parent_pid);
    if (!parent) return 0;
    length = PATH_CAP;
    int valid = QueryFullProcessImageNameW(parent, 0, outer, &length) &&
        _wcsicmp(outer, expected) == 0;
    CloseHandle(parent);
    return valid;
}

static void error_message(const Paths *paths, const char *message) {
    FILE *log = NULL;
    if (has_stderr()) {
        fprintf(stderr, "Coil launcher: %s\n", message);
        fflush(stderr);
        return;
    }
    if (launcher_config.error_logging && paths && paths->log[0]) {
        wchar_t directory[PATH_CAP];
        wcscpy_s(directory, PATH_CAP, paths->log);
        wchar_t *last = wcsrchr(directory, L'\\');
        if (last) {
            *last = 0;
            wchar_t *parent = wcsrchr(directory, L'\\');
            if (parent) {
                *parent = 0;
                CreateDirectoryW(directory, NULL);
                *parent = L'\\';
            }
            CreateDirectoryW(directory, NULL);
        }
        if (_wfopen_s(&log, paths->log, L"ab") == 0) {
            fprintf(log, "Coil launcher: %s\n", message);
            fclose(log);
            return;
        }
    }
    OutputDebugStringA(message);
}

/* Delayed opening avoids creating an empty log on every GUI application run.
 * The native startup-error path above writes to the same location.
 */
static const char prelude[] =
    "import sys as _coil_sys\n"
    "_coil_sys.frozen = True\n"
    "if not hasattr(_coil_sys, 'orig_argv'):\n"
    "    _coil_sys.orig_argv = list(_coil_sys.argv)\n"
    "if _coil_sys._coil_error_logging and (_coil_sys.stderr is None or _coil_sys._coil_no_stderr):\n"
    "    import io as _coil_io, os as _coil_os\n"
    "    class _CoilErrorStream(_coil_io.TextIOBase):\n"
    "        encoding = 'utf-8'\n"
    "        errors = 'backslashreplace'\n"
    "        def writable(self): return True\n"
    "        def write(self, text):\n"
    "            if text:\n"
    "                _coil_os.makedirs(_coil_os.path.dirname(_coil_sys._coil_error_log), exist_ok=True)\n"
    "                with open(_coil_sys._coil_error_log, 'a', encoding='utf-8', errors='backslashreplace') as stream:\n"
    "                    stream.write(text)\n"
    "            return len(text)\n"
    "    _coil_sys.stderr = _coil_sys.__stderr__ = _CoilErrorStream()\n";

int wmain(int argc, wchar_t **argv) {
    int result = 1;
    Paths *paths = calloc(1, sizeof(Paths));
    HMODULE python = NULL;
    PyConfig config;
    PyStatus status;
    int config_initialized = 0;

    void (*config_init)(PyConfig *);
    void (*config_clear)(PyConfig *) = NULL;
    PyStatus (*config_string)(PyConfig *, wchar_t **, const wchar_t *);
    PyStatus (*config_argv)(PyConfig *, Py_ssize_t, wchar_t *const *);
    PyStatus (*list_append)(PyWideStringList *, const wchar_t *);
    PyStatus (*initialize)(const PyConfig *);
    int (*status_exception)(PyStatus);
    int (*status_exit)(PyStatus);
    int (*run_main)(void);
    int (*run_string)(const char *, PyCompilerFlags *);
    PyObject *(*unicode_from_wide)(const wchar_t *, Py_ssize_t);
    PyObject *(*long_from_long)(long);
    int (*sys_set)(const char *, PyObject *);
    void (*decref)(PyObject *);
    int (*finalize)(void);

    if (!paths) return 1;
    DWORD length = GetModuleFileNameW(NULL, paths->exe, PATH_CAP);
    if (!length || length >= PATH_CAP) goto path_error;
    wcscpy_s(paths->root, PATH_CAP, paths->exe);
    wchar_t *slash = wcsrchr(paths->root, L'\\');
    if (!slash) goto path_error;
    *slash = 0;

    wchar_t local[PATH_CAP];
    DWORD local_length = GetEnvironmentVariableW(L"LOCALAPPDATA", local, PATH_CAP);
    if (!local_length || local_length >= PATH_CAP) {
        local_length = GetTempPathW(PATH_CAP, local);
        if (!local_length || local_length >= PATH_CAP) local[0] = 0;
    }
    if (local[0]) _snwprintf_s(paths->log, PATH_CAP, _TRUNCATE,
        L"%s\\Coil\\logs\\%s-%lu.log", local, slash + 1, GetCurrentProcessId());

    if (!join(paths->internal, paths->root, L"_internal") ||
        !join(paths->dll, paths->root, L"python" WIDEN(PY_TAG) L".dll") ||
        !join(paths->zip, paths->internal, L"python" WIDEN(PY_TAG) L".zip") ||
        !join(paths->app, paths->internal, L"app") ||
        !join(paths->lib, paths->internal, L"lib") ||
        !join(paths->boot, paths->internal, (const wchar_t *)launcher_config.boot_script)) goto path_error;

    if (!launcher_config.boot_script[0]) {
        error_message(paths, "This launcher has no embedded entry point.");
        goto done;
    }
    /* Avoid current-directory/PATH DLL hijacking; extension modules can use
     * the bundle's two native library directories throughout the process.
     */
    if (!SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) ||
        !AddDllDirectory(paths->root) || !AddDllDirectory(paths->internal)) {
        error_message(paths, "Could not configure bundled DLL search paths.");
        goto done;
    }
    python = LoadLibraryExW(paths->dll, NULL,
        LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
    if (!python) {
        char message[256];
        sprintf_s(message, sizeof(message), "Could not load bundled Python " PY_TAG " (Windows error %lu).", GetLastError());
        error_message(paths, message);
        goto done;
    }
#define LOAD(variable, name) do { \
    *(FARPROC *)&variable = GetProcAddress(python, name); \
    if (!variable) { error_message(paths, "Bundled Python is missing " name); goto done; } \
} while (0)
    LOAD(config_init, "PyConfig_InitIsolatedConfig");
    LOAD(config_clear, "PyConfig_Clear");
    LOAD(config_string, "PyConfig_SetString");
    LOAD(config_argv, "PyConfig_SetArgv");
    LOAD(list_append, "PyWideStringList_Append");
    LOAD(initialize, "Py_InitializeFromConfig");
    LOAD(status_exception, "PyStatus_Exception");
    LOAD(status_exit, "PyStatus_IsExit");
    LOAD(run_main, "Py_RunMain");
    LOAD(run_string, "PyRun_SimpleStringFlags");
    LOAD(unicode_from_wide, "PyUnicode_FromWideChar");
    LOAD(long_from_long, "PyLong_FromLong");
    LOAD(sys_set, "PySys_SetObject");
    LOAD(decref, "Py_DecRef");
    LOAD(finalize, "Py_FinalizeEx");
#undef LOAD

    config_init(&config);
    config_initialized = 1;
    config.parse_argv = 0;
    config.use_environment = 0;
    config.site_import = 0;
    config.user_site_directory = 0;
    config.write_bytecode = 0;
    config.install_signal_handlers = 1;
    config.optimization_level = launcher_config.optimization;
    config.module_search_paths_set = 1;
#define CHECK(expression) do { status = (expression); if (status_exception(status)) goto init_error; } while (0)
    const wchar_t *identity = paths->exe;
    if (launcher_config.portable && portable_identity(paths->outer)) identity = paths->outer;
    argv[0] = (wchar_t *)identity;
    CHECK(config_argv(&config, argc, argv));
    CHECK(config_string(&config, &config.program_name, identity));
    CHECK(config_string(&config, &config.executable, identity));
    CHECK(config_string(&config, &config.home, paths->internal));
    CHECK(config_string(&config, &config.prefix, paths->internal));
    CHECK(config_string(&config, &config.exec_prefix, paths->internal));
    CHECK(config_string(&config, &config.base_prefix, paths->internal));
    CHECK(config_string(&config, &config.base_exec_prefix, paths->internal));
    CHECK(config_string(&config, &config.run_filename, paths->boot));
    CHECK(list_append(&config.module_search_paths, paths->root));
    CHECK(list_append(&config.module_search_paths, paths->internal));
    CHECK(list_append(&config.module_search_paths, paths->zip));
    CHECK(list_append(&config.module_search_paths, paths->app));
    CHECK(list_append(&config.module_search_paths, paths->lib));
    CHECK(initialize(&config));
    config_clear(&config);
    config_initialized = 0;
#undef CHECK
    PyObject *log_path = unicode_from_wide(paths->log, -1);
    PyObject *bundle_dir = unicode_from_wide(paths->root, -1);
    PyObject *no_stderr = long_from_long(!has_stderr());
    PyObject *error_logging = long_from_long(launcher_config.error_logging);
    if (!log_path || !bundle_dir || !no_stderr || !error_logging || sys_set("_coil_error_log", log_path) < 0 ||
        sys_set("_coil_error_logging", error_logging) < 0 ||
        sys_set("_coil_bundle_dir", bundle_dir) < 0 ||
        sys_set("_coil_no_stderr", no_stderr) < 0) {
        if (log_path) decref(log_path);
        if (bundle_dir) decref(bundle_dir);
        if (no_stderr) decref(no_stderr);
        if (error_logging) decref(error_logging);
        error_message(paths, "Could not configure application error reporting.");
        finalize();
        goto done;
    }
    decref(log_path);
    decref(bundle_dir);
    decref(no_stderr);
    decref(error_logging);
    if (run_string(prelude, NULL) < 0) {
        error_message(paths, "Could not initialize the application launcher.");
        finalize();
        goto done;
    }
    result = run_main(); /* Handles SystemExit, tracebacks, atexit and flushing. */
    goto done;

init_error:
    if (status_exit(status)) result = status.exitcode;
    else error_message(paths, status.err_msg ? status.err_msg : "Python initialization failed.");
    goto done;
path_error:
    error_message(paths, "The executable or runtime path is too long or invalid.");
done:
    if (config_initialized) config_clear(&config);
    /* Do not unload Python: process detach may still run extension cleanup. */
    free(paths);
    return result;
}

"""Tests for the packager."""

import os
import struct
import shutil
import subprocess
import sys
import textwrap
from pathlib import Path
from unittest.mock import patch, MagicMock
import zipfile

import pytest

from coil.packager import (
    package_bundled,
    package_portable,
    _add_dir_to_zip,
    _configure_pth,
    _remove_py_files,
    _strip_installed_packages,
    _generate_bootstrap_script,
    _get_python_ver_tag,
    _zip_directory,
    _COIL_MAGIC,
)
from coil.bootloader import (
    BOOTLOADER_VERSION,
    BOOTLOADER_ARCH,
    BOOTLOADER_SIZE,
    get_bootloader_stub,
)


def _make_project(base: Path) -> Path:
    """Create a sample project."""
    project = base / "myproject"
    project.mkdir()
    (project / "main.py").write_text('print("hello")\n')
    (project / "helper.py").write_text('def greet():\n    return "hi"\n')
    return project


def _make_runtime(base: Path) -> Path:
    """Create a fake runtime directory."""
    runtime = base / "runtime"
    runtime.mkdir()
    (runtime / "python.exe").write_bytes(b"MZ" + b"\x00" * 200)
    (runtime / "pythonw.exe").write_bytes(b"MZ" + b"\x00" * 200)
    (runtime / "python313.dll").write_bytes(b"fake dll")
    (runtime / "python3.dll").write_bytes(b"fake dll")
    (runtime / "vcruntime140.dll").write_bytes(b"fake dll")
    (runtime / "python313._pth").write_text("python313.zip\n.\n")
    (runtime / "python313.zip").write_bytes(b"fake zip")
    return runtime


def test_package_bundled(tmp_path: Path):
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    result = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
    )

    assert result.is_dir()
    assert result.name == "MyApp"
    # Bundled uses _internal/ structure
    assert (result / "MyApp.exe").is_file()
    assert (result / "_internal" / "app" / "main.pyc").is_file()
    assert (result / "_internal" / "app" / "helper.pyc").is_file()
    assert (result / "python313.dll").is_file()


def test_package_bundled_secure(tmp_path: Path):
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    result = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
        secure=True,
    )

    assert result.is_dir()
    assert (result / "_internal" / "app" / "main.pyc").is_file()
    from coil.obfuscator import COIL_SOURCE_ARCHIVE
    assert not (result / "_internal" / "app" / COIL_SOURCE_ARCHIVE).exists()


def test_package_bundled_with_deps(tmp_path: Path):
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    deps = tmp_path / "deps"
    deps.mkdir()
    (deps / "somelib.py").write_text("x = 1\n")

    result = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
        deps_dir=deps,
    )

    assert (result / "_internal" / "lib").is_dir()


def test_package_portable(tmp_path: Path):
    """Portable produces a single .exe file (bootloader + zip)."""
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    results = package_portable(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
    )

    assert len(results) == 1
    exe_path = results[0]
    assert exe_path.name == "MyApp.exe"
    assert exe_path.is_file()

    # Should be a SINGLE file, no directory
    assert not (output / "MyApp").is_dir()

    # Verify it starts with MZ (valid PE) and ends with 12-byte COIL trailer
    data = exe_path.read_bytes()
    assert data[:2] == b"MZ"
    # Trailer: [zip_offset:u32][build_hash:u32][magic:u32]
    magic = struct.unpack_from("<I", data, len(data) - 4)[0]
    assert magic == _COIL_MAGIC
    build_hash = struct.unpack_from("<I", data, len(data) - 8)[0]
    zip_offset = struct.unpack_from("<I", data, len(data) - 12)[0]
    assert zip_offset > 0
    assert build_hash != 0


def test_package_portable_gui(tmp_path: Path):
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    results = package_portable(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
        gui=True,
    )

    assert len(results) == 1
    assert results[0].name == "MyApp.exe"
    assert results[0].is_file()


def test_package_portable_multiple_entries(tmp_path: Path):
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    results = package_portable(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py", "helper.py"],
        name="MyApp",
        target_os="windows",
    )

    assert len(results) == 2
    names = {r.name for r in results}
    assert "main.exe" in names
    assert "helper.exe" in names
    # Each should be a single file
    for r in results:
        assert r.is_file()
        data = r.read_bytes()
        assert data[:2] == b"MZ"


def test_zip_directory(tmp_path: Path):
    src = tmp_path / "testdir"
    src.mkdir()
    (src / "a.txt").write_text("hello")
    sub = src / "sub"
    sub.mkdir()
    (sub / "b.txt").write_text("world")

    zip_bytes = _zip_directory(src)

    import io
    with zipfile.ZipFile(io.BytesIO(zip_bytes), "r") as zf:
        names = zf.namelist()
        assert "a.txt" in names
        assert "sub\\b.txt" in names or "sub/b.txt" in names


def test_zip_directory_compression(tmp_path: Path):
    """Verify that compress=True produces DEFLATED entries and smaller output."""
    src = tmp_path / "comptest"
    src.mkdir()
    # Write repetitive data so DEFLATED compresses well
    (src / "data.txt").write_bytes(b"hello world\n" * 1000)

    stored = _zip_directory(src, compress=False)
    deflated = _zip_directory(src, compress=True)

    # DEFLATED should be smaller than STORED for repetitive data
    assert len(deflated) < len(stored)

    # Both should be valid zips with the same content
    import io
    with zipfile.ZipFile(io.BytesIO(deflated), "r") as zf:
        assert "data.txt" in zf.namelist()
        assert zf.read("data.txt") == b"hello world\n" * 1000


def test_zip_directory_default_is_compressed(tmp_path: Path):
    """Verify _zip_directory defaults to compression (compress=True)."""
    src = tmp_path / "deftest"
    src.mkdir()
    (src / "data.txt").write_text("aaaa" * 500)

    zip_bytes = _zip_directory(src)

    import io
    with zipfile.ZipFile(io.BytesIO(zip_bytes), "r") as zf:
        info = zf.infolist()[0]
        assert info.compress_type == zipfile.ZIP_DEFLATED


def test_add_dir_to_zip(tmp_path: Path):
    src = tmp_path / "source"
    src.mkdir()
    (src / "a.txt").write_text("hello")
    sub = src / "sub"
    sub.mkdir()
    (sub / "b.txt").write_text("world")

    zip_path = tmp_path / "test.zip"
    with zipfile.ZipFile(zip_path, "w") as zf:
        _add_dir_to_zip(zf, src, "prefix")

    with zipfile.ZipFile(zip_path, "r") as zf:
        names = zf.namelist()
        assert "prefix/a.txt" in names
        assert "prefix/sub/b.txt" in names


def test_remove_py_files(tmp_path: Path):
    (tmp_path / "code.py").write_text("x = 1\n")
    (tmp_path / "data.txt").write_text("data")

    _remove_py_files(tmp_path)

    assert not (tmp_path / "code.py").exists()
    assert (tmp_path / "data.txt").exists()


def test_strip_installed_packages(tmp_path: Path):
    (tmp_path / "pkg-1.0.dist-info").mkdir()
    (tmp_path / "tests").mkdir()
    (tmp_path / "docs").mkdir()
    (tmp_path / "actual_code.py").write_text("x = 1\n")

    _strip_installed_packages(tmp_path)

    assert not (tmp_path / "pkg-1.0.dist-info").exists()
    assert not (tmp_path / "tests").exists()
    assert not (tmp_path / "docs").exists()
    assert (tmp_path / "actual_code.py").exists()


def test_generate_bootstrap_script():
    script = _generate_bootstrap_script("main.pyc")
    assert "main.pyc" in script
    assert "sys.path" in script
    assert "__main__" in script
    # Should have error handling for missing entry point
    assert "not found" in script.lower() or "isfile" in script
    # Should catch ImportError specifically
    assert "ImportError" in script
    # Should not call sys.exit(0) — that breaks sitecustomize
    lines = [l.strip() for l in script.splitlines()]
    assert "sys.exit(0)" not in lines


def test_get_python_ver_tag(tmp_path: Path):
    (tmp_path / "python313.dll").write_bytes(b"fake")
    assert _get_python_ver_tag(tmp_path) == "313"


def test_get_python_ver_tag_empty(tmp_path: Path):
    assert _get_python_ver_tag(tmp_path) == ""


def test_bootloader_version():
    assert BOOTLOADER_VERSION >= 2
    assert BOOTLOADER_ARCH == "x86_64"
    assert BOOTLOADER_SIZE > 0


def test_bootloader_stub_valid_pe():
    stub = get_bootloader_stub("x86_64")
    assert len(stub) == BOOTLOADER_SIZE
    assert stub[:2] == b"MZ"


def test_bootloader_stub_default_arch():
    stub = get_bootloader_stub()
    assert len(stub) > 0
    assert stub[:2] == b"MZ"


def test_bootloader_stub_unknown_arch():
    import pytest
    with pytest.raises(RuntimeError, match="No bootloader available"):
        get_bootloader_stub("mips64")


def test_package_bundled_optimize_level(tmp_path: Path):
    """Optimize level is passed through to obfuscator."""
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    result = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
        optimize=0,
    )

    # Should succeed with optimize=0
    assert result.is_dir()
    assert (result / "_internal" / "app" / "main.pyc").is_file()


def test_package_bundled_optimize_default(tmp_path: Path):
    """Default optimize: 1 for non-secure, 2 for secure."""
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    # Non-secure default (optimize=None → 1)
    result = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
    )
    assert result.is_dir()

    # Secure default (optimize=None → 2)
    result2 = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py"],
        name="SecApp",
        target_os="windows",
        secure=True,
    )
    assert result2.is_dir()


# ---------------------------------------------------------------------------
# sitecustomize.py behavior tests (issue: bundled-app boilerplate elimination)
# ---------------------------------------------------------------------------


def _setup_fake_bundle(tmp_path: Path) -> tuple[Path, Path]:
    """Create a minimal bundle layout and run _configure_pth against it.

    Returns (root_dir, internal_dir).
    """
    root = tmp_path / "bundle"
    internal = root / "_internal"
    internal.mkdir(parents=True)
    (internal / "lib").mkdir()
    # _configure_pth needs an existing python*._pth to rewrite
    (root / "python313._pth").write_text("python313.zip\n.\n")
    _configure_pth(root, internal, entry_name="app", ver_tag="313")
    return root, internal


def _exec_sitecustomize(internal: Path, extra_globals: dict | None = None) -> dict:
    """Exec the generated sitecustomize.py in an isolated namespace.

    Captures sys.path state BEFORE restoration so callers can inspect the
    additions site.addsitedir() made. Returns the ns dict augmented with
    _captured_sys_path (list) for that purpose, and restores sys.path on exit.
    """
    site_file = internal / "sitecustomize.py"
    source = site_file.read_text()
    ns: dict = {
        "__file__": str(site_file),
        "__name__": "sitecustomize",
    }
    if extra_globals:
        ns.update(extra_globals)
    saved_path = list(sys.path)
    try:
        exec(compile(source, str(site_file), "exec"), ns)
        ns["_captured_sys_path"] = list(sys.path)
    finally:
        sys.path[:] = saved_path
    return ns


def test_sitecustomize_processes_pth_files(tmp_path: Path):
    """A .pth file in _internal/lib must have its paths added to sys.path.

    Regression: pywin32.pth (which lists win32, win32/lib, Pythonwin) was
    ignored because ._pth disables standard site-packages discovery. The
    generated sitecustomize calls site.addsitedir(_internal/lib) to restore
    .pth processing for bundled packages.
    """
    _, internal = _setup_fake_bundle(tmp_path)
    lib = internal / "lib"
    (lib / "win32").mkdir()
    (lib / "win32" / "lib").mkdir()
    (lib / "Pythonwin").mkdir()
    # Emulate pywin32.pth (real file is "win32\nwin32\\lib\nPythonwin\n")
    (lib / "fake_pywin32.pth").write_text("win32\nwin32/lib\nPythonwin\n")

    ns = _exec_sitecustomize(internal)

    normalized = {os.path.normcase(os.path.normpath(p)) for p in ns["_captured_sys_path"]}
    for expected in (lib / "win32", lib / "win32" / "lib", lib / "Pythonwin"):
        key = os.path.normcase(os.path.normpath(str(expected)))
        assert key in normalized, f"{expected} not on sys.path after sitecustomize"


def test_sitecustomize_pth_import_directive_runs(tmp_path: Path):
    """`import X` lines in .pth files must be executed (site.py semantics)."""
    _, internal = _setup_fake_bundle(tmp_path)
    lib = internal / "lib"
    # Create a tiny importable module and a .pth that imports it
    (lib / "pth_probe.py").write_text("PROBE_TAG = 'hit'\n")
    (lib / "probe.pth").write_text("import pth_probe\n")

    ns = _exec_sitecustomize(internal)
    # After sitecustomize runs, pth_probe should have been imported
    assert "pth_probe" in sys.modules
    assert sys.modules["pth_probe"].PROBE_TAG == "hit"
    # Clean up so we don't pollute later tests
    sys.modules.pop("pth_probe", None)


def test_sitecustomize_registers_dll_dirs(tmp_path: Path, monkeypatch):
    """os.add_dll_directory called for dirs with .dll or .pyd files, not others.

    Heuristic: any leaf directory directly containing a .dll or .pyd file is
    registered. Pure-Python dirs are not. Keeps the count bounded.
    """
    _, internal = _setup_fake_bundle(tmp_path)
    lib = internal / "lib"

    # Fixture tree
    (lib / "pywin32_system32").mkdir()
    (lib / "pywin32_system32" / "fake.dll").write_bytes(b"")
    (lib / "pure_python").mkdir()
    (lib / "pure_python" / "foo.py").write_text("")
    (lib / "with_pyd").mkdir()
    (lib / "with_pyd" / "ext.pyd").write_bytes(b"")
    (lib / "nested").mkdir()
    (lib / "nested" / "deeper").mkdir()
    (lib / "nested" / "deeper" / "buried.dll").write_bytes(b"")

    registered: list[str] = []

    def fake_add_dll_directory(d):
        registered.append(os.path.normcase(os.path.normpath(d)))

        class _Cookie:
            def close(self):
                pass
        return _Cookie()

    # raising=False so the attribute is added on non-Windows platforms too
    monkeypatch.setattr(os, "add_dll_directory", fake_add_dll_directory, raising=False)

    _exec_sitecustomize(internal)

    def _key(p: Path) -> str:
        return os.path.normcase(os.path.normpath(str(p)))

    assert _key(lib / "pywin32_system32") in registered
    assert _key(lib / "with_pyd") in registered
    assert _key(lib / "nested" / "deeper") in registered
    assert _key(lib / "pure_python") not in registered
    # Sanity: the test fixture triggers 3 registrations; the heuristic should
    # stay in "handful" territory even as the tree grows.
    assert len(registered) < 10


def test_sitecustomize_no_dll_api_is_safe(tmp_path: Path, monkeypatch):
    """If os.add_dll_directory isn't available (non-Win or old Py), skip cleanly."""
    _, internal = _setup_fake_bundle(tmp_path)
    (internal / "lib" / "something").mkdir()
    (internal / "lib" / "something" / "x.dll").write_bytes(b"")

    # Remove the attribute so the hasattr check is False
    monkeypatch.delattr(os, "add_dll_directory", raising=False)

    # Must not raise
    _exec_sitecustomize(internal)


def _host_boot_name() -> str:
    """Boot script basename that the generated sitecustomize will look for
    when driven by the host python (sys.executable)."""
    return f"_boot_{Path(sys.executable).stem}.py"


@pytest.mark.skipif(sys.platform != "win32", reason="Inno Setup is Windows-specific")
def test_sitecustomize_exits_cleanly_after_boot_script(tmp_path: Path):
    """Clean return from boot script → process exits 0, no stdin/REPL hang.

    Without the sys.exit(0) guard, CPython falls through to stdin after
    initialization and blocks (Inno Setup waituntilterminated). We test the
    guard by running the host python, importing the generated sitecustomize,
    and asserting a clean exit with stdin redirected to DEVNULL.
    """
    _, internal = _setup_fake_bundle(tmp_path)
    (internal / _host_boot_name()).write_text("print('BOOT_OK')\n")

    result = subprocess.run(
        [sys.executable, "-c",
         f"import sys; sys.path.insert(0, r'{internal}'); import sitecustomize"],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=5,
        text=True,
    )
    assert result.returncode == 0, f"stderr={result.stderr!r}"
    assert "BOOT_OK" in result.stdout


@pytest.mark.skipif(sys.platform != "win32", reason="Inno Setup is Windows-specific")
def test_sitecustomize_preserves_nonzero_exit_code(tmp_path: Path):
    """Explicit sys.exit(N) from the boot script must win over the sys.exit(0)
    guard that sitecustomize adds for the clean-return case."""
    _, internal = _setup_fake_bundle(tmp_path)
    (internal / _host_boot_name()).write_text("import sys; sys.exit(3)\n")

    result = subprocess.run(
        [sys.executable, "-c",
         f"import sys; sys.path.insert(0, r'{internal}'); import sitecustomize"],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=5,
    )
    assert result.returncode == 3, f"stderr={result.stderr!r}"

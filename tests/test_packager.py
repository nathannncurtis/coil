"""Tests for the packager."""

import struct
import shutil
from pathlib import Path
from unittest.mock import patch, MagicMock
import zipfile

from coil.packager import (
    package_bundled,
    package_portable,
    _add_dir_to_zip,
    _remove_py_files,
    _strip_installed_packages,
    _generate_bootstrap_script,
    _get_python_ver_tag,
    _zip_directory,
    _COIL_MAGIC,
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

    # Verify it starts with MZ (valid PE) and ends with COIL magic
    data = exe_path.read_bytes()
    assert data[:2] == b"MZ"
    magic = struct.unpack_from("<I", data, len(data) - 4)[0]
    assert magic == _COIL_MAGIC


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


def test_get_python_ver_tag(tmp_path: Path):
    (tmp_path / "python313.dll").write_bytes(b"fake")
    assert _get_python_ver_tag(tmp_path) == "313"


def test_get_python_ver_tag_empty(tmp_path: Path):
    assert _get_python_ver_tag(tmp_path) == ""

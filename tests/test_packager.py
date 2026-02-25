"""Tests for the packager."""

import shutil
from pathlib import Path
from unittest.mock import patch, MagicMock
import zipfile

from coil.packager import (
    package_bundled,
    _add_dir_to_zip,
    _remove_py_files,
    _strip_installed_packages,
    _generate_sfx_stub,
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
    (runtime / "python.exe").write_bytes(b"fake python")
    (runtime / "python313.dll").write_bytes(b"fake dll")
    (runtime / "python313._pth").write_text("python313.zip\n.\n")
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
    assert (result / "runtime" / "python.exe").is_file()
    assert (result / "app" / "main.pyc").is_file()
    assert (result / "app" / "helper.pyc").is_file()
    assert (result / "MyApp.bat").is_file()


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
    assert (result / "app" / "main.pyc").is_file()
    # Secure mode should not have source archive
    from coil.obfuscator import COIL_SOURCE_ARCHIVE
    assert not (result / "app" / COIL_SOURCE_ARCHIVE).exists()


def test_package_bundled_multiple_entry_points(tmp_path: Path):
    project = _make_project(tmp_path)
    runtime = _make_runtime(tmp_path)
    output = tmp_path / "dist"

    result = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=["main.py", "helper.py"],
        name="MyApp",
        target_os="windows",
    )

    assert (result / "main.bat").is_file()
    assert (result / "helper.bat").is_file()


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

    assert (result / "lib").is_dir()


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


def test_generate_sfx_stub():
    stub = _generate_sfx_stub(gui=False)
    assert "@echo off" in stub
    assert "python.exe" in stub
    assert "Expand-Archive" in stub

"""Compiled bytecode must not record the build machine's absolute paths."""

from __future__ import annotations

import marshal
import sys
from pathlib import Path
from types import CodeType

import pytest

from coil.obfuscator import compile_directory
from coil.packager import _remove_py_files


def _filenames(code: CodeType) -> set[str]:
    names = {code.co_filename}
    for const in code.co_consts:
        if isinstance(const, CodeType):
            names |= _filenames(const)
    return names


def _pyc_filenames(pyc: Path) -> set[str]:
    return _filenames(marshal.loads(pyc.read_bytes()[16:]))


def _make_tree(root: Path) -> None:
    (root / "pkg").mkdir(parents=True)
    (root / "app.py").write_text("def f():\n    return 1\n", encoding="utf-8")
    (root / "pkg" / "__init__.py").write_text("", encoding="utf-8")
    (root / "pkg" / "mod.py").write_text("class C:\n    def m(self):\n        return 2\n", encoding="utf-8")


@pytest.mark.parametrize("optimize", [1, 2])
@pytest.mark.parametrize("use_runtime_python", [False, True])
def test_project_pyc_records_relative_paths(tmp_path: Path, optimize: int, use_runtime_python: bool):
    src = tmp_path / "proj"
    out = tmp_path / "out"
    _make_tree(src)
    compile_directory(src, out, optimize=optimize,
                      runtime_python=Path(sys.executable) if use_runtime_python else None)

    recorded = {p.relative_to(out).as_posix(): _pyc_filenames(p) for p in out.rglob("*.pyc")}
    assert recorded["app.pyc"] == {"app.py"}
    assert recorded["pkg/mod.pyc"] == {str(Path("pkg") / "mod.py")}
    for names in recorded.values():
        assert not any(str(tmp_path) in n for n in names)


@pytest.mark.parametrize("use_runtime_python", [False, True])
def test_dependency_pyc_records_relative_paths(tmp_path: Path, use_runtime_python: bool):
    site = tmp_path / "site-packages"
    _make_tree(site)
    _remove_py_files(site, runtime_python=Path(sys.executable) if use_runtime_python else None, optimize=1)

    recorded = {p.relative_to(site).as_posix(): _pyc_filenames(p) for p in site.rglob("*.pyc")}
    assert recorded["app.pyc"] == {"app.py"}
    assert recorded["pkg/mod.pyc"] == {str(Path("pkg") / "mod.py")}
    assert not list(site.rglob("*.py"))

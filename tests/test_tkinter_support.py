"""tkinter is copied from a matching CPython install into the bundle."""
from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

from coil import tkinter_support
from coil.tkinter_support import (find_tkinter_install, missing_message, needs_tkinter,
                                  stage_tkinter)


def _fake_install(root: Path) -> Path:
    files = {
        "Lib/tkinter/__init__.py": "", "Lib/tkinter/ttk.py": "",
        "Lib/tkinter/__pycache__/ttk.pyc": "",
        "DLLs/_tkinter.pyd": "", "DLLs/tcl86t.dll": "", "DLLs/tk86t.dll": "", "DLLs/zlib1.dll": "", "DLLs/_ssl.pyd": "",
        "tcl/tcl8.6/init.tcl": "", "tcl/tk8.6/tk.tcl": "", "tcl/tk8.6/license.terms": "",
        "tcl/tk8.6/demos/widget": "", "tcl/tcl8/8.6/http-2.9.tm": "",
        "tcl/tix8.4.3/Tix.tcl": "", "tcl/tcl86t.lib": "", "tcl/nmake/x.mak": "",
        "python.exe": "",
    }
    for rel, text in files.items():
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return root


def test_needs_tkinter():
    assert needs_tkinter({"os", "tkinter"})
    assert needs_tkinter({"customtkinter"})
    assert not needs_tkinter({"os", "json"})


def test_stage_copies_runtime_files_only(tmp_path):
    lib = tmp_path / "lib"
    stage_tkinter(_fake_install(tmp_path / "py"), lib)
    staged = {p.relative_to(lib).as_posix() for p in lib.rglob("*") if p.is_file()}
    assert staged == {
        "tkinter/__init__.py", "tkinter/ttk.py",
        "_tkinter.pyd", "tcl86t.dll", "tk86t.dll", "zlib1.dll",
        "tcl/tcl8.6/init.tcl", "tcl/tk8.6/tk.tcl", "tcl/tk8.6/license.terms",
        "tcl/tcl8/8.6/http-2.9.tm",
    }


def test_finds_matching_install(tmp_path, monkeypatch):
    wrong, right = _fake_install(tmp_path / "a"), _fake_install(tmp_path / "b")
    monkeypatch.setattr(tkinter_support, "_candidates", lambda version: [wrong, right])
    monkeypatch.setattr(tkinter_support, "_matches", lambda prefix, version: prefix == right)
    assert find_tkinter_install("3.12.4") == right


def test_install_without_tcl_files_is_rejected(tmp_path):
    prefix = _fake_install(tmp_path / "py")
    (prefix / "DLLs" / "_tkinter.pyd").unlink()
    assert not tkinter_support._matches(prefix, (3, 12))


def test_no_install_returns_none_and_message_names_version(monkeypatch):
    monkeypatch.setattr(tkinter_support, "_candidates", lambda version: [])
    assert find_tkinter_install("3.12") is None
    assert "3.12" in missing_message("3.12.4")


@pytest.mark.skipif(sys.platform != "win32", reason="Windows installs")
def test_host_install_matches_its_own_version():
    version = f"{sys.version_info.major}.{sys.version_info.minor}"
    try:
        import tkinter  # noqa: F401
    except ImportError:
        pytest.skip("host Python has no tkinter")
    assert find_tkinter_install(version) is not None


def test_build_fails_clearly_without_install(tmp_path, monkeypatch):
    from coil.builder import build
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text("def f():\n    import tkinter\n")
    monkeypatch.setattr("coil.tkinter_support.find_tkinter_install", lambda version: None)
    with pytest.raises(RuntimeError, match="3.12"):
        build(project, ["main.py"], python_version="3.12", output_dir=str(tmp_path / "dist"))


TK_APP = '''import tkinter
root = tkinter.Tk()
root.update()
root.destroy()
print("TK_OK", tkinter.TkVersion)
'''


@pytest.mark.skipif(
    sys.platform != "win32" or os.environ.get("COIL_RUN_NATIVE_INTEGRATION") != "1",
    reason="Set COIL_RUN_NATIVE_INTEGRATION=1 on Windows to run real executables",
)
@pytest.mark.parametrize("mode", ["bundled", "portable"])
def test_built_tkinter_app_runs(tmp_path, real_runtime, monkeypatch, mode):
    from coil.builder import build
    from coil.packager import _get_python_ver_tag
    tag = _get_python_ver_tag(real_runtime)
    version = f"{tag[0]}.{tag[1:]}"
    if find_tkinter_install(version) is None:
        pytest.skip(f"no CPython {version} with tkinter installed")
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text(TK_APP)
    monkeypatch.setattr("coil.builder.prepare_runtime", lambda **kwargs: real_runtime)
    output = build(project, ["main.py"], mode=mode, python_version=version,
                   output_dir=str(tmp_path / "dist"), name="TkApp")[0]
    exe = output / "TkApp.exe" if mode == "bundled" else output
    env = dict(os.environ, LOCALAPPDATA=str(tmp_path / "local"))
    for variable in ("TCL_LIBRARY", "TK_LIBRARY", "PYTHONHOME", "PYTHONPATH"):
        env.pop(variable, None)
    result = subprocess.run([str(exe)], capture_output=True, text=True, timeout=60, env=env)
    assert result.returncode == 0, result.stderr
    assert result.stdout.startswith("TK_OK")
    if mode == "bundled":
        assert (output / "_internal/lib/tkinter/__init__.pyc").is_file()
        assert not list((output / "_internal/lib/tkinter").glob("*.py"))

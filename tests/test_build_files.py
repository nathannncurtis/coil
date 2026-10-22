"""[build.files]: parsing, validation, copying, and real executables."""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from coil.cli import main
from coil.config import get_build_config, resolve_files
from coil.doctor import _check_config
from coil.inspect import run_inspect
from coil.packager import _copy_extra_files, package_bundled, package_portable


def _project(tmp_path: Path) -> Path:
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text("print('x')\n")
    (tmp_path / "shared").mkdir()
    (tmp_path / "shared" / "a.txt").write_text("A")
    return project


def test_build_config_reads_files_and_profile_replaces():
    raw = {"build": {"files": {"a": "x"}}, "profile": {"p": {"files": {"b": "y"}}}}
    assert get_build_config(raw)["files"] == {"a": "x"}
    assert get_build_config(raw, profile="p")["files"] == {"b": "y"}
    assert get_build_config({})["files"] == {}


def test_files_must_be_a_table():
    with pytest.raises(ValueError, match="table"):
        get_build_config({"build": {"files": ["a"]}})
    with pytest.raises(ValueError, match="table"):
        get_build_config({"profile": {"p": {"files": "a"}}}, profile="p")


def test_resolve_relative_and_absolute_sources(tmp_path):
    project = _project(tmp_path)
    absolute = tmp_path / "shared" / "a.txt"
    out = resolve_files({"assets\\shared": "../shared", "vendor.txt": str(absolute)}, project)
    assert out == [("assets/shared", (tmp_path / "shared").resolve()), ("vendor.txt", absolute.resolve())]


@pytest.mark.parametrize("dest", [
    "", "/abs", "\\abs", "C:/x", "C:x", "../x", "a/../x", "a/..", ".", "./a", "a//b", "a/",
    "a:stream", "con", "a/NUL.txt", "lpt1", "bad.", "bad ", "a/b ", "x|y", "x*", "x?", 'x"y',
    "a\x01", "_internal", "_INTERNAL/x", "//server/share/x",
])
def test_invalid_destinations(tmp_path, dest):
    project = _project(tmp_path)
    with pytest.raises(ValueError, match="build.files"):
        resolve_files({dest: "../shared"}, project)


def test_missing_source_and_bad_source_type(tmp_path):
    project = _project(tmp_path)
    with pytest.raises(ValueError, match="does not exist"):
        resolve_files({"a": "../nope"}, project)
    for bad in (5, "", "  ", None, ["x"]):
        with pytest.raises(ValueError, match="source"):
            resolve_files({"a": bad}, project)


def test_destinations_differing_only_by_case_or_separator_are_duplicates(tmp_path):
    project = _project(tmp_path)
    with pytest.raises(ValueError, match="twice"):
        resolve_files({"A/b": "../shared", "a\\B": "../shared"}, project)


def test_copy_extra_files_copies_to_root_and_app(tmp_path):
    shared = tmp_path / "shared"
    (shared / "sub").mkdir(parents=True)
    (shared / "sub" / "b.txt").write_text("B")
    (shared / "a.txt").write_text("A")
    stage, app = tmp_path / "stage", tmp_path / "stage" / "_internal" / "app"
    app.mkdir(parents=True)
    _copy_extra_files([("assets/shared", shared), ("one.txt", shared / "a.txt")], stage, app)
    for base in (stage, app):
        assert (base / "assets/shared/a.txt").read_text() == "A"
        assert (base / "assets/shared/sub/b.txt").read_text() == "B"
        assert (base / "one.txt").read_text() == "A"


@pytest.mark.parametrize("existing", ["app.exe", "_internal/app/main.pyc", "_internal/python312.zip"])
def test_copy_extra_files_refuses_to_overwrite_bundle_files(tmp_path, existing):
    source = tmp_path / "s.txt"
    source.write_text("S")
    stage = tmp_path / "stage"
    app = stage / "_internal" / "app"
    app.mkdir(parents=True)
    (stage / existing).parent.mkdir(parents=True, exist_ok=True)
    (stage / existing).write_text("ORIGINAL")
    with pytest.raises(ValueError, match="overwrite"):
        _copy_extra_files([(existing, source)], stage, app)
    assert (stage / existing).read_text() == "ORIGINAL"


def test_copy_extra_files_refuses_file_over_directory_and_overlap(tmp_path):
    source = tmp_path / "s.txt"
    source.write_text("S")
    stage = tmp_path / "stage"
    app = stage / "_internal" / "app"
    (stage / "data").mkdir(parents=True)
    app.mkdir(parents=True)
    with pytest.raises(ValueError, match="overwrite"):
        _copy_extra_files([("data", source)], stage, app)
    with pytest.raises(ValueError, match="overwrite"):
        _copy_extra_files([("x", source), ("x", source)], stage, app)


def _write_toml(project: Path, text: str) -> None:
    (project / "coil.toml").write_text(text)


def test_dry_run_lists_files_and_reports_errors(tmp_path, capsys):
    project = _project(tmp_path)
    _write_toml(project, '[build.files]\n"assets/shared" = "../shared"\n')
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--dry-run", "--mode", "bundled"])
    assert stop.value.code == 0
    assert "assets/shared <-" in capsys.readouterr().out

    _write_toml(project, '[build.files]\n"../x" = "../shared"\n')
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--dry-run"])
    assert stop.value.code == 1
    assert "invalid segment" in capsys.readouterr().out


def test_dry_run_profile_files(tmp_path, capsys):
    project = _project(tmp_path)
    _write_toml(project, '[build.files]\n"base" = "../shared"\n[profile.rel.files]\n"rel" = "../shared"\n')
    with pytest.raises(SystemExit):
        main(["build", str(project), "--dry-run", "--profile", "rel"])
    out = capsys.readouterr().out
    assert "rel <-" in out and "base <-" not in out


def test_doctor_and_inspect_validate_files(tmp_path, capsys):
    project = _project(tmp_path)
    _write_toml(project, '[build.files]\n"ok" = "../shared"\n')
    assert any(r.status == "pass" and "build.files" in r.message for r in _check_config(project))
    _write_toml(project, '[build.files]\n"ok" = "../missing"\n')
    assert any(r.status == "fail" and "does not exist" in r.message for r in _check_config(project))
    assert run_inspect(project) == 1
    _write_toml(project, '[build.files]\n"ok" = "../shared"\n')
    run_inspect(project)
    assert "extra file: ok <-" in capsys.readouterr().out


native = pytest.mark.skipif(
    sys.platform != "win32" or os.environ.get("COIL_RUN_NATIVE_INTEGRATION") != "1",
    reason="Set COIL_RUN_NATIVE_INTEGRATION=1 on Windows to run real executables",
)

SOURCE = """import ctypes, importlib, json
from pathlib import Path
root = Path(__file__).resolve().parents[2]
lib = ctypes.WinDLL("vendor.dll")
buf = ctypes.create_unicode_buffer(1024)
ctypes.windll.kernel32.GetModuleFileNameW(ctypes.c_void_p(lib._handle), buf, 1024)
imported = []
for name in ("pydoc_" + "data", "lib2" + "to3", "pyd" + "oc"):
    importlib.import_module(name)
    imported.append(name)
print(json.dumps(dict(
    shared=(root / "assets/shared/a.txt").read_text(),
    app_copy=(Path(__file__).resolve().parent / "assets/shared/a.txt").read_text(),
    dll=buf.value, root=str(root), imported=imported)))
"""


@pytest.fixture(scope="module")
def native_project(tmp_path_factory):
    root = tmp_path_factory.mktemp("build-files")
    project = root / "project"
    project.mkdir()
    (project / "main.py").write_text(SOURCE)
    (root / "shared-assets").mkdir()
    (root / "shared-assets" / "a.txt").write_text("OUTSIDE")
    dll = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "version.dll"
    files = [("assets/shared", (root / "shared-assets").resolve()), ("vendor.dll", dll)]
    return root, project, files


def _check(output: str):
    data = json.loads(output)
    assert data["shared"] == data["app_copy"] == "OUTSIDE"
    assert Path(data["dll"]).parent == Path(data["root"])
    assert data["imported"] == ["pydoc_data", "lib2to3", "pydoc"]
    return data


@native
def test_bundled_build_with_outside_files_and_full_stdlib(native_project, real_runtime, tmp_path):
    _, project, files = native_project
    bundle = package_bundled(project, tmp_path / "dist", real_runtime, ["main.py"], "app", "windows",
                             extra_files=files)
    result = subprocess.run([str(bundle / "app.exe")], capture_output=True, text=True, timeout=60)
    assert result.returncode == 0, result.stderr
    _check(result.stdout)
    assert (bundle / "vendor.dll").is_file()


@native
def test_portable_build_with_outside_files_and_full_stdlib(native_project, real_runtime, tmp_path):
    _, project, files = native_project
    exe = package_portable(project, tmp_path / "dist", real_runtime, ["main.py"], "app", "windows",
                           extra_files=files)[0]
    local, temp = tmp_path / "local", tmp_path / "temp"
    local.mkdir()
    temp.mkdir()
    env = dict(os.environ, LOCALAPPDATA=str(local), TEMP=str(temp), TMP=str(temp))
    result = subprocess.run([str(exe)], capture_output=True, text=True, timeout=120, env=env)
    assert result.returncode == 0, result.stderr
    data = _check(result.stdout)
    assert str(local).lower() in data["root"].lower()


@native
def test_build_fails_when_destination_collides_with_runtime_file(native_project, real_runtime, tmp_path):
    root, project, _ = native_project
    with pytest.raises(ValueError, match="overwrite"):
        package_bundled(project, tmp_path / "dist", real_runtime, ["main.py"], "app", "windows",
                        extra_files=[("app.exe", root / "shared-assets" / "a.txt")])

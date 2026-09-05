"""Regressions for destructive names, resource stamping and package data."""
import json
from pathlib import Path
import struct
import subprocess
import sys

import pytest

from coil.packager import package_bundled


@pytest.mark.parametrize("name", ["../victim", "..", "sub/app", "NUL", "trailing."])
def test_invalid_output_name_fails_before_touching_output(tmp_path, name):
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text("pass")
    sentinel = tmp_path / "victim" / "sentinel.txt"
    sentinel.parent.mkdir()
    sentinel.write_text("keep")
    with pytest.raises(ValueError, match="Invalid application name"):
        package_bundled(project, tmp_path / "dist", tmp_path / "unused-runtime",
                        ["main.py"], name, "windows")
    assert sentinel.read_text() == "keep"
    assert not (tmp_path / "dist").exists()


def test_duplicate_entry_names_fail_before_packaging(tmp_path):
    for directory in ("a", "b"):
        (tmp_path / directory).mkdir()
        (tmp_path / directory / "main.py").write_text("pass")
    with pytest.raises(ValueError, match="distinct Windows executable names"):
        package_bundled(tmp_path, tmp_path / "dist", tmp_path / "unused-runtime",
                        ["a/main.py", "b/main.py"], "App", "windows")
    assert not (tmp_path / "dist").exists()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows executables")
def test_stamped_entries_run_and_package_resources_are_available(tmp_path, real_runtime):
    import pefile
    project = tmp_path / "project"
    project.mkdir()
    source = 'import json, sys; print(json.dumps(sys.argv))\n'
    for entry in ("main.py", "Helper With Space.py"):
        (project / entry).write_text(source)
    # A one-pixel 32-bit ICO, built directly as fixture bytes.
    bitmap = struct.pack("<IIIHHIIIIII", 40, 1, 2, 1, 32, 0, 4, 0, 0, 0, 0)
    bitmap += b"\xff\x00\x00\xff" + b"\0" * 4
    icon = project / "sample.ico"
    icon.write_bytes(struct.pack("<HHH", 0, 1, 1) +
                     struct.pack("<BBBBHHII", 1, 1, 0, 0, 1, 32, len(bitmap), 22) + bitmap)
    bundle = package_bundled(project, tmp_path / "dist", real_runtime,
        ["main.py", "Helper With Space.py"], "Stamped", "windows", icon=str(icon),
        subsystems={"main": "console", "Helper With Space": "gui"},
        versioninfo={name: {"product_name": "Product " + name,
                           "file_description": "Description " + name,
                           "file_version": "1.2.3.4"}
                     for name in ("main", "Helper With Space")})
    for name, subsystem in (("main", 3), ("Helper With Space", 2)):
        exe = bundle / (name + ".exe")
        pe = pefile.PE(str(exe))
        assert pe.OPTIONAL_HEADER.Subsystem == subsystem
        ids = {resource.struct.Id for resource in pe.DIRECTORY_ENTRY_RESOURCE.entries}
        assert {3, 14, 16}.issubset(ids)
        strings = {}
        for group in pe.FileInfo:
            for info in group:
                for table in getattr(info, "StringTable", []):
                    strings.update(table.entries)
        assert strings[b"ProductName"].decode() == "Product " + name
        pe.close()
        result = subprocess.run([str(exe), "--flag", "a b"], capture_output=True,
                                text=True, timeout=15)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout) == [str(exe), "--flag", "a b"]
    assert (bundle / "sample.ico").read_bytes() == icon.read_bytes()
    assert (bundle / "_internal/license.txt").is_file()


@pytest.mark.skipif(sys.platform != "win32", reason="Windows executables")
@pytest.mark.parametrize("flags,expected", [([], {"main": 3, "gui": 2}),
                                         (["--console"], {"main": 3, "gui": 3})])
def test_cli_selects_subsystems_for_every_entry(tmp_path, real_runtime, monkeypatch, flags, expected):
    import pefile
    from coil.cli import main
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text('print("main")')
    (project / "gui.py").write_text('if False: import tkinter\nprint("gui")')
    monkeypatch.setattr("coil.builder.prepare_runtime", lambda **kwargs: real_runtime)
    output = tmp_path / "dist"
    main(["build", str(project), "--entry", "main.py", "--entry", "gui.py",
          "--python", "3.12", "--mode", "bundled", "--name", "App",
          "--output", str(output), *flags])
    for entry, subsystem in expected.items():
        exe = output / "App" / (entry + ".exe")
        pe = pefile.PE(str(exe))
        assert pe.OPTIONAL_HEADER.Subsystem == subsystem
        pe.close()
        result = subprocess.run([str(exe)], capture_output=True, text=True, timeout=15)
        assert result.returncode == 0 and result.stdout.strip() == entry


@pytest.mark.skipif(sys.platform != "win32", reason="Windows executables")
def test_rebuild_in_custom_project_output_does_not_scan_its_own_launchers(tmp_path, real_runtime, monkeypatch):
    from coil.builder import build
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text('print("repeat build")')
    monkeypatch.setattr("coil.builder.prepare_runtime", lambda **kwargs: real_runtime)
    for _ in range(2):
        bundle = build(project, ["main.py"], mode="bundled", python_version="3.12",
                       output_dir=str(project / "releases"), name="App")[0]
        result = subprocess.run([str(bundle / "App.exe")], capture_output=True, text=True, timeout=15)
        assert result.returncode == 0 and result.stdout.strip() == "repeat build"

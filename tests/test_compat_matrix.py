"""Build real third-party-package fixtures with Coil and run the executables."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = [
    pytest.mark.skipif(sys.platform != "win32", reason="Windows executables"),
    pytest.mark.skipif(os.environ.get("COIL_RUN_COMPAT") != "1", reason="Set COIL_RUN_COMPAT=1 to build the compatibility matrix"),
]

COMPAT = Path(__file__).parent / "compat"
FIXTURES = ["numpy", "pillow", "pyqt5", "pyqt6", "tkinter", "pdfplumber",
            "pyautogui", "cryptography", "lxml", "psutil", "plugins"]
PYTHON = "3.12.10"


def _plugin_wheels(tmp_path: Path) -> list[str]:
    wheels = tmp_path / "wheels"
    wheels.mkdir()
    for plugin in sorted((COMPAT / "plugins").glob("plugin_*")):
        copy = shutil.copytree(plugin, tmp_path / "plugin-src" / plugin.name, ignore=shutil.ignore_patterns("build", "*.egg-info", "__pycache__"))
        subprocess.run([sys.executable, "-m", "pip", "wheel", "--no-deps", "--disable-pip-version-check",
                        "-q", "-w", str(wheels), str(copy)], check=True, capture_output=True)
    return [str(wheel) for wheel in sorted(wheels.glob("*.whl"))]


@pytest.mark.parametrize("mode", ["bundled", "portable"])
@pytest.mark.parametrize("fixture", FIXTURES)
def test_fixture_builds_and_runs(fixture, mode, tmp_path, real_runtime, monkeypatch):
    from coil.builder import build

    monkeypatch.setattr("coil.builder.prepare_runtime", lambda **kwargs: real_runtime)
    source = COMPAT / fixture
    project = tmp_path / "project"
    shutil.copytree(source / "app" if fixture == "plugins" else source, project)
    include = _plugin_wheels(tmp_path) if fixture == "plugins" else None
    outputs = build(project, ["main.py"], mode=mode, python_version=PYTHON, output_dir=str(tmp_path / "dist"),
                    name=fixture, include=include)
    exe = outputs[0] / f"{fixture}.exe" if mode == "bundled" else outputs[0]
    assert exe.is_file(), exe

    local = tmp_path / "local"
    local.mkdir()
    environment = dict(os.environ, LOCALAPPDATA=str(local))
    result = subprocess.run([str(exe)], stdin=subprocess.DEVNULL, capture_output=True, text=True,
                            timeout=180, env=environment, cwd=tmp_path)
    assert result.returncode == 0, f"exit={result.returncode}\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
    assert f"COMPAT_OK {fixture}" in result.stdout, f"stdout={result.stdout!r}\nstderr={result.stderr!r}"

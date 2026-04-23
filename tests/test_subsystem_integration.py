"""End-to-end: build the subsystem fixture and verify each exe's PE subsystem.

Uses a real Python runtime (copied from the host interpreter) so target
exes are stampable PEs. Parses produced exes with pefile.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "win32", reason="PE subsystem stamping is Windows-only"
)

pefile = pytest.importorskip("pefile")

from coil.config import get_subsystem_config, load_config
from coil.packager import package_bundled


FIXTURE_DIR = Path(__file__).parent / "fixtures" / "subsystem_sample"


def _read_subsystem(exe_path: Path) -> int:
    pe = pefile.PE(str(exe_path), fast_load=True)
    try:
        return int(pe.OPTIONAL_HEADER.Subsystem)
    finally:
        pe.close()


def test_bundled_build_stamps_subsystem_per_entry(
    tmp_path: Path, monkeypatch, real_runtime: Path
):
    """Full pipeline: 3 entries with different subsystem declarations.

    - main: no explicit subsystem → falls through (gui=False by default
      → console subsystem, via the default python.exe copy path).
    - gui_entry: explicit "gui" → GUI subsystem (2).
    - console_entry: explicit "console" → Console subsystem (3).
    """
    raw = load_config(FIXTURE_DIR)
    assert raw is not None

    entries = ["main.py", "gui_entry.py", "console_entry.py"]
    stems = [Path(e).stem for e in entries]

    subsystems: dict[str, str] = {}
    for stem in stems:
        sub = get_subsystem_config(raw, entry_name=stem)
        if sub is not None:
            subsystems[stem] = sub

    # Only the two explicit entries should be in the dict.
    assert subsystems == {"gui_entry": "gui", "console_entry": "console"}

    # Skip real compilation: we don't need real .pyc files.
    def _fake_obfuscate(project_dir, internal_dir, ui=None, optimize=0, runtime_python=None, skip=None):
        app_dir = internal_dir / "app"
        app_dir.mkdir(parents=True, exist_ok=True)
        for src in project_dir.glob("*.py"):
            if skip is not None and skip(src):
                continue
            (app_dir / (src.stem + ".pyc")).write_bytes(b"")

    monkeypatch.setattr("coil.packager.obfuscate_default", _fake_obfuscate)
    monkeypatch.setattr("coil.packager.obfuscate_secure", _fake_obfuscate)

    output = tmp_path / "dist"

    bundle = package_bundled(
        project_dir=FIXTURE_DIR,
        output_dir=output,
        runtime_dir=real_runtime,
        entry_points=entries,
        name="SubsystemSample",
        target_os="windows",
        subsystems=subsystems,
    )
    assert bundle.is_dir()

    # main.exe: no override; gui=False default → python.exe (Console=3)
    assert _read_subsystem(bundle / "main.exe") == 3
    # gui_entry.exe: explicit "gui" → GUI=2
    assert _read_subsystem(bundle / "gui_entry.exe") == 2
    # console_entry.exe: explicit "console" → Console=3
    assert _read_subsystem(bundle / "console_entry.exe") == 3


def test_bundled_build_explicit_override_beats_gui_flag(
    tmp_path: Path, monkeypatch, real_runtime: Path
):
    """Top-level gui=True says GUI for everything, but an explicit
    "console" override on an entry must still produce Console."""
    entries = ["main.py", "console_entry.py"]

    subsystems = {"console_entry": "console"}

    def _fake_obfuscate(project_dir, internal_dir, ui=None, optimize=0, runtime_python=None, skip=None):
        app_dir = internal_dir / "app"
        app_dir.mkdir(parents=True, exist_ok=True)
        for src in project_dir.glob("*.py"):
            if skip is not None and skip(src):
                continue
            (app_dir / (src.stem + ".pyc")).write_bytes(b"")

    monkeypatch.setattr("coil.packager.obfuscate_default", _fake_obfuscate)
    monkeypatch.setattr("coil.packager.obfuscate_secure", _fake_obfuscate)

    output = tmp_path / "dist"

    bundle = package_bundled(
        project_dir=FIXTURE_DIR,
        output_dir=output,
        runtime_dir=real_runtime,
        entry_points=entries,
        name="SubsystemSample",
        target_os="windows",
        gui=True,
        subsystems=subsystems,
    )

    # main.exe: no override, top-level gui=True → GUI subsystem (2)
    assert _read_subsystem(bundle / "main.exe") == 2
    # console_entry.exe: explicit override beats top-level gui=True → Console (3)
    assert _read_subsystem(bundle / "console_entry.exe") == 3

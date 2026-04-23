"""Tests for set_pe_subsystem on Windows.

Verifies the PE subsystem WORD is written correctly (GUI=2, Console=3),
that flipping does not corrupt other PE bytes, and that invalid string
values raise ValueError.
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "win32", reason="PE subsystem writer is Windows-only"
)

pefile = pytest.importorskip("pefile")

from coil.platforms.windows import set_pe_subsystem


def _copy_python_exe(tmp_path: Path, name: str = "sample.exe") -> Path:
    src = Path(sys.executable)
    dest = tmp_path / name
    shutil.copy2(src, dest)
    return dest


def _read_subsystem(exe_path: Path) -> int:
    pe = pefile.PE(str(exe_path), fast_load=True)
    try:
        return int(pe.OPTIONAL_HEADER.Subsystem)
    finally:
        pe.close()


def test_set_subsystem_to_console(tmp_path: Path):
    exe = _copy_python_exe(tmp_path)
    set_pe_subsystem(exe, "console")
    assert _read_subsystem(exe) == 3


def test_set_subsystem_to_gui(tmp_path: Path):
    exe = _copy_python_exe(tmp_path)
    set_pe_subsystem(exe, "gui")
    assert _read_subsystem(exe) == 2


def test_set_subsystem_bool_backwards_compatible(tmp_path: Path):
    exe = _copy_python_exe(tmp_path)
    set_pe_subsystem(exe, True)
    assert _read_subsystem(exe) == 2
    set_pe_subsystem(exe, False)
    assert _read_subsystem(exe) == 3


def test_flip_flop_does_not_corrupt_other_bytes(tmp_path: Path):
    """Round-tripping console↔gui should only ever change the 2-byte subsystem WORD."""
    exe = _copy_python_exe(tmp_path)
    original = exe.read_bytes()

    for target in ("console", "gui", "console", "gui"):
        set_pe_subsystem(exe, target)

    # End on gui; verify the file is original except for the 2-byte subsystem
    # WORD at pe_offset + 0x5C.
    set_pe_subsystem(exe, "gui")
    after = exe.read_bytes()

    import struct
    pe_offset = struct.unpack_from("<I", original, 0x3C)[0]
    subsystem_offset = pe_offset + 0x5C

    # Bytes before the subsystem WORD are identical.
    assert original[:subsystem_offset] == after[:subsystem_offset]
    # Bytes after the subsystem WORD are identical.
    assert original[subsystem_offset + 2:] == after[subsystem_offset + 2:]
    # The subsystem WORD itself reads as GUI (2).
    assert struct.unpack_from("<H", after, subsystem_offset)[0] == 2


def test_set_subsystem_rejects_invalid_string(tmp_path: Path):
    exe = _copy_python_exe(tmp_path)
    with pytest.raises(ValueError, match="neither"):
        set_pe_subsystem(exe, "neither")


def test_set_subsystem_rejects_empty_string(tmp_path: Path):
    exe = _copy_python_exe(tmp_path)
    with pytest.raises(ValueError):
        set_pe_subsystem(exe, "")

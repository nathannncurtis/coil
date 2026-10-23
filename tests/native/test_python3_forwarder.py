"""Tests for the python3.dll stable-ABI forwarder (spec section 10.1).

The ``python3-forwarder`` cargo bin writes the DLL from
``coil-capi-sys/python3-exports.txt``. These tests compare it with the real
``python3.dll`` from the embeddable distribution CI already downloads: same
export names, the same name-to-ordinal mapping (CPython's file assigns
ordinals in sorted order, which the writer reproduces) and the same
``python312.<name>`` forwarder string for every entry, data exports
included.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import zipfile
from collections.abc import Callable
from pathlib import Path

import pefile
import pytest

ROOT = Path(__file__).resolve().parents[2]
NATIVE = ROOT / "native"
PINNED_ZIP = "python-3.12.10-embed-amd64.zip"

pytestmark = pytest.mark.skipif(
    shutil.which("cargo") is None, reason="writing the forwarder needs cargo"
)


@pytest.fixture(scope="module")
def write_forwarder(tmp_path_factory) -> Callable[[], bytes]:
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "coilc", "--bin", "python3-forwarder"],
        cwd=NATIVE,
        check=True,
    )
    name = "python3-forwarder.exe" if sys.platform == "win32" else "python3-forwarder"
    binary = NATIVE / "target" / "debug" / name
    assert binary.is_file(), binary

    def write() -> bytes:
        out = tmp_path_factory.mktemp("forwarder") / "python3.dll"
        subprocess.run([str(binary), str(out)], check=True)
        return out.read_bytes()

    return write


def _runtime_zip() -> Path | None:
    explicit = os.environ.get("COIL_TEST_RUNTIME_ZIP")
    if explicit:
        archive = Path(explicit)
        if not archive.is_file():
            raise FileNotFoundError(f"COIL_TEST_RUNTIME_ZIP does not exist: {archive}")
        return archive
    cache = Path.home() / ".coil" / "cache" / "runtimes"
    found = sorted(cache.glob(PINNED_ZIP)) if cache.is_dir() else []
    return found[-1] if found else None


@pytest.fixture(scope="module")
def real_pe() -> pefile.PE:
    archive = _runtime_zip()
    if archive is None:
        if os.environ.get("CI"):
            pytest.fail("CI must provide the 3.12.10 embeddable zip for the comparison")
        pytest.skip(f"No cached {PINNED_ZIP} at ~/.coil/cache/runtimes")
    with zipfile.ZipFile(archive) as embedded:
        return pefile.PE(data=embedded.read("python3.dll"))


@pytest.fixture(scope="module")
def forwarder_pe(write_forwarder) -> pefile.PE:
    return pefile.PE(data=write_forwarder())


def test_names_ordinals_and_forwarders_match_the_real_dll(
    forwarder_pe: pefile.PE, real_pe: pefile.PE
) -> None:
    ours = forwarder_pe.DIRECTORY_ENTRY_EXPORT.symbols
    real = real_pe.DIRECTORY_ENTRY_EXPORT.symbols
    assert len(ours) == len(real) == 867
    assert [s.name for s in ours] == [s.name for s in real]
    assert {s.name: s.ordinal for s in ours} == {s.name: s.ordinal for s in real}
    assert {s.name: s.forwarder for s in ours} == {s.name: s.forwarder for s in real}
    # CPython's file forwards all 867 entries, data exports included.
    assert all(s.forwarder for s in ours)
    assert all(s.forwarder for s in real)


def test_dll_headers(forwarder_pe: pefile.PE) -> None:
    pe = forwarder_pe
    # EXECUTABLE_IMAGE | LARGE_ADDRESS_AWARE | DLL, entry point zero: the
    # loader only reads the export table.
    assert pe.FILE_HEADER.Characteristics == 0x2022
    assert pe.OPTIONAL_HEADER.AddressOfEntryPoint == 0
    assert pe.OPTIONAL_HEADER.ImageBase == 0x180000000
    assert [s.Name.rstrip(b"\x00").decode() for s in pe.sections] == [".rdata"]
    directories = pe.OPTIONAL_HEADER.DATA_DIRECTORY
    assert len(directories) == 16
    # No imports, no relocations: the export blob holds RVAs only.
    assert directories[1].VirtualAddress == 0 and directories[1].Size == 0
    assert directories[5].VirtualAddress == 0 and directories[5].Size == 0
    assert not hasattr(pe, "DIRECTORY_ENTRY_IMPORT")
    assert not hasattr(pe, "DIRECTORY_ENTRY_BASERELOC")


def test_export_directory_shape(forwarder_pe: pefile.PE) -> None:
    exports = forwarder_pe.DIRECTORY_ENTRY_EXPORT
    assert exports.name == b"python3.dll"
    assert exports.struct.Base == 1
    assert exports.struct.NumberOfFunctions == exports.struct.NumberOfNames == 867
    symbols = exports.symbols
    names = [s.name for s in symbols]
    assert names == sorted(names)  # the loader binary-searches the name table
    # A forwarder is an export address entry pointing inside the export
    # directory's extent, so the directory size must cover the forwarder
    # strings, not just the directory table.
    directory = forwarder_pe.OPTIONAL_HEADER.DATA_DIRECTORY[0]
    for symbol in symbols:
        assert directory.VirtualAddress <= symbol.address < (
            directory.VirtualAddress + directory.Size
        )


def test_deterministic_output(write_forwarder) -> None:
    assert write_forwarder() == write_forwarder()

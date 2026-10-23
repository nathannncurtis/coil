"""Tests for the ``.pdata``/``.xdata`` unwind tables (spec section 12.4).

A small cargo bin target, ``unwind-test-image``, links a hand-assembled
program with one function per prologue shape (frameless, small
``UWOP_ALLOC_SMALL`` frame, ``UWOP_PUSH_NONVOL`` callee saves,
``UWOP_ALLOC_LARGE``, and a 0x3008-byte probed frame) and prints one
``key=value`` layout fact per line. These tests check the sections, the
exception data directory and every ``RUNTIME_FUNCTION``/``UNWIND_INFO``
byte against those facts with ``pefile``.

On Windows the image is also executed. ``walker`` runs inside a
24-frame-deep call chain through every shape, captures a ``CONTEXT`` and
walks back to ``main`` with ``RtlLookupFunctionEntry`` +
``RtlVirtualUnwind``: every frame's return address must lie inside the
expected function's extent, the looked-up ``RUNTIME_FUNCTION`` must be
exactly that function's, the return address must sit at the reported
stack pointer, that pointer must be 8 (mod 16), and the establisher frame
must equal it minus 8, since every function pushes rbp first. Exit code
42 means the walk verified every frame back to ``main``; 1xx/2xx codes
name the failed check (see the bin's doc comment).
"""

from __future__ import annotations

import shutil
import struct
import subprocess
import sys
from pathlib import Path

import pefile
import pytest

ROOT = Path(__file__).resolve().parents[2]
NATIVE = ROOT / "native"

FUNCTIONS = ["main", "small_fn", "small_saved", "alloc_large", "chain", "walker"]

pytestmark = pytest.mark.skipif(
    shutil.which("cargo") is None, reason="linking the test image needs cargo"
)

# Exit codes the walker reports, for failure messages.
WALK_FAILURES = {
    101: "RtlLookupFunctionEntry returned null for walker's own pc",
    102: "walker's pc lies outside its own RUNTIME_FUNCTION",
    103: "RtlVirtualUnwind reported a handler where none is described",
    110: "the return address is not stored at the reported stack pointer",
    111: "a frame's entry rsp is not 8 (mod 16)",
    112: "EstablisherFrame is not entry rsp minus 8",
    140: "a return address lies outside the expected function's extent",
    130: "RtlLookupFunctionEntry returned null for a return address",
    141: "the looked-up RUNTIME_FUNCTION is not the expected function's",
}


@pytest.fixture(scope="module")
def image_builder() -> Path:
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "coilc", "--bin", "unwind-test-image"],
        cwd=NATIVE,
        check=True,
    )
    name = "unwind-test-image.exe" if sys.platform == "win32" else "unwind-test-image"
    binary = NATIVE / "target" / "debug" / name
    assert binary.is_file(), binary
    return binary


@pytest.fixture(scope="module")
def linked(tmp_path_factory: pytest.TempPathFactory, image_builder: Path):
    exe = tmp_path_factory.mktemp("unwind") / "walk.exe"
    proc = subprocess.run(
        [str(image_builder), str(exe)], capture_output=True, text=True, check=True
    )
    facts = {
        key: int(value)
        for key, value in (line.split("=", 1) for line in proc.stdout.splitlines())
    }
    data = exe.read_bytes()
    return data, facts, pefile.PE(data=data)


def test_exception_directory_and_sections(linked) -> None:
    data, facts, pe = linked
    directories = pe.OPTIONAL_HEADER.DATA_DIRECTORY
    assert (directories[3].VirtualAddress, directories[3].Size) == (
        facts["pdata_rva"],
        facts["pdata_size"],
    )

    by_name = {s.Name.rstrip(b"\x00").decode(): s for s in pe.sections}
    assert [n for n in by_name] == [".text", ".data", ".pdata", ".xdata", ".idata", ".reloc"]
    for name, rva_fact, size_fact in [
        (".pdata", "pdata_rva", "pdata_size"),
        (".xdata", "xdata_rva", "xdata_size"),
    ]:
        section = by_name[name]
        assert section.Characteristics == 0x40000040  # INITIALIZED_DATA | READ
        assert section.VirtualAddress == facts[rva_fact]
        assert section.Misc_VirtualSize == facts[size_fact]
        assert section.VirtualAddress % 0x1000 == 0
        assert section.SizeOfRawData % 0x200 == 0
    # One RUNTIME_FUNCTION per function.
    assert facts["pdata_size"] == 12 * len(FUNCTIONS)


def test_runtime_functions_match_the_functions(linked) -> None:
    _, facts, pe = linked
    raw = pe.get_data(facts["pdata_rva"], facts["pdata_size"])
    entries = [
        struct.unpack_from("<III", raw, index * 12) for index in range(len(raw) // 12)
    ]
    assert len(entries) == len(FUNCTIONS)
    # Sorted by BeginAddress, as RtlLookupFunctionEntry binary-searches.
    begins = [entry[0] for entry in entries]
    assert begins == sorted(begins)

    expected = sorted(
        (facts[f"{name}_rva"], facts[f"{name}_rva"] + facts[f"{name}_size"])
        for name in FUNCTIONS
    )
    assert [(entry[0], entry[1]) for entry in entries] == expected
    # The text symbols land exactly at the extents the tables describe.
    for name in FUNCTIONS[1:]:
        assert facts[f"sym_{name}"] == facts[f"{name}_rva"]

    # Unwind data addresses stay inside .xdata, four-aligned, in .pdata
    # order, exactly the per-entry padded lengths apart.
    assert entries[0][2] == facts["xdata_rva"]
    cursor = facts["xdata_rva"]
    for entry in entries:
        assert entry[2] == cursor
        assert entry[2] % 4 == 0
        info = pe.get_data(entry[2], 8)
        slots = info[2]
        cursor += 4 + 2 * slots + 2 * (slots % 2)
    assert cursor == facts["xdata_rva"] + facts["xdata_size"]


def test_unwind_info_headers_and_prologues(linked) -> None:
    data, facts, pe = linked
    raw = pe.get_data(facts["pdata_rva"], facts["pdata_size"])
    xdata = pe.get_data(facts["xdata_rva"], facts["xdata_size"])
    order = sorted(FUNCTIONS, key=lambda name: facts[f"{name}_rva"])
    at = 0
    for name in order:
        begin, _end, unwind_at = struct.unpack_from("<III", raw, 12 * order.index(name))
        assert unwind_at == facts["xdata_rva"] + at
        version_flags, prologue_size, slots, frame = xdata[at : at + 4]
        assert version_flags == 0x01  # version 1, no handler flags
        assert frame == 0x05  # FrameRegister = rbp, FrameOffset = 0
        assert prologue_size == facts[f"{name}_prologue"]
        assert slots > 0
        # Every function starts with push rbp; mov rbp, rsp.
        text_at = pe.get_offset_from_rva(begin)
        assert data[text_at : text_at + 4] == b"\x55\x48\x89\xE5"
        at += 4 + 2 * slots + 2 * (slots % 2)
    assert at == facts["xdata_size"]


def test_walker_chain_covers_every_shape(linked) -> None:
    # The linked image really contains the deep chain: chain calls itself
    # DEPTH times and walker's probed frame is the largest function.
    _, facts, _ = linked
    assert facts["depth"] == 24
    # walker carries the probe loop (23 bytes) plus the imm32 sub and its
    # three pushes, so its prologue is the longest of every shape.
    assert facts["walker_prologue"] >= 39
    assert facts["walker_prologue"] == max(
        facts[f"{name}_prologue"] for name in FUNCTIONS
    )
    assert facts["chain_size"] < facts["walker_size"]


@pytest.mark.skipif(sys.platform != "win32", reason="needs the Windows loader")
@pytest.mark.parametrize("attempt", [1, 2])
def test_unwind_walk_reaches_main(linked, tmp_path: Path, attempt: int) -> None:
    data, _, _ = linked
    # Two runs land on two independent ASLR bases: the walk must hold at
    # any load address, since the expectation table is base-relocated and
    # every address the walker compares comes from RtlLookupFunctionEntry.
    exe = tmp_path / f"walk-{attempt}.exe"
    exe.write_bytes(data)
    result = subprocess.run([str(exe)], timeout=30)
    assert result.returncode == 42, (
        f"run {attempt}: exit {result.returncode}: "
        f"{WALK_FAILURES.get(result.returncode, 'unknown failure')}"
    )

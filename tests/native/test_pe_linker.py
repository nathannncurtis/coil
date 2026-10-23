"""Tests for the PE32+ linker core (spec sections 12.2 and 13.3).

A small cargo bin target, ``link-test-image``, links a fixed
hand-assembled program (bytes written in the test binary, no encoder) and
prints one ``key=value`` layout fact per line. These tests build it, link
the image and check headers, sections, imports and relocations with
``pefile``. On Windows the image is also executed: it reads its load base
from the PEB and compares it with the link-time ``IMAGE_BASE``, so its
exit code reports where it landed. 42 means the loader rebased the image
and the ``Abs64``-relocated pointer still dereferenced the argument for
``kernel32!ExitProcess``; a wrong ``.reloc`` under ASLR faults instead of
exiting 42. 43 means the image still sits at the preferred 0x140000000,
where the statically written pointer must be the correct one. Both runs
assert exit code 42 as the acceptance text specifies: the rebased run as
linked (so 42 proves the base moved), and the default-base run with the
ASLR bits cleared and the preferred-base flag immediate patched to 0 in
its own copy — a second, unpatched-flag copy of that same image still
exits 43, proving the loader honored the preferred base. ``--no-constants`` links a variant without
``.rdata`` content to check the linker omits empty sections.
"""

from __future__ import annotations

import hashlib
import shutil
import struct
import subprocess
import sys
from pathlib import Path

import pefile
import pytest

ROOT = Path(__file__).resolve().parents[2]
NATIVE = ROOT / "native"

pytestmark = pytest.mark.skipif(
    shutil.which("cargo") is None, reason="linking the test image needs cargo"
)

DIR64 = 10
DYNAMIC_BASE = 0x0040
HIGH_ENTROPY_VA = 0x0020


@pytest.fixture(scope="module")
def image_linker() -> Path:
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "coilc", "--bin", "link-test-image"],
        cwd=NATIVE,
        check=True,
    )
    name = "link-test-image.exe" if sys.platform == "win32" else "link-test-image"
    binary = NATIVE / "target" / "debug" / name
    assert binary.is_file(), binary
    return binary


def link_image(linker: Path, out: Path, *flags: str) -> tuple[bytes, dict[str, int]]:
    proc = subprocess.run(
        [str(linker), str(out), *flags], capture_output=True, text=True, check=True
    )
    facts = {
        key: int(value)
        for key, value in (line.split("=", 1) for line in proc.stdout.splitlines())
    }
    return out.read_bytes(), facts


def read32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def link_console(tmp_path: Path, image_linker: Path) -> tuple[bytes, dict[str, int], pefile.PE]:
    data, facts = link_image(image_linker, tmp_path / "console.exe")
    return data, facts, pefile.PE(data=data)


def test_headers(tmp_path: Path, image_linker: Path) -> None:
    data, facts, pe = link_console(tmp_path, image_linker)
    assert data[:2] == b"MZ"
    assert read32(data, 0x3C) == 0x80  # e_lfanew
    assert b"This program cannot be run in DOS mode" in data[0x40:0x80]
    assert data[0x80:0x84] == b"PE\x00\x00"

    header = pe.FILE_HEADER
    assert header.Machine == pefile.MACHINE_TYPE["IMAGE_FILE_MACHINE_AMD64"]
    assert header.NumberOfSections == 5
    assert header.SizeOfOptionalHeader == 240
    assert header.Characteristics & pefile.IMAGE_CHARACTERISTICS["IMAGE_FILE_EXECUTABLE_IMAGE"]
    assert header.Characteristics & pefile.IMAGE_CHARACTERISTICS["IMAGE_FILE_LARGE_ADDRESS_AWARE"]
    assert header.TimeDateStamp != 0

    optional = pe.OPTIONAL_HEADER
    assert optional.Magic == pefile.OPTIONAL_HEADER_MAGIC_PE_PLUS
    assert optional.ImageBase == 0x140000000
    assert optional.SectionAlignment == 0x1000
    assert optional.FileAlignment == 0x200
    assert optional.AddressOfEntryPoint == facts["entry_rva"]
    assert optional.BaseOfCode == facts["text_rva"]
    assert optional.SizeOfCode == 0x200
    assert optional.SizeOfUninitializedData == facts["bss_size"]
    assert optional.SizeOfStackReserve == 16 * 1024 * 1024
    assert optional.SizeOfStackCommit == 0x1000
    assert optional.SizeOfHeapReserve == 0x100000
    assert optional.SizeOfHeapCommit == 0x1000
    assert optional.CheckSum == 0
    assert optional.NumberOfRvaAndSizes == 16
    assert optional.SizeOfImage == facts["size_of_image"]
    assert optional.SizeOfHeaders == pe.sections[0].PointerToRawData
    assert optional.Subsystem == pefile.SUBSYSTEM_TYPE["IMAGE_SUBSYSTEM_WINDOWS_CUI"]

    dll_characteristics = optional.DllCharacteristics
    for bit in (
        pefile.DLL_CHARACTERISTICS["IMAGE_DLLCHARACTERISTICS_DYNAMIC_BASE"],
        pefile.DLL_CHARACTERISTICS["IMAGE_DLLCHARACTERISTICS_HIGH_ENTROPY_VA"],
        pefile.DLL_CHARACTERISTICS["IMAGE_DLLCHARACTERISTICS_NX_COMPAT"],
        pefile.DLL_CHARACTERISTICS["IMAGE_DLLCHARACTERISTICS_TERMINAL_SERVER_AWARE"],
    ):
        assert dll_characteristics & bit
    assert dll_characteristics == (
        DYNAMIC_BASE | HIGH_ENTROPY_VA | 0x0100 | 0x8000
    )


def test_data_directories(tmp_path: Path, image_linker: Path) -> None:
    _, facts, pe = link_console(tmp_path, image_linker)
    directories = pe.OPTIONAL_HEADER.DATA_DIRECTORY
    assert len(directories) == 16
    assert (directories[1].VirtualAddress, directories[1].Size) == (
        facts["idata_rva"],
        facts["idata_size"],
    )
    assert (directories[5].VirtualAddress, directories[5].Size) == (
        facts["reloc_rva"],
        facts["reloc_size"],
    )
    assert (directories[12].VirtualAddress, directories[12].Size) == (
        facts["iat_rva"],
        facts["iat_size"],
    )
    # .rsrc and .pdata/.xdata are separate tasks; everything else stays zero.
    empty = [0, 2, 3, 4, 6, 7, 8, 9, 10, 11, 13, 14, 15]
    for index in empty:
        assert directories[index].VirtualAddress == 0
        assert directories[index].Size == 0


def test_sections(tmp_path: Path, image_linker: Path) -> None:
    _, facts, pe = link_console(tmp_path, image_linker)
    expected = [
        (".text", 0x60000020),
        (".rdata", 0x40000040),
        (".data", 0xC0000040),
        (".idata", 0xC0000040),
        (".reloc", 0x42000040),
    ]
    assert [s.Name.rstrip(b"\x00").decode() for s in pe.sections] == [n for n, _ in expected]
    for section, (name, characteristics) in zip(pe.sections, expected):
        assert section.Characteristics == characteristics, name
        assert section.VirtualAddress % 0x1000 == 0
        assert section.SizeOfRawData % 0x200 == 0
        assert section.PointerToRawData % 0x200 == 0
    assert [s.VirtualAddress for s in pe.sections] == sorted(s.VirtualAddress for s in pe.sections)

    assert pe.sections[0].VirtualAddress == facts["text_rva"]
    assert pe.sections[1].VirtualAddress == facts["rdata_rva"]
    data = pe.sections[2]
    assert data.VirtualAddress == facts["data_rva"]
    # .bss rides in .data's virtual extent beyond its file bytes.
    assert data.Misc_VirtualSize == facts["data_vsize"]
    assert data.SizeOfRawData == facts["data_raw"]
    assert facts["data_vsize"] == facts["bss_offset"] + facts["bss_size"]
    assert facts["scratch"] == facts["data_rva"] + facts["bss_offset"]
    assert data.Misc_VirtualSize > data.SizeOfRawData - 0x200 + 1  # bss beyond raw content


def test_imports(tmp_path: Path, image_linker: Path) -> None:
    _, facts, pe = link_console(tmp_path, image_linker)
    entries = pe.DIRECTORY_ENTRY_IMPORT
    assert [e.dll for e in entries] == [b"kernel32.dll", b"user32.dll"]
    expected_functions = [b"ExitProcess", b"GetCurrentProcessId"], [b"GetDesktopWindow"]

    for entry, functions in zip(entries, expected_functions):
        assert [i.name for i in entry.imports] == functions
        for imp in entry.imports:
            assert imp.hint == 0
            # PE/COFF requires word-aligned hint/name entries; the DLL name
            # strings leave odd parity behind them, so this is a real check.
            assert imp.hint_name_table_rva % 2 == 0
            # ILT and IAT agree before the loader rewrites the IAT.
            count = len(entry.imports) + 1
            ilt = pe.get_data(entry.struct.OriginalFirstThunk, 8 * count)
            iat = pe.get_data(entry.struct.FirstThunk, 8 * count)
            assert ilt == iat
            assert ilt[-8:] == b"\x00" * 8  # null terminator
            for value in struct.unpack_from(f"<{count - 1}Q", ilt):
                assert value & (1 << 63) == 0  # by name, not ordinal
            # The hint/name entry: zero hint, the name, a NUL, then one
            # zero byte when 2 + len + 1 is odd.
            pad = (len(imp.name) + 1) % 2
            hint_name = pe.get_data(imp.hint_name_table_rva, 2 + len(imp.name) + 1 + pad)
            assert hint_name == b"\x00\x00" + imp.name + b"\x00" + b"\x00" * pad

    kernel32 = entries[0]
    assert kernel32.imports[0].address == 0x140000000 + facts["iat_exit_process_rva"]
    # The IAT data directory spans exactly the thunk arrays.
    assert facts["iat_rva"] == kernel32.struct.FirstThunk
    assert facts["iat_size"] == (2 + 1 + 1 + 1) * 8


def test_relocations(tmp_path: Path, image_linker: Path) -> None:
    data, facts, pe = link_console(tmp_path, image_linker)
    expected = [facts["rdata_slot"], facts["ptr_slot"], facts["func_ptr"]]
    blocks = pe.DIRECTORY_ENTRY_BASERELOC
    seen = sorted(
        entry.rva
        for block in blocks
        for entry in block.entries
        if entry.type == DIR64  # pefile also lists the type-0 padding entries
    )
    assert seen == expected
    for block in blocks:
        fixups = [entry for entry in block.entries if entry.type != 0]
        padding = [entry for entry in block.entries if entry.type == 0]
        assert all(entry.type == DIR64 for entry in fixups)
        assert len(padding) <= 1
        page = block.struct.VirtualAddress
        size = block.struct.SizeOfBlock
        assert page % 0x1000 == 0
        assert size % 4 == 0
        assert size == 8 + 2 * len(block.entries) + 2 * (len(block.entries) % 2)

    # The exact .reloc bytes: page blocks in RVA order, entries sorted,
    # zero-padded to a multiple of four.
    rebuilt = bytearray()
    for page in sorted({rva & ~0xFFF for rva in expected}):
        offsets = sorted(rva & 0xFFF for rva in expected if rva & ~0xFFF == page)
        rebuilt += struct.pack("<II", page, 8 + 2 * len(offsets) + 2 * (len(offsets) % 2))
        for offset in offsets:
            rebuilt += struct.pack("<H", (DIR64 << 12) | offset)
        if len(offsets) % 2:
            rebuilt += b"\x00\x00"
    reloc = pe.sections[4]
    assert data[reloc.PointerToRawData : reloc.PointerToRawData + reloc.SizeOfRawData][
        : len(rebuilt)
    ] == rebuilt

    # The static absolute addresses the loader will relocate.
    assert struct.unpack("<Q", pe.get_data(facts["rdata_slot"], 8))[0] == (
        0x140000000 + facts["msg"]
    )
    assert struct.unpack("<Q", pe.get_data(facts["ptr_slot"], 8))[0] == (
        0x140000000 + facts["value"]
    )
    assert struct.unpack("<Q", pe.get_data(facts["func_ptr"], 8))[0] == (
        0x140000000 + facts["text_cont"]
    )


def test_rel32_displacements(tmp_path: Path, image_linker: Path) -> None:
    _, facts, pe = link_console(tmp_path, image_linker)
    text = pe.sections[0]
    raw = pe.get_data(text.VirtualAddress, text.Misc_VirtualSize)
    assert raw[0:4] == b"\x48\x83\xEC\x28"  # sub rsp, 0x28

    def disp32(at: int) -> int:
        return struct.unpack_from("<i", raw, at)[0]

    # jmp probe: rel32 at 0x05, next instruction at 0x09.
    assert raw[0x04] == 0xE9
    assert disp32(0x05) == facts["probe"] - (facts["text_rva"] + 0x09)
    # lea rcx, [rip+disp]: rel32 at 0x0D targeting msg + 2 (the addend).
    assert raw[0x0A:0x0D] == b"\x48\x8D\x0D"
    assert disp32(0x0D) == facts["msg"] + 2 - (facts["text_rva"] + 0x11)
    # mov rax, [rip+disp]: rel32 at 0x14 targeting ptr_slot in .data.
    assert raw[0x11:0x14] == b"\x48\x8B\x05"
    assert disp32(0x14) == facts["ptr_slot"] - (facts["text_rva"] + 0x18)
    # mov ecx, [rax]; add ecx, r8d; add ecx, 40 — the exit code is the
    # dereferenced value plus 40 plus the base-probe flag.
    assert raw[0x18:0x20] == b"\x8B\x08\x41\x03\xC8\x83\xC1\x28"
    # call [rip+disp]: rel32 at 0x22 resolving to the IAT slot.
    assert raw[0x20:0x22] == b"\xFF\x15"
    assert disp32(0x22) == facts["iat_exit_process_rva"] - (facts["text_rva"] + 0x26)
    # The base probe: mov rax, gs:[0x60] (the PEB), mov rax, [rax+0x10]
    # (ImageBaseAddress), movabs rcx, IMAGE_BASE, cmp rax, rcx.
    assert raw[0x28:0x31] == b"\x65\x48\x8B\x04\x25\x60\x00\x00\x00"
    assert raw[0x31:0x35] == b"\x48\x8B\x40\x10"
    assert raw[0x35:0x37] == b"\x48\xB9"
    assert struct.unpack_from("<Q", raw, 0x37)[0] == 0x140000000
    assert raw[0x3F:0x42] == b"\x48\x39\xC8"
    # r8d = 1 at the preferred base (the immediate the default-base test
    # patches to 0), jne rebased (skip the r8d = 0 store), and the two
    # short jumps back to the dereference block.
    assert raw[0x42:0x48] == b"\x41\xB8\x01\x00\x00\x00"
    assert raw[facts["flag_offset"]] == 1  # pinned: the patchable flag byte
    assert raw[0x48:0x4A] == b"\x75\x02"
    assert raw[0x4A] == 0xEB and raw[0x4B] == (0x0A - 0x4C) & 0xFF
    assert raw[0x4C:0x54] == b"\x41\xB8\x00\x00\x00\x00" + bytes(
        [0xEB, (0x0A - 0x54) & 0xFF]
    )


def test_deterministic_output_and_stamp(tmp_path: Path, image_linker: Path) -> None:
    first, _ = link_image(image_linker, tmp_path / "first.exe")
    second, _ = link_image(image_linker, tmp_path / "second.exe")
    assert first == second

    stamp_offset = read32(first, 0x3C) + 4 + 4  # signature + Machine/NumberOfSections
    stamp = read32(first, stamp_offset)
    zeroed = bytearray(first)
    zeroed[stamp_offset : stamp_offset + 4] = b"\x00\x00\x00\x00"
    digest = hashlib.sha256(zeroed).digest()
    assert struct.unpack(">I", digest[:4])[0] == stamp


def test_subsystem_gui(tmp_path: Path, image_linker: Path) -> None:
    data, facts = link_image(image_linker, tmp_path / "gui.exe", "--gui")
    pe = pefile.PE(data=data)
    assert pe.OPTIONAL_HEADER.Subsystem == pefile.SUBSYSTEM_TYPE["IMAGE_SUBSYSTEM_WINDOWS_GUI"]
    assert pe.OPTIONAL_HEADER.AddressOfEntryPoint == facts["entry_rva"]


@pytest.mark.skipif(sys.platform != "win32", reason="needs the Windows loader")
def test_runs_at_default_image_base(tmp_path: Path, image_linker: Path) -> None:
    data, facts, pe = link_console(tmp_path, image_linker)
    # Clear DYNAMIC_BASE and HIGH_ENTROPY_VA so the loader maps the image at
    # its preferred 0x140000000 and applies no relocations: the static
    # addresses the linker wrote must be correct on their own. Two copies
    # run. The first also zeroes the preferred-base flag immediate, so the
    # dereferenced value plus 40 plus 0 exits 42 — the acceptance text's
    # exit code, at the default image base. The second keeps the flag: it
    # exits 43 only because the PEB probe saw 0x140000000, proving these
    # runs really landed at the preferred base and not a silent rebase.
    offset = pe.DOS_HEADER.e_lfanew + 4 + 20 + 70
    flag_at = pe.sections[0].PointerToRawData + facts["flag_offset"]
    assert data[flag_at] == 1  # the `mov r8d, imm32` low byte

    def cleared_bits(flag: int) -> bytes:
        patched = bytearray(data)
        struct.pack_into(
            "<H",
            patched,
            offset,
            pe.OPTIONAL_HEADER.DllCharacteristics & ~(DYNAMIC_BASE | HIGH_ENTROPY_VA),
        )
        patched[flag_at] = flag
        return bytes(patched)

    assert pefile.PE(data=cleared_bits(0)).OPTIONAL_HEADER.DllCharacteristics & 0x0060 == 0

    acceptance = tmp_path / "default-base.exe"
    acceptance.write_bytes(cleared_bits(0))
    result = subprocess.run([str(acceptance)], timeout=30)
    assert result.returncode == 42

    proof = tmp_path / "default-base-flag.exe"
    proof.write_bytes(cleared_bits(1))
    result = subprocess.run([str(proof)], timeout=30)
    assert result.returncode == 43


@pytest.mark.skipif(sys.platform != "win32", reason="needs the Windows loader")
def test_runs_rebased_by_aslr(tmp_path: Path, image_linker: Path) -> None:
    # As linked (DYNAMIC_BASE | HIGH_ENTROPY_VA) the loader picks a random
    # base and must apply the DIR64 fixups; the program dereferences the
    # Abs64'ed pointer before calling ExitProcess, so a wrong .reloc
    # crashes with 0xC0000005. The exit code proves the rebase happened:
    # the program only exits 42 when the PEB probe found a load base other
    # than 0x140000000 — had the loader kept the preferred base, the same
    # dereference would exit 43 and this assertion would fail. Two runs
    # land on two independent random bases.
    for attempt in (1, 2):
        exe = tmp_path / f"rebased-{attempt}.exe"
        link_image(image_linker, exe)
        result = subprocess.run([str(exe)], timeout=30)
        assert result.returncode == 42, f"run {attempt}: {result.returncode}"


def test_empty_sections_are_omitted(tmp_path: Path, image_linker: Path) -> None:
    # The --no-constants variant has no .rdata content and nothing that
    # could name an .rdata symbol, so the image must not carry a zero-size
    # .rdata header whose RVA collides with .data's.
    data, facts = link_image(
        image_linker, tmp_path / "no-constants.exe", "--no-constants"
    )
    pe = pefile.PE(data=data)
    names = [section.Name.rstrip(b"\0").decode() for section in pe.sections]
    assert names == [".text", ".data", ".idata", ".reloc"]
    previous = None
    for section in pe.sections:
        assert section.Misc_VirtualSize > 0, section.Name
        if previous is not None:
            assert section.VirtualAddress >= (
                previous.VirtualAddress + previous.Misc_VirtualSize
            )
        previous = section
    # The remaining layout facts still hold: .data takes .rdata's old page.
    assert facts["data_rva"] == facts["rdata_rva"] == 0x2000
    assert pe.OPTIONAL_HEADER.DATA_DIRECTORY[1].VirtualAddress == facts["idata_rva"]

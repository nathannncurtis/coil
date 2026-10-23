"""Tests for the .rsrc section (spec section 13.3).

``rsrc-test-image``, a small cargo bin target, does two things: it dumps
the raw ``VS_VERSIONINFO`` bytes the Rust side builds, and it links a
minimal image carrying icon, VERSIONINFO and manifest resources and prints
one ``key=value`` layout fact per line. The Python side builds the same
``VS_VERSIONINFO`` bytes by driving the real ``set_version_info`` with a
fake ``ctypes.windll`` that captures the ``UpdateResourceW`` payload, and
reads everything back out of the linked image with ``pefile``: the icon
group must match the ``.ico``, the version strings must match the inputs
byte-for-byte with the reference writer, and the manifest must be the
launcher template's bytes.
"""

from __future__ import annotations

import ctypes
import shutil
import struct
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

import pefile
import pytest

ROOT = Path(__file__).resolve().parents[2]
NATIVE = ROOT / "native"
MANIFEST = ROOT / "src" / "coil" / "templates" / "launcher.manifest"

# The reference writer comes from the checked-out sources: the Rust-side
# environment installs only pytest and pefile, and a differential test
# should read the reference from this tree even when a copy of coil is
# installed elsewhere.
sys.path.insert(0, str(ROOT / "src"))

from coil.platforms.windows import set_version_info  # noqa: E402

RT_ICON = 3
RT_GROUP_ICON = 14
RT_VERSION = 16
RT_MANIFEST = 24
LANG_EN_US = 0x0409

pytestmark = pytest.mark.skipif(
    shutil.which("cargo") is None, reason="linking the test image needs cargo"
)


@pytest.fixture(scope="module")
def rsrc_tool() -> Path:
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "coilc", "--bin", "rsrc-test-image"],
        cwd=NATIVE,
        check=True,
    )
    name = "rsrc-test-image.exe" if sys.platform == "win32" else "rsrc-test-image"
    binary = NATIVE / "target" / "debug" / name
    assert binary.is_file(), binary
    return binary


def link_resources(
    tool: Path, out: Path, *args: str
) -> tuple[bytes, dict[str, int]]:
    proc = subprocess.run(
        [str(tool), "link", str(out), *args], capture_output=True, text=True, check=True
    )
    facts = {
        key: int(value)
        for key, value in (line.split("=", 1) for line in proc.stdout.splitlines())
    }
    return out.read_bytes(), facts


def dump_versioninfo(tool: Path, out: Path, *args: str) -> bytes:
    subprocess.run(
        [str(tool), "dump-versioninfo", str(out), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return out.read_bytes()


def resources_of(pe: pefile.PE) -> dict[int, dict[int, dict[int, bytes]]]:
    """Every resource as type -> id -> language -> raw bytes."""
    result: dict[int, dict[int, dict[int, bytes]]] = {}
    for type_entry in pe.DIRECTORY_ENTRY_RESOURCE.entries:
        types = result.setdefault(type_entry.id, {})
        for name_entry in type_entry.directory.entries:
            langs = types.setdefault(name_entry.id, {})
            for lang_entry in name_entry.directory.entries:
                langs[lang_entry.id] = pe.get_data(
                    lang_entry.data.struct.OffsetToData, lang_entry.data.struct.Size
                )
    return result


def read_version_strings(pe: pefile.PE) -> dict[str, str]:
    result: dict[str, str] = {}
    for file_info_list in pe.FileInfo:
        for file_info in file_info_list:
            if getattr(file_info, "Key", b"") == b"StringFileInfo":
                for table in file_info.StringTable:
                    for key, value in table.entries.items():
                        result[key.decode()] = value.decode()
    return result


def _fake_kernel32() -> tuple[SimpleNamespace, list[tuple[int, int, int, bytes]]]:
    """A kernel32 stand-in that captures UpdateResourceW payloads, so the
    real set_version_info can run (and be diffed) on any OS."""
    updates: list[tuple[int, int, int, bytes]] = []

    def begin(_path: str, _delete: bool) -> int:
        return 1

    def update(_handle: int, type_: int, name: int, lang: int, buf, size: int) -> bool:
        updates.append((type_, name, lang, bytes(buf)[:size]))
        return True

    def end(_handle: int, _discard: bool) -> bool:
        return True

    kernel32 = SimpleNamespace(
        BeginUpdateResourceW=begin, UpdateResourceW=update, EndUpdateResourceW=end
    )
    return kernel32, updates


def build_reference_bytes(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, **kwargs: object
) -> bytes:
    """The bytes set_version_info would write as its RT_VERSION resource."""
    kernel32, updates = _fake_kernel32()
    monkeypatch.setattr(ctypes, "windll", SimpleNamespace(kernel32=kernel32), raising=False)
    set_version_info(tmp_path / "app.exe", **kwargs)  # type: ignore[arg-type]
    assert len(updates) == 1
    type_, name, lang, data = updates[0]
    assert (type_, name, lang) == (RT_VERSION, 1, LANG_EN_US)
    return data


CASES = [
    # Every field explicit, plain ASCII.
    dict(
        product_name="Demo",
        file_description="Demo Application",
        file_version="1.2.3.4",
        product_version="1.2.0.0",
        company_name="Acme Corp",
        legal_copyright="Copyright (c) 2026 Acme Corp",
        internal_name="demo-app",
        original_filename="app.exe",
        comments="Built by the platform team",
    ),
    # Non-ASCII in every field, including a supplementary-plane character
    # (a surrogate pair in UTF-16) and non-Latin scripts.
    dict(
        product_name="Ünïcødé アプリ 🚀",
        file_description="Приложение éè",
        file_version="1.2.3.4",
        product_version="0.1.2.3",
        company_name="Firma GmbH & Co. 😀",
        legal_copyright="© 日本語株式会社",
        internal_name="ünïcødé-äpp",
        original_filename="ünïcødé.exe",
        comments="日本語コメント 🎉",
    ),
    # Empty description and internal name are kept as empty strings; empty
    # company, copyright and comments are left out of the table entirely.
    dict(
        product_name="Minimal",
        file_description="",
        file_version="1.0.0.0",
        product_version="1.0.0.0",
        company_name="",
        legal_copyright="",
        internal_name="",
        original_filename="app.exe",
        comments="",
    ),
    # Version quirks: '-' reads as '.', unparsable parts are zero and extra
    # parts are ignored; the description defaults to the product name on
    # both sides (no file_description given).
    dict(
        product_name="Quirks",
        file_version="6-1.2",
        product_version="1.0b2.3",
        original_filename="quirks.exe",
    ),
    # int()'s version grammar: underscores between digits and decimal
    # digits from other scripts parse on both sides.
    dict(
        product_name="IntGrammar",
        file_version="1_2.3",
        product_version="１.１_0",
        original_filename="int-grammar.exe",
    ),
    dict(
        product_name="WideDigits",
        file_version="١٢.３",
        product_version="𝟏𝟐.０_5",
        original_filename="wide-digits.exe",
    ),
]


@pytest.mark.parametrize("case", CASES, ids=lambda case: case["product_name"])
def test_versioninfo_bytes_match_set_version_info(
    tmp_path: Path, rsrc_tool: Path, monkeypatch: pytest.MonkeyPatch, case: dict
) -> None:
    expected = build_reference_bytes(monkeypatch, tmp_path, **case)
    args = [f"{key.replace('_', '-')}={value}" for key, value in case.items()]
    dump = tmp_path / "case.exe"
    assert dump_versioninfo(rsrc_tool, dump, *args) == expected


def test_versioninfo_defaults_match_set_version_info(
    tmp_path: Path, rsrc_tool: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # Only the product name: the reference defaults the description and
    # internal name to it and the original filename to the exe's name. The
    # dump output is named app.exe so the binary's default matches.
    expected = build_reference_bytes(monkeypatch, tmp_path, product_name="OnlyName")
    dump = tmp_path / "app.exe"
    assert dump_versioninfo(rsrc_tool, dump, "product-name=OnlyName") == expected


def test_version_parts_that_cannot_pack_refuse_on_both_sides(
    tmp_path: Path, rsrc_tool: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # The reference combines each part pair with Python's bitwise OR and
    # packs it as one unsigned DWORD; when that overflows, struct.pack
    # fails before anything is written. The Rust side refuses the same
    # input instead of emitting bytes the reference never produces.
    with pytest.raises(struct.error):
        build_reference_bytes(
            monkeypatch, tmp_path, product_name="Range", file_version="65536.0"
        )
    out = tmp_path / "range.bin"
    proc = subprocess.run(
        [
            str(rsrc_tool),
            "dump-versioninfo",
            str(out),
            "product-name=Range",
            "file-version=65536.0",
        ],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0
    assert "file_version" in proc.stderr
    assert not out.exists()


def make_ico() -> tuple[bytes, list[dict], list[bytes]]:
    """A synthetic .ico with two images; returns the file bytes, the
    directory entries and the images."""
    images = [bytes([0x11]) * 128, bytes([0x22]) * 1024]
    entries = [
        dict(width=16, height=16, colors=0, reserved=0, planes=1, bits=32),
        dict(width=32, height=32, colors=8, reserved=0, planes=1, bits=8),
    ]
    out = struct.pack("<HHH", 0, 1, len(entries))
    offset = 6 + 16 * len(entries)
    for entry, image in zip(entries, images):
        out += struct.pack(
            "<BBBBHHII",
            entry["width"],
            entry["height"],
            entry["colors"],
            entry["reserved"],
            entry["planes"],
            entry["bits"],
            len(image),
            offset,
        )
        offset += len(image)
    for image in images:
        out += image
    return out, entries, images


def test_icon_group_and_images(tmp_path: Path, rsrc_tool: Path) -> None:
    ico, entries, images = make_ico()
    ico_path = tmp_path / "app.ico"
    ico_path.write_bytes(ico)
    data, _ = link_resources(
        rsrc_tool, tmp_path / "app.exe", "--icon", str(ico_path), "product-name=IconApp"
    )
    pe = pefile.PE(data=data)
    resources = resources_of(pe)
    assert set(resources) == {RT_ICON, RT_GROUP_ICON, RT_VERSION}

    # Every icon image is an RT_ICON resource under its 1-based ID, byte
    # for byte as the .ico carried it.
    assert sorted(resources[RT_ICON]) == [1, 2]
    for index, image in enumerate(images):
        assert resources[RT_ICON][index + 1][0] == image

    # The group is set_exe_icon's GRPICONDIR: header plus one 14-byte
    # entry per image referencing the RT_ICON IDs.
    expected_group = struct.pack("<HHH", 0, 1, len(entries))
    for index, entry in enumerate(entries):
        expected_group += struct.pack(
            "<BBBBHHIH",
            entry["width"],
            entry["height"],
            entry["colors"],
            entry["reserved"],
            entry["planes"],
            entry["bits"],
            len(images[index]),
            index + 1,
        )
    assert resources[RT_GROUP_ICON][1][0] == expected_group


def test_version_strings_and_fixed_info(tmp_path: Path, rsrc_tool: Path) -> None:
    values = dict(
        product_name="Ünïcødé アプリ",
        file_description="Приложение",
        file_version="1.2.3.4",
        product_version="4.5.6.7",
        company_name="",
        legal_copyright="",
        internal_name="",
        original_filename="app.exe",
        comments="",
    )
    args = [f"{key.replace('_', '-')}={value}" for key, value in values.items()]
    data, _ = link_resources(rsrc_tool, tmp_path / "app.exe", *args)
    pe = pefile.PE(data=data)
    resources = resources_of(pe)

    # The linked RT_VERSION is byte-identical to the dumped one, which the
    # differential tests pin to set_version_info's bytes.
    dump = tmp_path / "dump.exe"
    assert resources[RT_VERSION][1][LANG_EN_US] == dump_versioninfo(
        rsrc_tool, dump, *args
    )

    strings = read_version_strings(pe)
    assert strings["ProductName"] == values["product_name"]
    assert strings["FileDescription"] == values["file_description"]
    assert strings["FileVersion"] == values["file_version"]
    assert strings["ProductVersion"] == values["product_version"]
    assert strings["InternalName"] == values["internal_name"]
    assert strings["OriginalFilename"] == values["original_filename"]
    # Empty company, copyright and comments are not in the table at all.
    assert set(strings) == {
        "FileDescription",
        "FileVersion",
        "InternalName",
        "ProductName",
        "ProductVersion",
        "OriginalFilename",
    }

    fixed = pe.VS_FIXEDFILEINFO[0]
    assert fixed.Signature == 0xFEEF04BD
    assert (fixed.FileVersionMS, fixed.FileVersionLS) == ((1 << 16) | 2, (3 << 16) | 4)
    assert (fixed.ProductVersionMS, fixed.ProductVersionLS) == (
        (4 << 16) | 5,
        (6 << 16) | 7,
    )


def test_fixed_info_reads_int_grammar_versions(tmp_path: Path, rsrc_tool: Path) -> None:
    # Underscores and wide digits parse on the linked path too, and the
    # packed DWORDs are Python's bitwise OR, which keeps a low part past
    # 0xFFFF as long as the combination fits 32 bits.
    data, _ = link_resources(
        rsrc_tool,
        tmp_path / "app.exe",
        "product-name=Grammar",
        "file-version=1_2.3",
        "product-version=0.429821",
    )
    pe = pefile.PE(data=data)
    fixed = pe.VS_FIXEDFILEINFO[0]
    assert (fixed.FileVersionMS, fixed.FileVersionLS) == ((12 << 16) | 3, 0)
    assert fixed.ProductVersionMS == 429821  # (0 << 16) | 429821, no truncation


def test_manifest_from_the_launcher_template(
    tmp_path: Path, rsrc_tool: Path
) -> None:
    data, _ = link_resources(
        rsrc_tool,
        tmp_path / "app.exe",
        "--manifest",
        str(MANIFEST),
        "product-name=ManifestApp",
    )
    pe = pefile.PE(data=data)
    resources = resources_of(pe)
    assert resources[RT_MANIFEST][1][0] == MANIFEST.read_bytes()


def test_rsrc_section_layout_and_determinism(tmp_path: Path, rsrc_tool: Path) -> None:
    ico, _, _ = make_ico()
    ico_path = tmp_path / "app.ico"
    ico_path.write_bytes(ico)
    # original-filename is explicit: it would otherwise default to the
    # output file's name, which is a real input, not noise.
    args = (
        "--icon",
        str(ico_path),
        "--manifest",
        str(MANIFEST),
        "product-name=All",
        "original-filename=app.exe",
        "comments=hello",
    )
    data, facts = link_resources(rsrc_tool, tmp_path / "app.exe", *args)
    pe = pefile.PE(data=data)

    names = [section.Name.rstrip(b"\x00").decode() for section in pe.sections]
    assert names == [".text", ".rsrc"]
    rsrc_section = pe.sections[-1]
    assert rsrc_section.Characteristics == 0x40000040
    assert rsrc_section.VirtualAddress == facts["rsrc_rva"]
    assert rsrc_section.Misc_VirtualSize == facts["rsrc_size"]
    assert rsrc_section.PointerToRawData >= pe.sections[0].PointerToRawData + 0x200

    directory = pe.OPTIONAL_HEADER.DATA_DIRECTORY[2]
    assert (directory.VirtualAddress, directory.Size) == (
        facts["rsrc_rva"],
        facts["rsrc_size"],
    )
    assert (
        pe.OPTIONAL_HEADER.SizeOfImage
        == (facts["rsrc_rva"] + facts["rsrc_size"] + 0xFFF) & ~0xFFF
    )

    again, _ = link_resources(rsrc_tool, tmp_path / "again.exe", *args)
    assert again == data


def test_no_resources_leave_no_rsrc_section(tmp_path: Path, rsrc_tool: Path) -> None:
    data, facts = link_resources(rsrc_tool, tmp_path / "app.exe")
    pe = pefile.PE(data=data)
    assert [s.Name.rstrip(b"\x00").decode() for s in pe.sections] == [".text"]
    directory = pe.OPTIONAL_HEADER.DATA_DIRECTORY[2]
    assert (directory.VirtualAddress, directory.Size) == (0, 0)
    assert facts["rsrc_size"] == 0


def test_bad_icon_is_rejected(tmp_path: Path, rsrc_tool: Path) -> None:
    bad = tmp_path / "bad.ico"
    bad.write_bytes(struct.pack("<HHH", 0, 2, 1) + b"\x00" * 32)  # type 2: cursor
    proc = subprocess.run(
        [str(rsrc_tool), "link", str(tmp_path / "app.exe"), "--icon", str(bad)],
        capture_output=True,
        text=True,
    )
    assert proc.returncode != 0
    assert proc.stderr.strip()

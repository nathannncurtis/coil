"""Tests for the VS_VERSION_INFO writer on Windows.

Validates that set_version_info replaces (not coexists with) the existing
VS_VERSION_INFO resource that python.exe ships with. The roundtrip uses
pefile so the writer's ctypes code isn't trusted to self-validate.
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "win32", reason="VERSIONINFO writer is Windows-only"
)

pefile = pytest.importorskip("pefile")

from coil.platforms.windows import set_version_info


def _copy_python_exe(tmp_path: Path, name: str = "sample.exe") -> Path:
    """Copy the running Python interpreter to a temp path as a realistic PE."""
    src = Path(sys.executable)
    dest = tmp_path / name
    shutil.copy2(src, dest)
    return dest


def _read_version_strings(exe_path: Path) -> dict[str, str]:
    """Parse VS_VERSION_INFO strings out of a PE using pefile."""
    pe = pefile.PE(str(exe_path), fast_load=True)
    pe.parse_data_directories(
        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_RESOURCE"]]
    )
    result: dict[str, str] = {}
    if not hasattr(pe, "FileInfo") or not pe.FileInfo:
        pe.close()
        return result
    for file_info_list in pe.FileInfo:
        for file_info in file_info_list:
            if getattr(file_info, "Key", b"") == b"StringFileInfo":
                for st in file_info.StringTable:
                    for k, v in st.entries.items():
                        key = k.decode("utf-8", "replace") if isinstance(k, bytes) else k
                        val = v.decode("utf-8", "replace") if isinstance(v, bytes) else v
                        result[key] = val
    pe.close()
    return result


def _read_version_languages(exe_path: Path) -> list[int]:
    """Return the list of language IDs carrying an RT_VERSION resource."""
    pe = pefile.PE(str(exe_path), fast_load=True)
    pe.parse_data_directories(
        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_RESOURCE"]]
    )
    langs: list[int] = []
    rt_version = pefile.RESOURCE_TYPE["RT_VERSION"]
    for entry in pe.DIRECTORY_ENTRY_RESOURCE.entries:
        if entry.id != rt_version:
            continue
        for name_entry in entry.directory.entries:
            for lang_entry in name_entry.directory.entries:
                langs.append(lang_entry.id)
    pe.close()
    return langs


def test_writer_replaces_rather_than_adds(tmp_path: Path):
    """The fix: we write at lang 0x0409 so there's only ONE RT_VERSION after."""
    exe = _copy_python_exe(tmp_path)
    before = _read_version_languages(exe)
    assert before, "python.exe should have an existing VS_VERSION_INFO"

    set_version_info(exe, product_name="MyProduct", file_description="My Tool")

    after = _read_version_languages(exe)
    # Writing at the same language ID as the pre-existing resource (0x0409)
    # means we have exactly as many RT_VERSION entries as before, not one more.
    assert after == before
    # And at least one of them is 0x0409 (en-US).
    assert 0x0409 in after


def test_writer_stamps_all_fields(tmp_path: Path):
    exe = _copy_python_exe(tmp_path, "app.exe")
    set_version_info(
        exe,
        product_name="Acme Suite",
        file_description="Acme Main Tool",
        file_version="1.2.3.4",
        product_version="1.2.0.0",
        company_name="Acme Corp",
        legal_copyright="Copyright (c) 2026 Acme Corp",
        internal_name="acme-main",
        original_filename="app.exe",
    )

    strings = _read_version_strings(exe)
    assert strings["ProductName"] == "Acme Suite"
    assert strings["FileDescription"] == "Acme Main Tool"
    assert strings["FileVersion"] == "1.2.3.4"
    assert strings["ProductVersion"] == "1.2.0.0"
    assert strings["CompanyName"] == "Acme Corp"
    assert strings["LegalCopyright"] == "Copyright (c) 2026 Acme Corp"
    assert strings["InternalName"] == "acme-main"
    assert strings["OriginalFilename"] == "app.exe"


def test_writer_evicts_python_filedescription(tmp_path: Path):
    """Before the fix, FileDescription still read 'Python' after stamping.

    This is the canary test for the language ID bug.
    """
    exe = _copy_python_exe(tmp_path)
    before = _read_version_strings(exe)
    # Sanity: python.exe ships with FileDescription "Python"
    assert before.get("FileDescription", "").lower().startswith("python")

    set_version_info(exe, product_name="NotPython", file_description="NotPython")

    after = _read_version_strings(exe)
    assert after.get("FileDescription") == "NotPython"
    assert after.get("FileDescription", "").lower() != "python"


def test_writer_defaults(tmp_path: Path):
    """Omitted fields fall back to reasonable defaults."""
    exe = _copy_python_exe(tmp_path, "Thing.exe")
    set_version_info(exe, product_name="Thing")

    strings = _read_version_strings(exe)
    assert strings["ProductName"] == "Thing"
    # file_description falls back to product_name
    assert strings["FileDescription"] == "Thing"
    # internal_name falls back to product_name
    assert strings["InternalName"] == "Thing"
    # original_filename falls back to exe filename
    assert strings["OriginalFilename"] == "Thing.exe"


def test_writer_unicode_values(tmp_path: Path):
    """Non-ASCII values roundtrip correctly (UTF-16 LE path)."""
    exe = _copy_python_exe(tmp_path)
    set_version_info(
        exe,
        product_name="Ácme Suíte",
        file_description="Tëst — App",
        company_name="Ünicode Corp",
    )
    strings = _read_version_strings(exe)
    assert strings["ProductName"] == "Ácme Suíte"
    assert strings["FileDescription"] == "Tëst — App"
    assert strings["CompanyName"] == "Ünicode Corp"


def test_writer_omits_empty_optional_fields(tmp_path: Path):
    """Empty company_name / legal_copyright / comments are not written as blank strings."""
    exe = _copy_python_exe(tmp_path)
    set_version_info(exe, product_name="Thing")
    strings = _read_version_strings(exe)
    assert "CompanyName" not in strings
    assert "LegalCopyright" not in strings
    assert "Comments" not in strings


def test_writer_stamps_comments(tmp_path: Path):
    """The new Comments field roundtrips through the writer."""
    exe = _copy_python_exe(tmp_path)
    set_version_info(
        exe,
        product_name="Thing",
        comments="Developed by Nathan Curtis",
    )
    strings = _read_version_strings(exe)
    assert strings["Comments"] == "Developed by Nathan Curtis"

"""End-to-end test: build the fixture and verify each exe's VERSIONINFO.

Uses a real Python runtime (copied from the host interpreter) so target
exes are stampable PEs. Parses the produced exes with pefile.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "win32", reason="VERSIONINFO stamping is Windows-only"
)

pefile = pytest.importorskip("pefile")

from coil.config import get_versioninfo_config, load_config
from coil.packager import package_bundled


FIXTURE_DIR = Path(__file__).parent / "fixtures" / "versioninfo_sample"


def _read_version_strings(exe_path: Path) -> dict[str, str]:
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


def test_bundled_build_stamps_versioninfo_from_fixture(
    tmp_path: Path, monkeypatch, real_runtime: Path
):
    """Full pipeline: parse fixture coil.toml, build, verify each exe.

    Mocks the .pyc compilation step — we're testing VERSIONINFO wiring,
    not the compiler.
    """
    raw = load_config(FIXTURE_DIR)
    assert raw is not None

    entries = ["main.py", "worker.py", "With Spaces.py", "no_comment.py"]
    stems = [Path(e).stem for e in entries]

    versioninfo = {
        stem: get_versioninfo_config(
            raw,
            entry_name=stem,
            project_name="AcmeSuite",
            project_dir=FIXTURE_DIR,
        )
        for stem in stems
    }

    # Skip real compilation: we don't need real .pyc files for this test.
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
        name="AcmeSuite",
        target_os="windows",
        versioninfo=versioninfo,
    )
    assert bundle.is_dir()

    # Primary entry keeps bundle name for single-entry naming, but with
    # multiple entries each gets its stem. Check each.
    expected = {
        "main.exe": {
            "ProductName": "Acme Suite",
            "FileDescription": "Acme Suite - Main Tool",
            "CompanyName": "Acme Corp",
            "LegalCopyright": "Copyright (c) 2026 Acme Corp",
            "FileVersion": "1.2.3.0",
            "ProductVersion": "1.2.3.0",
            "InternalName": "main",
            "OriginalFilename": "main.exe",
            "Comments": "Built by the platform team - main entry",
        },
        "worker.exe": {
            "ProductName": "Acme Suite",
            "FileDescription": "Acme Suite - Worker",
            "CompanyName": "Acme Corp",
            "InternalName": "acme-worker",
            "OriginalFilename": "worker.exe",
            "Comments": "Built by the platform team",
        },
        "With Spaces.exe": {
            "ProductName": "Acme Suite",
            "FileDescription": "Acme Suite - Spaced Tool",
            "CompanyName": "Acme Corp",
            "InternalName": "With Spaces",
            "OriginalFilename": "With Spaces.exe",
            "Comments": "Built by the platform team",
        },
        "no_comment.exe": {
            "ProductName": "Acme Suite",
            "FileDescription": "Acme Suite - Silent Tool",
            "CompanyName": "Acme Corp",
            "InternalName": "no_comment",
            "OriginalFilename": "no_comment.exe",
        },
    }

    for exe_name, want in expected.items():
        exe = bundle / exe_name
        assert exe.is_file(), f"expected {exe_name} in {bundle}"
        got = _read_version_strings(exe)
        for key, value in want.items():
            assert got.get(key) == value, (
                f"{exe_name}: {key} expected {value!r}, got {got.get(key)!r}"
            )
        # Canary: must not have inherited python.exe's FileDescription
        assert got["FileDescription"].lower() != "python"

    # Explicit empty override on no_comment must blank out the shared
    # `comments` value — the writer omits empty optional fields, so Comments
    # should not appear in the stamped PE at all.
    silent = _read_version_strings(bundle / "no_comment.exe")
    assert "Comments" not in silent, (
        f"no_comment.exe should have no Comments (got {silent.get('Comments')!r})"
    )


def test_bundled_build_with_quoted_stem_loads_and_stamps(tmp_path: Path):
    """Directly exercise the quoted-stem path via load_config round-trip."""
    raw = load_config(FIXTURE_DIR)
    assert raw is not None
    vi = get_versioninfo_config(
        raw,
        entry_name="With Spaces",
        project_name="AcmeSuite",
        project_dir=FIXTURE_DIR,
    )
    assert vi["file_description"] == "Acme Suite - Spaced Tool"
    assert vi["file_version"] == "1.2.3.0"
    assert vi["product_name"] == "Acme Suite"

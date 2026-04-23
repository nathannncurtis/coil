"""End-to-end tests for sitecustomize exe-to-boot dispatch.

Builds real bundles with a host Python runtime copy, rewrites each produced
_boot_*.py with a tagged stub, subprocess-runs the exes, and asserts the
correct boot ran. Covers the four canonical layouts:

  - single entry, no rename        (main.py -> main.exe)
  - single entry, renamed via name (main.py -> Renamed.exe)
  - multi entry                    (main.py + worker.py)
  - multi entry with spaces        (main.py + "Helper With Space.py")

If any of these misroute, the generated sitecustomize has a live dispatch
bug (not just a latent one in the Step 2/3 fallbacks).
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "win32",
    reason="sitecustomize dispatch uses the python.exe launcher (Windows-only)",
)


def _find_embed_zip() -> Path:
    """Locate a cached Windows embeddable Python zip.

    Prefers a version matching the host interpreter (so runtime_python pyc
    magic is compatible with pytest's compile step). Falls back to any
    available embeddable zip.
    """
    cache = Path.home() / ".coil" / "cache" / "runtimes"
    if not cache.is_dir():
        pytest.skip(
            f"No embeddable Python cache at {cache}. "
            "Run any `coil build` to populate it."
        )
    host = f"{sys.version_info.major}.{sys.version_info.minor}"
    preferred = sorted(cache.glob(f"python-{host}.*-embed-amd64.zip"))
    if preferred:
        return preferred[-1]
    fallback = sorted(cache.glob("python-*-embed-amd64.zip"))
    if fallback:
        return fallback[-1]
    pytest.skip(f"No embeddable Python zip in {cache}.")


def _make_real_runtime(tmp_path: Path) -> Path:
    """Extract a cached embeddable Python distribution as the build runtime.

    A full embeddable dist is required (not just python.exe + DLLs): the
    generated sitecustomize runs under a renamed python.exe, and CPython
    aborts during init if the bundled stdlib zip is missing `encodings`.
    """
    runtime = tmp_path / "runtime"
    runtime.mkdir()

    zip_path = _find_embed_zip()
    with zipfile.ZipFile(zip_path, "r") as zf:
        zf.extractall(runtime)
    return runtime


def _fake_obfuscate(project_dir, internal_dir, ui=None, optimize=0, runtime_python=None, skip=None):
    """Produce empty .pyc files per .py source. Real compilation isn't needed
    because the tagged boot stubs exit before touching the pyc."""
    app_dir = internal_dir / "app"
    app_dir.mkdir(parents=True, exist_ok=True)
    for src in project_dir.glob("*.py"):
        if skip is not None and skip(src):
            continue
        (app_dir / (src.stem + ".pyc")).write_bytes(b"")


def _tag_boot_scripts(internal_dir: Path) -> list[str]:
    """Overwrite each _boot_*.py with a stub that prints a tag identifying
    itself, then exits 0. Returns the stems found."""
    stems: list[str] = []
    for boot in internal_dir.glob("_boot_*.py"):
        stem = boot.name[len("_boot_"):-len(".py")]
        stems.append(stem)
        boot.write_text(
            "import sys\n"
            f"print('BOOT_TAG={stem}', flush=True)\n"
            "sys.exit(0)\n",
            encoding="utf-8",
        )
    return stems


def _build_and_run(
    tmp_path: Path,
    monkeypatch,
    entries: list[str],
    name: str,
) -> dict[str, str]:
    """Build a bundle, rewrite boot stubs, launch each exe, return {exe_stem: boot_tag}."""
    from coil.packager import package_bundled

    project = tmp_path / "proj"
    project.mkdir()
    for entry in entries:
        (project / entry).write_text(f"# {entry}\n", encoding="utf-8")

    monkeypatch.setattr("coil.packager.obfuscate_default", _fake_obfuscate)
    monkeypatch.setattr("coil.packager.obfuscate_secure", _fake_obfuscate)

    runtime = _make_real_runtime(tmp_path)
    output = tmp_path / "dist"

    bundle = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=runtime,
        entry_points=entries,
        name=name,
        target_os="windows",
    )
    assert bundle.is_dir()

    _tag_boot_scripts(bundle / "_internal")

    observed: dict[str, str] = {}
    for exe in sorted(bundle.glob("*.exe")):
        result = subprocess.run(
            [str(exe)],
            stdin=subprocess.DEVNULL,
            capture_output=True,
            timeout=15,
            text=True,
        )
        assert result.returncode == 0, (
            f"{exe.name} exited {result.returncode}: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        tag = None
        for line in result.stdout.splitlines():
            if line.startswith("BOOT_TAG="):
                tag = line[len("BOOT_TAG="):].strip()
                break
        assert tag is not None, (
            f"{exe.name} produced no BOOT_TAG: "
            f"stdout={result.stdout!r} stderr={result.stderr!r}"
        )
        observed[exe.stem] = tag
    return observed


def test_single_entry_no_rename(tmp_path: Path, monkeypatch):
    observed = _build_and_run(tmp_path, monkeypatch, entries=["main.py"], name="main")
    assert observed == {"main": "main"}


def test_single_entry_with_rename(tmp_path: Path, monkeypatch):
    observed = _build_and_run(tmp_path, monkeypatch, entries=["main.py"], name="Renamed")
    assert observed == {"Renamed": "Renamed"}


def test_multi_entry(tmp_path: Path, monkeypatch):
    observed = _build_and_run(
        tmp_path, monkeypatch, entries=["main.py", "worker.py"], name="MultiApp"
    )
    assert observed == {"main": "main", "worker": "worker"}


def test_multi_entry_with_spaces(tmp_path: Path, monkeypatch):
    observed = _build_and_run(
        tmp_path,
        monkeypatch,
        entries=["main.py", "Helper With Space.py"],
        name="SpacedApp",
    )
    assert observed == {"main": "main", "Helper With Space": "Helper With Space"}

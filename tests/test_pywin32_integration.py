"""End-to-end: build a bundle that imports pywin32 and run it.

Proves Problems 1 + 2 + 3 are all closed:
  1. .pth processing — pywin32.pth adds win32, win32/lib, Pythonwin to sys.path.
  2. DLL directories — pywin32_system32/ DLLs are findable at extension load time.
  3. Exit behavior — the exe returns 0 without falling into a REPL or stdin block.

Skips cleanly if:
  - Not running on Windows.
  - pywin32 isn't importable in the host environment.
  - Host site-packages layout can't be found.
  - No cached Coil runtime matches the host Python version.
"""

from __future__ import annotations

import importlib.util
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

pytestmark = pytest.mark.skipif(
    sys.platform != "win32", reason="pywin32 is Windows-only"
)


_PYWIN32_ITEMS = (
    "win32",
    "win32com",
    "win32comext",
    "pythonwin",       # lowercase on disk; pywin32.pth references "Pythonwin"
    "pywin32_system32",
    "pythoncom.py",
    "pywintypes.py",
    "pywin32.pth",
    "pywin32_bootstrap.py",  # may or may not exist at top level
)


def _host_site_packages() -> Path | None:
    import site
    for p in site.getsitepackages():
        pp = Path(p) / "Lib" / "site-packages"
        if pp.is_dir():
            return pp
        pp2 = Path(p)
        if pp2.name.lower() == "site-packages" and pp2.is_dir():
            return pp2
    return None


def _copy_pywin32(src: Path, dst: Path) -> None:
    """Copy pywin32 artifacts from host site-packages to deps dir.

    pywin32 on disk uses 'pythonwin' (lowercase), but pywin32.pth references
    'Pythonwin'. Windows is case-insensitive so it resolves, but we don't
    rename — the .pth processing in site.addsitedir() checks os.path.exists
    which is case-insensitive on Windows.
    """
    dst.mkdir(parents=True, exist_ok=True)
    for name in _PYWIN32_ITEMS:
        s = src / name
        if not s.exists():
            continue
        d = dst / name
        if s.is_dir():
            shutil.copytree(s, d, dirs_exist_ok=True)
        else:
            shutil.copy2(s, d)


def _is_host_version_match(runtime: Path) -> bool:
    """True if the runtime's pythonXX.dll matches the host minor version.

    The bundled pywin32 .pyd files are ABI-specific, so this test only makes
    sense when the runtime's Python matches the host interpreter that owns
    the pywin32 install we're copying from.
    """
    host_short = f"{sys.version_info.major}{sys.version_info.minor}"
    return (runtime / f"python{host_short}.dll").is_file()


def test_pywin32_import_end_to_end(tmp_path: Path, real_runtime: Path):
    if importlib.util.find_spec("win32pipe") is None:
        pytest.skip("pywin32 not importable in host environment")
    site_pkgs = _host_site_packages()
    if site_pkgs is None or not (site_pkgs / "pywin32.pth").is_file():
        pytest.skip("pywin32.pth not found in host site-packages")
    if not _is_host_version_match(real_runtime):
        pytest.skip(
            f"real_runtime doesn't match host Python "
            f"{sys.version_info.major}.{sys.version_info.minor}; "
            "pywin32 .pyd ABI would mismatch"
        )

    # Materialize deps dir mimicking a pip install --target
    deps = tmp_path / "deps"
    _copy_pywin32(site_pkgs, deps)

    # Minimal app exercising the full import path
    project = tmp_path / "proj"
    project.mkdir()
    (project / "main.py").write_text(
        "from win32com.client import Dispatch\n"
        "import win32pipe\n"
        "import win32api\n"
        "print('OK')\n",
        encoding="utf-8",
    )

    from coil.packager import package_bundled
    bundle = package_bundled(
        project_dir=project,
        output_dir=tmp_path / "dist",
        runtime_dir=real_runtime,
        entry_points=["main.py"],
        name="pywin32probe",
        target_os="windows",
        deps_dir=deps,
    )

    exe = bundle / "pywin32probe.exe"
    assert exe.is_file(), f"built exe missing at {exe}"

    result = subprocess.run(
        [str(exe)],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        timeout=30,
        text=True,
    )
    assert result.returncode == 0, (
        f"exit={result.returncode}\nstdout={result.stdout!r}\nstderr={result.stderr!r}"
    )
    assert "OK" in result.stdout, f"stdout={result.stdout!r}"

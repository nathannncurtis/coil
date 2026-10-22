"""Locate a CPython install to supply tkinter, which the embeddable runtime lacks."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

TK_IMPORTS = {"tkinter", "_tkinter", "customtkinter", "ttkbootstrap", "ttkthemes", "tkinterdnd2"}

_TCL_SKIP = ("demos", "tix*", "nmake", "*.lib", "*.sh")


def needs_tkinter(imports: set[str]) -> bool:
    return bool(imports & TK_IMPORTS)


def _minor(python_version: str) -> tuple[int, int]:
    major, minor = python_version.split(".")[:2]
    return int(major), int(minor)


def _has_tkinter_files(prefix: Path) -> bool:
    dlls = prefix / "DLLs"
    return ((prefix / "Lib" / "tkinter" / "__init__.py").is_file()
            and (dlls / "_tkinter.pyd").is_file()
            and any(dlls.glob("tcl*.dll")) and any(dlls.glob("tk*.dll"))
            and (prefix / "tcl" / "tcl8.6" / "init.tcl").is_file()
            and (prefix / "tcl" / "tk8.6" / "tk.tcl").is_file())


def _matches(prefix: Path, version: tuple[int, int]) -> bool:
    python = prefix / "python.exe"
    if not python.is_file() or not _has_tkinter_files(prefix):
        return False
    try:
        out = subprocess.run(
            [str(python), "-I", "-c",
             "import sys,struct;print(sys.version_info[0],sys.version_info[1],struct.calcsize('P'))"],
            capture_output=True, text=True, timeout=30).stdout.split()
    except (OSError, subprocess.SubprocessError):
        return False
    return out == [str(version[0]), str(version[1]), "8"]


def _candidates(version: tuple[int, int]) -> list[Path]:
    found = [Path(sys.base_prefix)]
    tag = f"{version[0]}.{version[1]}"
    launcher = shutil.which("py")
    if launcher:
        try:
            out = subprocess.run(
                [launcher, f"-{tag}", "-c", "import sys;print(sys.base_prefix)"],
                capture_output=True, text=True, timeout=30)
            if out.returncode == 0 and out.stdout.strip():
                found.append(Path(out.stdout.strip()))
        except (OSError, subprocess.SubprocessError):
            pass
    if sys.platform == "win32":
        import winreg
        for hive in (winreg.HKEY_CURRENT_USER, winreg.HKEY_LOCAL_MACHINE):
            try:
                with winreg.OpenKey(hive, rf"SOFTWARE\Python\PythonCore\{tag}\InstallPath") as key:
                    found.append(Path(winreg.QueryValueEx(key, "")[0]))
            except OSError:
                pass
    return found


def find_tkinter_install(python_version: str) -> Path | None:
    """Return the prefix of a 64-bit CPython install of the target minor version that has tkinter."""
    version = _minor(python_version)
    seen: set[Path] = set()
    for prefix in _candidates(version):
        key = prefix.resolve() if prefix.exists() else prefix
        if key in seen:
            continue
        seen.add(key)
        if _matches(prefix, version):
            return prefix
    return None


def missing_message(python_version: str) -> str:
    tag = ".".join(python_version.split(".")[:2])
    return (f"The project uses tkinter, which the Windows embeddable runtime does not include. "
            f"Coil copies it from an installed 64-bit CPython {tag} with Tcl/Tk, but none was found. "
            f"Install Python {tag} (64-bit, with 'tcl/tk and IDLE') or remove the tkinter import.")


def stage_tkinter(install: Path, lib_dir: Path) -> None:
    """Copy tkinter, _tkinter, the Tcl/Tk DLLs (Tcl links zlib1.dll) and the Tcl/Tk script libraries into lib_dir."""
    lib_dir.mkdir(parents=True, exist_ok=True)
    shutil.copytree(install / "Lib" / "tkinter", lib_dir / "tkinter",
                    ignore=shutil.ignore_patterns("__pycache__"), dirs_exist_ok=True)
    dlls = install / "DLLs"
    for src in [dlls / "_tkinter.pyd", dlls / "zlib1.dll", *dlls.glob("tcl*.dll"), *dlls.glob("tk*.dll")]:
        if src.is_file():
            shutil.copy2(src, lib_dir / src.name)
    shutil.copytree(install / "tcl", lib_dir / "tcl",
                    ignore=shutil.ignore_patterns(*_TCL_SKIP), dirs_exist_ok=True)

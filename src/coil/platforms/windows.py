"""Windows platform handler for building .exe files.

Creates a launcher batch-turned-exe or Python-based launcher that
boots the embedded Python runtime and runs the entry point.
"""

import os
import platform
import shutil
import struct
import subprocess
import sys
import zipfile
from pathlib import Path

from coil.platforms.base import PlatformHandler


# PE header constants for GUI/Console subsystem
_PE_CONSOLE_SUBSYSTEM = 3
_PE_GUI_SUBSYSTEM = 2


class WindowsHandler(PlatformHandler):
    """Windows .exe builder."""

    def get_runtime_arch(self) -> str:
        """Get Windows architecture for the embeddable runtime."""
        machine = platform.machine().lower()
        if machine in ("amd64", "x86_64"):
            return "amd64"
        elif machine in ("x86", "i386", "i686"):
            return "win32"
        else:
            return "amd64"

    def get_executable_extension(self) -> str:
        """Windows executables use .exe."""
        return ".exe"

    def create_launcher(
        self,
        output_dir: Path,
        entry_point: str,
        name: str,
        gui: bool = False,
        icon: str | None = None,
    ) -> Path:
        """Create a Windows launcher script.

        Generates a launcher that invokes the embedded Python runtime
        with the entry point script.

        Args:
            output_dir: Directory containing runtime and app files.
            entry_point: Entry point script (e.g. "main.pyc").
            name: Name for the output executable.
            gui: Suppress console window.
            icon: Path to .ico file (optional).

        Returns:
            Path to the launcher.
        """
        launcher_name = f"{name}.exe"
        launcher_path = output_dir / launcher_name

        # Create a Python-based launcher script
        launcher_script = output_dir / f"_launcher_{name}.py"
        script_content = _generate_launcher_script(entry_point, gui)
        launcher_script.write_text(script_content, encoding="utf-8")

        # Create a batch wrapper that calls the embedded Python
        bat_path = output_dir / f"{name}.bat"
        python_exe = _find_python_exe(output_dir)
        bat_content = _generate_bat_launcher(python_exe, launcher_script.name, gui)
        bat_path.write_text(bat_content, encoding="utf-8")

        # For GUI mode, create a VBS wrapper to avoid console flash
        if gui:
            vbs_path = output_dir / f"{name}.vbs"
            vbs_content = _generate_vbs_launcher(bat_path.name)
            vbs_path.write_text(vbs_content, encoding="utf-8")

        return bat_path


def _find_python_exe(runtime_dir: Path) -> str:
    """Find the python.exe in the runtime directory."""
    # Check directly in the runtime dir
    if (runtime_dir / "python.exe").is_file():
        return "python.exe"

    # Check in a runtime subdirectory
    for subdir in ("runtime", "python"):
        if (runtime_dir / subdir / "python.exe").is_file():
            return f"{subdir}\\python.exe"

    return "python.exe"


def _generate_launcher_script(entry_point: str, gui: bool) -> str:
    """Generate the Python launcher script content."""
    return f'''\
import os
import sys

# Set up paths relative to this launcher
_base = os.path.dirname(os.path.abspath(__file__))
_app = os.path.join(_base, "app")
_lib = os.path.join(_base, "lib")

# Add app and lib to sys.path
if _app not in sys.path:
    sys.path.insert(0, _app)
if os.path.isdir(_lib) and _lib not in sys.path:
    sys.path.insert(0, _lib)

# Run the entry point
_entry = os.path.join(_app, "{entry_point}")
if _entry.endswith(".pyc"):
    import importlib.util
    spec = importlib.util.spec_from_file_location("__main__", _entry)
    if spec and spec.loader:
        mod = importlib.util.module_from_spec(spec)
        mod.__name__ = "__main__"
        sys.modules["__main__"] = mod
        spec.loader.exec_module(mod)
else:
    with open(_entry, "r") as f:
        exec(compile(f.read(), _entry, "exec"), {{"__name__": "__main__", "__file__": _entry}})
'''


def _generate_bat_launcher(python_exe: str, launcher_script: str, gui: bool) -> str:
    """Generate the batch launcher content."""
    if gui:
        return f'@echo off\nstart "" /B "%~dp0{python_exe}" "%~dp0{launcher_script}"\n'
    else:
        return f'@echo off\n"%~dp0{python_exe}" "%~dp0{launcher_script}" %*\n'


def _generate_vbs_launcher(bat_name: str) -> str:
    """Generate a VBScript launcher to hide the console window for GUI apps."""
    return (
        f'Set WshShell = CreateObject("WScript.Shell")\n'
        f'WshShell.Run """{bat_name}""", 0, False\n'
    )


def set_pe_subsystem(exe_path: Path, gui: bool) -> None:
    """Modify a PE executable's subsystem flag.

    Sets the subsystem to GUI (2) or Console (3) in the PE header.
    This is used to prevent a console window from appearing for GUI apps.

    Args:
        exe_path: Path to the .exe file.
        gui: True for GUI subsystem, False for console.
    """
    subsystem = _PE_GUI_SUBSYSTEM if gui else _PE_CONSOLE_SUBSYSTEM

    with open(exe_path, "r+b") as f:
        # Read DOS header to find PE offset
        f.seek(0x3C)
        pe_offset = struct.unpack("<I", f.read(4))[0]

        # PE subsystem is at PE offset + 0x5C
        subsystem_offset = pe_offset + 0x5C
        f.seek(subsystem_offset)
        f.write(struct.pack("<H", subsystem))

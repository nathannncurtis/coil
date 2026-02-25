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


def set_exe_icon(exe_path: Path, icon_path: Path) -> None:
    """Embed an .ico file into a PE executable's resources.

    Uses the Windows UpdateResource API to write the icon data
    directly into the exe's resource section.

    Args:
        exe_path: Path to the .exe file to modify.
        icon_path: Path to the .ico file.
    """
    import ctypes

    icon_path = Path(icon_path)
    if not icon_path.is_file():
        raise FileNotFoundError(f"Icon file not found: {icon_path}")

    with open(icon_path, "rb") as f:
        ico_data = f.read()

    # Parse ICONDIR header
    if len(ico_data) < 6:
        raise ValueError("Invalid .ico file: too small")
    reserved, ico_type, count = struct.unpack_from("<HHH", ico_data, 0)
    if ico_type != 1 or count == 0:
        raise ValueError("Not a valid .ico file")

    # Parse ICONDIRENTRY entries (16 bytes each)
    entries = []
    for i in range(count):
        offset = 6 + i * 16
        if offset + 16 > len(ico_data):
            raise ValueError("Invalid .ico file: truncated directory")
        width, height, colors, res, planes, bits, size, img_offset = (
            struct.unpack_from("<BBBBHHII", ico_data, offset)
        )
        entries.append({
            "width": width,
            "height": height,
            "colors": colors,
            "reserved": res,
            "planes": planes,
            "bits": bits,
            "size": size,
            "offset": img_offset,
        })

    # Windows resource constants
    RT_ICON = 3
    RT_GROUP_ICON = 14
    LANG_NEUTRAL = 0

    kernel32 = ctypes.windll.kernel32

    # Set up proper function signatures for the Windows API
    kernel32.BeginUpdateResourceW.argtypes = [ctypes.c_wchar_p, ctypes.c_bool]
    kernel32.BeginUpdateResourceW.restype = ctypes.c_void_p

    kernel32.UpdateResourceW.argtypes = [
        ctypes.c_void_p,  # hUpdate
        ctypes.c_void_p,  # lpType (MAKEINTRESOURCE)
        ctypes.c_void_p,  # lpName (MAKEINTRESOURCE)
        ctypes.c_ushort,  # wLanguage
        ctypes.c_void_p,  # lpData
        ctypes.c_ulong,   # cb
    ]
    kernel32.UpdateResourceW.restype = ctypes.c_bool

    kernel32.EndUpdateResourceW.argtypes = [ctypes.c_void_p, ctypes.c_bool]
    kernel32.EndUpdateResourceW.restype = ctypes.c_bool

    # Begin updating resources
    handle = kernel32.BeginUpdateResourceW(str(exe_path), False)
    if not handle:
        raise OSError(f"BeginUpdateResource failed (error {ctypes.GetLastError()})")

    try:
        # Add each icon image as an RT_ICON resource
        for i, entry in enumerate(entries):
            raw = ico_data[entry["offset"]:entry["offset"] + entry["size"]]
            buf = ctypes.create_string_buffer(raw)
            resource_id = i + 1

            ok = kernel32.UpdateResourceW(
                handle, RT_ICON, resource_id, LANG_NEUTRAL,
                buf, len(raw),
            )
            if not ok:
                raise OSError(
                    f"UpdateResource RT_ICON failed (error {ctypes.GetLastError()})"
                )

        # Build GRPICONDIR: header (6 bytes) + entries (14 bytes each)
        grp_data = struct.pack("<HHH", 0, 1, count)
        for i, entry in enumerate(entries):
            grp_data += struct.pack(
                "<BBBBHHIH",
                entry["width"],
                entry["height"],
                entry["colors"],
                entry["reserved"],
                entry["planes"],
                entry["bits"],
                entry["size"],
                i + 1,  # nID — references the RT_ICON resource ID
            )

        grp_buf = ctypes.create_string_buffer(grp_data)
        ok = kernel32.UpdateResourceW(
            handle, RT_GROUP_ICON, 1, LANG_NEUTRAL,
            grp_buf, len(grp_data),
        )
        if not ok:
            raise OSError(
                f"UpdateResource RT_GROUP_ICON failed (error {ctypes.GetLastError()})"
            )

        # Commit
        if not kernel32.EndUpdateResourceW(handle, False):
            raise OSError(
                f"EndUpdateResource failed (error {ctypes.GetLastError()})"
            )
    except Exception:
        # Discard changes on failure
        kernel32.EndUpdateResourceW(handle, True)
        raise


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

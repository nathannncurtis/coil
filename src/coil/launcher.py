"""Native bundled launchers that pass application arguments straight to Python.

The entry point is patched into the executable itself, so renaming the executable
cannot redirect it to a different application. Resource and subsystem stamping
can follow the patch. CPython's PyConfig ABI requires one stub per minor version.
"""

from __future__ import annotations

from pathlib import Path
import re
import struct

from coil._launcher_stubs import STUBS


_CONFIG_MARKER = b"COIL_NATIVE_CONFIG_V1_7D39B28A".ljust(32, b"\0")
_BOOT_BYTES = 512


def get_launcher(python_version: str, boot_script: str, optimization_level: int = 0,
                 *, portable: bool = False, error_logging: bool = False) -> bytes:
    """Return an x64 PE launcher for a Python minor and internal boot basename.

    ``python_version`` accepts a release (``3.12.10``), minor (``3.12``),
    or embedded-runtime tag (``312`` / ``python312``).
    """
    match = re.fullmatch(r"(?:python)?3\.?([0-9]{1,2})(?:\.[0-9]+)?", python_version)
    if not match:
        raise ValueError(f"Invalid Python version: {python_version!r}")
    minor = f"3.{int(match.group(1))}"
    if minor not in STUBS:
        raise ValueError(f"No bundled launcher for Python {minor}; supported: {', '.join(STUBS)}")
    if (not boot_script or any(char in boot_script for char in '/\\\0:')
            or boot_script in {".", ".."} or boot_script.endswith((" ", "."))):
        raise ValueError("Boot script must be a single filename inside _internal")
    encoded = boot_script.encode("utf-16-le")
    if len(encoded) > _BOOT_BYTES - 2:
        raise ValueError("Boot script filename exceeds 255 UTF-16 characters")
    if type(optimization_level) is not int or optimization_level not in (0, 1, 2):
        raise ValueError("optimization_level must be 0, 1, or 2")
    stub = STUBS[minor]
    if stub.count(_CONFIG_MARKER) != 1:
        raise RuntimeError(f"Corrupt bundled launcher configuration for Python {minor}")
    offset = stub.index(_CONFIG_MARKER) + len(_CONFIG_MARKER)
    patched = bytearray(stub)
    patched[offset:offset + _BOOT_BYTES] = encoded.ljust(_BOOT_BYTES, b"\0")
    struct.pack_into("<i", patched, offset + _BOOT_BYTES, optimization_level)
    struct.pack_into("<i", patched, offset + _BOOT_BYTES + 4, bool(portable))
    struct.pack_into("<i", patched, offset + _BOOT_BYTES + 8, bool(error_logging))
    return bytes(patched)


def create_launcher(destination: Path, python_version: str, boot_script: str,
                    optimization_level: int = 0, *, portable: bool = False,
                    error_logging: bool = False) -> None:
    """Write a launcher, ready for icon, version-resource and subsystem stamping."""
    Path(destination).write_bytes(get_launcher(python_version, boot_script, optimization_level,
                                             portable=portable, error_logging=error_logging))

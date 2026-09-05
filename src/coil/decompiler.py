"""Decompilation logic for Coil-built executables.

Recovers source from default-mode builds. Refuses secure builds.
"""

from __future__ import annotations

import json
import io
import struct
import zipfile
from pathlib import Path, PurePosixPath

from coil.obfuscator import COIL_METADATA_FILENAME, COIL_SOURCE_ARCHIVE


def decompile(executable_path: Path, output_dir: Path) -> bool:
    """Decompile a Coil-built executable back to source.

    Extracts the embedded source archive from a default-mode build.
    Refuses to decompile secure-mode builds.

    Args:
        executable_path: Path to the Coil-built executable or build directory.
        output_dir: Where to write recovered source files.

    Returns:
        True if decompilation succeeded, False otherwise.
    """
    # A portable exe owns its appended archive, even when unrelated bundled
    # applications happen to share its output directory.
    if executable_path.is_file():
        try:
            payload = _portable_payload(executable_path)
            if payload is not None:
                with zipfile.ZipFile(io.BytesIO(payload)) as archive:
                    prefix = "_internal/app/"
                    metadata = json.loads(archive.read(prefix + COIL_METADATA_FILENAME))
                    if _is_secure(metadata):
                        return False
                    source = _source_name(metadata)
                    return _extract_source(io.BytesIO(archive.read(prefix + source)), output_dir)
        except (OSError, ValueError, KeyError, zipfile.BadZipFile) as exc:
            print(f"Error: Could not read portable source archive: {exc}")
            return False

    app_dir = _find_app_dir(executable_path)
    if app_dir is None:
        print(
            "Error: This does not appear to be a Coil-built executable. "
            "No Coil metadata found."
        )
        return False

    # Read metadata
    meta_path = app_dir / COIL_METADATA_FILENAME
    try:
        metadata = json.loads(meta_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError) as e:
        print(f"Error: Could not read Coil metadata: {e}")
        return False

    # Check if secure build
    if _is_secure(metadata):
        return False

    # Find and extract source archive
    try:
        archive_name = _source_name(metadata)
    except ValueError as exc:
        print(f"Error: {exc}")
        return False
    archive_path = app_dir / archive_name
    if not archive_path.is_file():
        print("Error: Source archive not found in the executable.")
        return False

    try:
        return _extract_source(archive_path, output_dir)
    except (OSError, ValueError, zipfile.BadZipFile) as exc:
        print(f"Error: Could not recover source archive: {exc}")
        return False


def _is_secure(metadata: dict) -> bool:
    if metadata.get("secure", False):
        print("This executable was built with --secure and cannot be decompiled by Coil.")
        return True
    return False


def _source_name(metadata: dict) -> str:
    name = metadata.get("source_archive", COIL_SOURCE_ARCHIVE)
    if not isinstance(name, str) or name in ("", ".", "..") or any(c in name for c in "/\\:"):
        raise ValueError("Invalid source archive filename in Coil metadata.")
    return name


def _extract_source(source, output_dir: Path) -> bool:
    with zipfile.ZipFile(source) as archive:
        # Check every destination before writing, including existing symlinks.
        root = output_dir.resolve()
        for member in archive.infolist():
            name = member.filename.replace("\\", "/")
            parts = PurePosixPath(name)
            if parts.is_absolute() or ".." in parts.parts or ":" in name:
                raise ValueError(f"Unsafe source archive member: {member.filename}")
            destination = (root / name).resolve()
            if destination != root and root not in destination.parents:
                raise ValueError(f"Source destination escapes output directory: {member.filename}")
        output_dir.mkdir(parents=True, exist_ok=True)
        archive.extractall(output_dir)
        count = sum(member.filename.endswith(".py") for member in archive.infolist())
    print(f"Recovered {count} source file(s) to {output_dir}")
    return True


def _portable_payload(path: Path) -> bytes | None:
    """Read the legacy-compatible trailer, including signed PE overlays."""
    data = path.read_bytes()
    end = len(data)
    certificate = False
    if len(data) >= 64 and data[:2] == b"MZ":
        pe = struct.unpack_from("<I", data, 0x3C)[0]
        if pe + 24 + 152 <= len(data) and data[pe:pe + 4] == b"PE\0\0":
            optional = pe + 24
            magic = struct.unpack_from("<H", data, optional)[0]
            table = optional + (112 if magic == 0x20B else 96) + 4 * 8
            cert, size = struct.unpack_from("<II", data, table)
            if cert or size:
                if not cert or cert + size != len(data):
                    raise ValueError("Invalid PE certificate table.")
                end = cert
                certificate = True
    for padding in range(8 if certificate else 1):
        trailer = end - padding - 12
        if trailer < 0:
            break
        offset, _, magic = struct.unpack_from("<III", data, trailer)
        if magic == 0x434F494C:
            if offset >= trailer:
                raise ValueError("Invalid portable archive offset.")
            return data[offset:trailer]
        if not data[end - padding - 1] == 0:
            break
    return None


def _find_app_dir(path: Path) -> Path | None:
    """Find the app directory containing Coil metadata.

    Searches the given path and common subdirectories for the
    .coil_meta.json file.

    Args:
        path: Path to search (executable or directory).

    Returns:
        Path to the app directory, or None if not found.
    """
    root = path if path.is_dir() else path.parent if path.is_file() else None
    if root is not None:
        for candidate in (root, root / "app", root / "_internal" / "app"):
            if (candidate / COIL_METADATA_FILENAME).is_file():
                return candidate
    return None

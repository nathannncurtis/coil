"""Decompilation logic for Coil-built executables.

Recovers source from default-mode builds. Refuses secure builds.
"""

import json
import shutil
import zipfile
from pathlib import Path

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
    # Find the app directory containing coil metadata
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
    if metadata.get("secure", False):
        print(
            "This executable was built with --secure and cannot be "
            "decompiled by Coil."
        )
        return False

    # Find and extract source archive
    archive_name = metadata.get("source_archive", COIL_SOURCE_ARCHIVE)
    archive_path = app_dir / archive_name
    if not archive_path.is_file():
        print("Error: Source archive not found in the executable.")
        return False

    output_dir.mkdir(parents=True, exist_ok=True)

    with zipfile.ZipFile(archive_path, "r") as zf:
        zf.extractall(output_dir)

    file_count = sum(1 for _ in output_dir.rglob("*.py"))
    print(f"Recovered {file_count} source file(s) to {output_dir}")
    return True


def _find_app_dir(path: Path) -> Path | None:
    """Find the app directory containing Coil metadata.

    Searches the given path and common subdirectories for the
    .coil_meta.json file.

    Args:
        path: Path to search (executable or directory).

    Returns:
        Path to the app directory, or None if not found.
    """
    # If it's a directory, check it directly and common subdirs
    if path.is_dir():
        if (path / COIL_METADATA_FILENAME).is_file():
            return path
        if (path / "app" / COIL_METADATA_FILENAME).is_file():
            return path / "app"
        # Search recursively
        for meta in path.rglob(COIL_METADATA_FILENAME):
            return meta.parent

    # For executable files, check sibling app directory
    if path.is_file():
        parent = path.parent
        if (parent / "app" / COIL_METADATA_FILENAME).is_file():
            return parent / "app"
        if (parent / COIL_METADATA_FILENAME).is_file():
            return parent
        for meta in parent.rglob(COIL_METADATA_FILENAME):
            return meta.parent

    return None

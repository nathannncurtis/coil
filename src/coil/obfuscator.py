"""Obfuscation for Coil-built executables.

Default mode: compiles to .pyc with embedded metadata that allows
coil decompile to recover the original source.

Secure mode: bytecode-only, stripped debug info, no recoverable metadata.
"""

from __future__ import annotations

import compileall
import json
import marshal
import os
import py_compile
import shutil
import struct
import time
import zipfile
from pathlib import Path
from typing import TYPE_CHECKING, Callable, Optional

if TYPE_CHECKING:
    from coil.ui import BuildUI


COIL_METADATA_FILENAME = ".coil_meta.json"
COIL_SOURCE_ARCHIVE = ".coil_source.zip"


def compile_to_pyc(
    source_file: Path,
    output_file: Path,
    optimize: int = 0,
) -> None:
    """Compile a single .py file to .pyc.

    Args:
        source_file: Path to the .py source file.
        output_file: Path for the output .pyc file.
        optimize: Optimization level (0, 1, or 2).
    """
    output_file.parent.mkdir(parents=True, exist_ok=True)
    py_compile.compile(
        str(source_file),
        cfile=str(output_file),
        optimize=optimize,
        doraise=True,
    )


def compile_directory(
    source_dir: Path,
    output_dir: Path,
    optimize: int = 0,
    progress_callback: Optional[Callable[[int, int, Path], None]] = None,
) -> list[Path]:
    """Compile all .py files in a directory to .pyc files.

    Args:
        source_dir: Source directory containing .py files.
        output_dir: Destination for compiled .pyc files.
        optimize: Optimization level.
        progress_callback: Optional callback(current, total, file_path).

    Returns:
        List of compiled .pyc file paths.
    """
    compiled: list[Path] = []
    py_files = sorted(source_dir.rglob("*.py"))
    total = len(py_files)

    for i, py_file in enumerate(py_files):
        relative = py_file.relative_to(source_dir)
        pyc_file = output_dir / relative.with_suffix(".pyc")
        compile_to_pyc(py_file, pyc_file, optimize=optimize)
        compiled.append(pyc_file)
        if progress_callback is not None:
            progress_callback(i + 1, total, py_file)

    return compiled


def obfuscate_default(
    source_dir: Path,
    output_dir: Path,
    ui: Optional[BuildUI] = None,
    optimize: int = 1,
) -> Path:
    """Default obfuscation: compile to .pyc and embed recoverable source.

    Source is packaged into a zip archive alongside the compiled files,
    with metadata that coil decompile can use to recover it.

    Args:
        source_dir: Project source directory.
        output_dir: Build output directory.
        ui: Optional BuildUI for progress display.
        optimize: Bytecode optimization level (0, 1, or 2). Default 1
                  strips assert statements for smaller bytecode.

    Returns:
        Path to the output directory containing compiled files.
    """
    app_dir = output_dir / "app"
    app_dir.mkdir(parents=True, exist_ok=True)

    # Compile all .py to .pyc
    if ui is not None:
        py_files = sorted(source_dir.rglob("*.py"))
        total = len(py_files)
        with ui.file_progress("Compiling source", total=total) as progress:
            task = progress.add_task("", total=total)

            def _cb(current: int, total: int, path: Path) -> None:
                progress.advance(task)

            compile_directory(source_dir, app_dir, optimize=optimize, progress_callback=_cb)
    else:
        compile_directory(source_dir, app_dir, optimize=optimize)

    # Archive original source for recovery
    source_archive = app_dir / COIL_SOURCE_ARCHIVE
    with zipfile.ZipFile(source_archive, "w", zipfile.ZIP_DEFLATED) as zf:
        for py_file in source_dir.rglob("*.py"):
            arcname = str(py_file.relative_to(source_dir))
            zf.write(py_file, arcname)

    # Write metadata
    metadata = {
        "coil_version": _get_version(),
        "secure": False,
        "source_archive": COIL_SOURCE_ARCHIVE,
        "timestamp": int(time.time()),
    }
    meta_path = app_dir / COIL_METADATA_FILENAME
    meta_path.write_text(json.dumps(metadata, indent=2), encoding="utf-8")

    return app_dir


def obfuscate_secure(
    source_dir: Path,
    output_dir: Path,
    ui: Optional[BuildUI] = None,
    optimize: int = 2,
) -> Path:
    """Secure obfuscation: bytecode-only, no recoverable source.

    Compiles with maximum optimization, strips debug info,
    and includes no source archive or recovery metadata.

    Args:
        source_dir: Project source directory.
        output_dir: Build output directory.
        ui: Optional BuildUI for progress display.
        optimize: Bytecode optimization level. Default 2 strips
                  docstrings and assert statements.

    Returns:
        Path to the output directory containing compiled files.
    """
    app_dir = output_dir / "app"
    app_dir.mkdir(parents=True, exist_ok=True)

    # Compile with specified optimization level
    if ui is not None:
        py_files = sorted(source_dir.rglob("*.py"))
        total = len(py_files)
        with ui.file_progress("Compiling source", total=total) as progress:
            task = progress.add_task("", total=total)

            def _cb(current: int, total: int, path: Path) -> None:
                progress.advance(task)

            compile_directory(source_dir, app_dir, optimize=optimize, progress_callback=_cb)
    else:
        compile_directory(source_dir, app_dir, optimize=optimize)

    # Write metadata marking this as secure (no source archive)
    metadata = {
        "coil_version": _get_version(),
        "secure": True,
        "timestamp": int(time.time()),
    }
    meta_path = app_dir / COIL_METADATA_FILENAME
    meta_path.write_text(json.dumps(metadata, indent=2), encoding="utf-8")

    return app_dir


def _get_version() -> str:
    """Get the current Coil version."""
    from coil import __version__
    return __version__

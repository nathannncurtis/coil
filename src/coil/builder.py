"""Build orchestration for Coil.

Coordinates the full build pipeline: dependency resolution, runtime setup,
compilation, obfuscation, and packaging.
"""

import shutil
import sys
import tempfile
from pathlib import Path

from coil.cli import detect_os, detect_python_version, resolve_entry_points
from coil.packager import install_dependencies, package_bundled, package_portable
from coil.resolver import resolve_dependencies
from coil.runtime import prepare_runtime


def build(
    project_dir: Path,
    entry_points: list[str],
    mode: str = "portable",
    target_os: str | None = None,
    python_version: str | None = None,
    gui: bool = False,
    secure: bool = False,
    exclude: list[str] | None = None,
    include: list[str] | None = None,
    output_dir: str = "./dist",
    name: str | None = None,
    icon: str | None = None,
    requirements: str | None = None,
    verbose: bool = False,
) -> list[Path]:
    """Execute the full build pipeline.

    Args:
        project_dir: Path to the project directory.
        entry_points: List of entry point scripts.
        mode: Build mode ("portable" or "bundled").
        target_os: Target OS.
        python_version: Target Python version.
        gui: GUI mode (no console window).
        secure: Secure obfuscation.
        exclude: Packages to exclude.
        include: Packages to force-include.
        output_dir: Output directory path.
        name: Application name.
        icon: Path to icon file.
        requirements: Explicit requirements file path.
        verbose: Verbose output.

    Returns:
        List of paths to created output files/directories.
    """
    target_os = target_os or detect_os()
    python_version = python_version or detect_python_version()
    name = name or project_dir.name
    out_path = Path(output_dir)

    _log(f"Building {name}...", verbose=True)
    _log(f"  Project:      {project_dir}", verbose)
    _log(f"  Entry points: {', '.join(entry_points)}", verbose)
    _log(f"  Mode:         {mode}", verbose)
    _log(f"  Target OS:    {target_os}", verbose)
    _log(f"  Python:       {python_version}", verbose)
    _log(f"  Secure:       {secure}", verbose)

    # Step 1: Resolve dependencies
    _log("Resolving dependencies...", verbose=True)
    packages = resolve_dependencies(
        project_dir=project_dir,
        python_version=python_version,
        requirements_path=requirements,
        exclude=exclude,
        include=include,
    )
    if packages:
        _log(f"  Found {len(packages)} dependencies: {', '.join(packages)}", verbose)
    else:
        _log("  No third-party dependencies found.", verbose)

    # Step 2: Download and prepare runtime
    _log("Preparing Python runtime...", verbose=True)
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)

        runtime_dir = prepare_runtime(
            python_version=python_version,
            dest_dir=tmp_path / "runtime",
            app_paths=["../app", "../lib"],
            verbose=verbose,
        )
        _log("  Runtime ready.", verbose)

        # Step 3: Install dependencies
        deps_dir = None
        if packages:
            _log("Installing dependencies...", verbose=True)
            deps_dir = tmp_path / "lib"
            install_dependencies(
                packages=packages,
                dest_dir=deps_dir,
                python_version=python_version,
                verbose=verbose,
            )
            _log("  Dependencies installed.", verbose)

        # Step 4: Package
        _log(f"Packaging ({mode} mode)...", verbose=True)
        if mode == "bundled":
            result = package_bundled(
                project_dir=project_dir,
                output_dir=out_path,
                runtime_dir=runtime_dir,
                entry_points=entry_points,
                name=name,
                target_os=target_os,
                gui=gui,
                secure=secure,
                icon=icon,
                deps_dir=deps_dir,
                verbose=verbose,
            )
            outputs = [result]
        else:
            outputs = package_portable(
                project_dir=project_dir,
                output_dir=out_path,
                runtime_dir=runtime_dir,
                entry_points=entry_points,
                name=name,
                target_os=target_os,
                gui=gui,
                secure=secure,
                icon=icon,
                deps_dir=deps_dir,
                verbose=verbose,
            )

    _log(f"Build complete!", verbose=True)
    for output in outputs:
        _log(f"  -> {output}", verbose=True)

    return outputs


def _log(message: str, verbose: bool) -> None:
    """Print a message if verbose or if it's a key status message."""
    if verbose:
        print(message)

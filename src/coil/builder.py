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
from coil.ui import BuildUI


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

    ui = BuildUI(verbose=verbose)
    ui.build_header(name, mode)

    if verbose:
        ui.detail(f"Project:      {project_dir}")
        ui.detail(f"Entry points: {', '.join(entry_points)}")
        ui.detail(f"Target OS:    {target_os}")
        ui.detail(f"Python:       {python_version}")
        ui.detail(f"Secure:       {secure}")

    # Step 1: Resolve dependencies
    ui.step("Resolving dependencies...")
    packages = resolve_dependencies(
        project_dir=project_dir,
        python_version=python_version,
        requirements_path=requirements,
        exclude=exclude,
        include=include,
    )
    if packages:
        ui.detail(f"Found {len(packages)} dependencies: {', '.join(packages)}")
    else:
        ui.detail("No third-party dependencies found.")

    # Step 2: Download and prepare runtime
    ui.step("Preparing runtime...")
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)

        runtime_dir = prepare_runtime(
            python_version=python_version,
            dest_dir=tmp_path / "runtime",
            app_paths=["../app", "../lib"],
            verbose=verbose,
            ui=ui,
        )

        # Step 3: Install dependencies
        deps_dir = None
        if packages:
            ui.step("Installing dependencies...")
            deps_dir = tmp_path / "lib"
            install_dependencies(
                packages=packages,
                dest_dir=deps_dir,
                python_version=python_version,
                verbose=verbose,
                ui=ui,
            )

        # Step 4: Package
        ui.step(f"Packaging ({mode})...")
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
                ui=ui,
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
                ui=ui,
            )

    ui.build_summary(outputs)

    return outputs


def _log(message: str, verbose: bool) -> None:
    """Print a message if verbose or if it's a key status message."""
    if verbose:
        print(message)

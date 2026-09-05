"""Build orchestration for Coil.

Coordinates the full build pipeline: dependency resolution, runtime setup,
compilation, obfuscation, and packaging.
"""

from __future__ import annotations

import hashlib
import shutil
import sys
import tempfile
from pathlib import Path

from coil.cli import detect_os, detect_python_version, get_cache_dir, resolve_entry_points
from coil.packager import install_dependencies, package_bundled, package_portable, _validate_build_paths
from coil.resolver import resolve_dependencies
from coil.runtime import prepare_runtime
from coil.ui import BuildUI


def _compute_deps_hash(
    packages: list[str], python_version: str | None = None,
    target_os: str = "windows", arch: str = "amd64",
) -> str:
    """Cache dependencies separately for each target runtime and architecture."""
    from coil.resolver import _requirement_name
    import re
    normalized = []
    for package in packages:
        value = package.strip()
        match = re.match(r"[A-Za-z0-9][A-Za-z0-9._-]*", value)
        # Requirement suffixes can contain case-sensitive URLs and markers.
        normalized.append(_requirement_name(value) + value[match.end():] if match else value)
    target = python_version or detect_python_version()
    content = "\n".join(["coil-deps-v2", target, target_os, arch, *sorted(normalized)])
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


def _get_clean_env_dir(deps_hash: str) -> Path:
    """Get the cache directory for a clean build environment."""
    return get_cache_dir() / "_clean_envs" / deps_hash


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
    clean: bool = False,
    optimize: int | None = None,
    versioninfo: dict[str, dict[str, str]] | None = None,
    subsystems: dict[str, str] | None = None,
    deps_auto: bool = True,
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
        clean: Build in a clean environment with only declared dependencies.
        optimize: Bytecode optimization level (0, 1, 2). None = auto.
        versioninfo: Resolved per-entry VERSIONINFO fields, keyed by entry stem.
        subsystems: Explicit per-entry PE subsystem overrides, keyed by
            entry stem. Values are "gui" or "console". Entries without an
            override fall through to the existing hybrid default.
        deps_auto: When True, auto-detect imports and union with declared
            dependencies. Reflects [build.dependencies].auto in coil.toml.

    Returns:
        List of paths to created output files/directories.
    """
    target_os = target_os or detect_os()
    python_version = python_version or detect_python_version()
    name = name or project_dir.name
    out_path = Path(output_dir)
    _validate_build_paths(project_dir, out_path, entry_points, name)
    excluded_paths = []
    try:
        out_path.resolve().relative_to(project_dir.resolve())
        excluded_paths = [out_path / name if out_path.resolve() == project_dir.resolve() else out_path]
    except ValueError:
        pass

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
        auto=deps_auto,
        warn=ui.warning,
        excluded_paths=excluded_paths,
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
            if clean:
                deps_hash = _compute_deps_hash(packages, python_version, target_os)
                clean_dir = _get_clean_env_dir(deps_hash)
                marker = clean_dir / ".coil_ready"

                if marker.is_file():
                    ui.step("Using cached clean environment...")
                    ui.detail(f"Cache hit: {deps_hash}")
                    deps_dir = clean_dir
                else:
                    ui.step("Creating clean build environment...")
                    clean_dir.parent.mkdir(parents=True, exist_ok=True)
                    # Publish a complete cache directory atomically; concurrent
                    # builds must never install into or delete each other's tree.
                    with tempfile.TemporaryDirectory(prefix=".install-", dir=clean_dir.parent) as staging:
                        staged_dir = Path(staging) / "lib"
                        install_dependencies(
                            packages=packages,
                            dest_dir=staged_dir,
                            python_version=python_version,
                            verbose=verbose,
                            ui=ui,
                        )
                        staged_dir.mkdir(parents=True, exist_ok=True)
                        (staged_dir / ".coil_ready").write_text(deps_hash, encoding="utf-8")
                        try:
                            staged_dir.rename(clean_dir)
                        except OSError:
                            if not marker.is_file():
                                raise RuntimeError(
                                    f"Incomplete dependency cache at {clean_dir}; "
                                    "build without --clean or remove that cache entry."
                                )
                    ui.detail(f"Cached clean environment: {deps_hash}")
                    deps_dir = clean_dir
            else:
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
                optimize=optimize,
                versioninfo=versioninfo,
                subsystems=subsystems,
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
                optimize=optimize,
                versioninfo=versioninfo,
                subsystems=subsystems,
            )

    ui.build_summary(outputs)

    return outputs


def _log(message: str, verbose: bool) -> None:
    """Print a message if verbose or if it's a key status message."""
    if verbose:
        print(message)

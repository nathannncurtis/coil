"""Lightweight build preview for Coil.

Shows what Coil detected in a project: entry point, dependencies,
estimated sizes, and config -- without building anything.
"""

from __future__ import annotations

import importlib.metadata
import sys
from pathlib import Path
from typing import Optional

from coil.cli import detect_python_version
from coil.resolver import resolve_dependencies
from coil.runtime import get_cached_zip_path, resolve_full_version
from coil.scanner import scan_project
from coil.utils.stdlib_list import get_stdlib_modules


def _get_dep_source(project_dir: Path, requirements_path: Optional[str] = None) -> str:
    """Determine where dependencies come from."""
    if requirements_path:
        return f"from {requirements_path}"
    if (project_dir / "requirements.txt").is_file():
        return "from requirements.txt"
    if (project_dir / "pyproject.toml").is_file():
        return "from pyproject.toml"
    return "from AST import scan"


def _get_installed_size(package_name: str) -> Optional[int]:
    """Try to get the installed size of a package using importlib.metadata."""
    try:
        dist = importlib.metadata.distribution(package_name)
        files = dist.files
        if files is None:
            return None
        total = 0
        for f in files:
            try:
                located = f.locate()
                if located.is_file():
                    total += located.stat().st_size
            except Exception:
                pass
        return total if total > 0 else None
    except importlib.metadata.PackageNotFoundError:
        return None
    except Exception:
        return None


def _get_installed_version(package_name: str) -> Optional[str]:
    """Get the installed version of a package."""
    try:
        return importlib.metadata.version(package_name)
    except Exception:
        return None


def _format_size(size_bytes: int) -> str:
    """Format bytes as human-readable size."""
    if size_bytes < 1024:
        return f"{size_bytes} B"
    elif size_bytes < 1024 * 1024:
        return f"{size_bytes / 1024:.1f} KB"
    elif size_bytes < 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f} MB"
    else:
        return f"{size_bytes / (1024 * 1024 * 1024):.1f} GB"


def _get_project_code_size(project_dir: Path) -> int:
    """Get total size of Python files in the project."""
    total = 0
    for f in project_dir.rglob("*.py"):
        try:
            total += f.stat().st_size
        except OSError:
            pass
    return total


def run_inspect(
    project_dir: Path,
    python_version: Optional[str] = None,
    requirements: Optional[str] = None,
    exclude: Optional[list[str]] = None,
    include: Optional[list[str]] = None,
    profile: Optional[str] = None,
) -> int:
    """Run the inspect analysis and print results.

    Returns 0 on success, 1 on error.
    """
    from rich.console import Console
    console = Console(highlight=False)

    python_version = python_version or detect_python_version()
    exclude = exclude or []
    include = include or []

    console.print(f"\n[bold]coil inspect[/bold]\n")
    console.print(f"Project: {project_dir}")

    # Entry point detection (without calling sys.exit)
    if (project_dir / "__main__.py").is_file():
        console.print("Entry point: __main__.py (auto-detected)")
    elif (project_dir / "main.py").is_file():
        console.print("Entry point: main.py (auto-detected)")
    else:
        py_files = sorted(f.name for f in project_dir.glob("*.py"))
        if py_files:
            console.print(f"[red]Entry point: not found[/red] (Python files: {', '.join(py_files)})")
        else:
            console.print("[red]Entry point: not found[/red]")
        console.print("  Use --entry to specify, or add __main__.py")

    console.print(f"Python target: {python_version}")

    # GUI mode detection
    from coil.scanner import detect_gui_imports
    gui_imports = detect_gui_imports(project_dir)
    if gui_imports:
        console.print(f"GUI mode: auto-detected (found: {', '.join(gui_imports)})")
    else:
        console.print("GUI mode: no (console app)")

    # Dependencies
    dep_source = _get_dep_source(project_dir, requirements)
    console.print(f"\n[bold]Dependencies[/bold] ({dep_source}):")

    # Get stdlib imports
    all_imports = scan_project(project_dir)
    stdlib = get_stdlib_modules(python_version)
    stdlib_used = sorted(all_imports & stdlib)

    if stdlib_used:
        console.print(f"  stdlib: {', '.join(stdlib_used[:10])}"
                      + (f" ... ({len(stdlib_used)} total)" if len(stdlib_used) > 10 else f" ({len(stdlib_used)} modules)"))
    else:
        console.print("  stdlib: (none detected)")

    # Third-party
    packages = resolve_dependencies(
        project_dir=project_dir,
        python_version=python_version,
        requirements_path=requirements,
        exclude=exclude,
        include=include,
    )

    total_deps_size = 0
    if packages:
        console.print("  third-party:")
        for pkg in packages:
            version = _get_installed_version(pkg)
            size = _get_installed_size(pkg)
            parts = [f"    {pkg}"]
            if version:
                parts.append(f"({version})")
            if size is not None:
                total_deps_size += size
                parts.append(f"- {_format_size(size)}")
            console.print(" ".join(parts))
    else:
        console.print("  third-party: (none)")

    if exclude:
        console.print(f"  excluded: {', '.join(exclude)}")
    else:
        console.print("  excluded: none")

    if include:
        console.print(f"  force-included: {', '.join(include)}")
    else:
        console.print("  force-included: none")

    # Size estimates
    console.print(f"\n[bold]Estimated output size:[/bold]")

    # Runtime size
    runtime_size = 0
    try:
        full_version = resolve_full_version(python_version)
        zip_path = get_cached_zip_path(full_version)
        if zip_path.is_file():
            runtime_size = zip_path.stat().st_size
    except Exception:
        pass

    if runtime_size:
        console.print(f"  Embeddable Python runtime: ~{_format_size(runtime_size)}")
    else:
        console.print("  Embeddable Python runtime: ~15 MB (estimate)")
        runtime_size = 15 * 1024 * 1024

    if total_deps_size:
        console.print(f"  Third-party dependencies: ~{_format_size(total_deps_size)}")
    elif packages:
        console.print(f"  Third-party dependencies: (install packages to estimate)")
    else:
        console.print(f"  Third-party dependencies: 0 B")

    project_size = _get_project_code_size(project_dir)
    console.print(f"  Project code: ~{_format_size(project_size)}")

    total = runtime_size + total_deps_size + project_size
    # Portable adds ~5% overhead for zip packaging
    portable_est = int(total * 1.05)
    console.print(f"  Estimated total: ~{_format_size(portable_est)} (portable), ~{_format_size(total)} (bundled)")

    # Config
    console.print()
    toml_path = project_dir / "coil.toml"
    if toml_path.is_file():
        profile_str = f" (profile: {profile})" if profile else " (profile: default)"
        console.print(f"Config: coil.toml found{profile_str}")
    else:
        console.print("Config: no coil.toml found")

    console.print()
    return 0

"""Pre-build diagnostics for Coil.

Checks for blocking issues before a build: Python version, runtime
availability, write permissions, config validity, and known package issues.
"""

from __future__ import annotations

import os
import sys
import urllib.request
from pathlib import Path
from typing import Optional

from coil.cli import detect_os, detect_python_version, get_cache_dir
from coil.runtime import get_cached_zip_path, get_embed_url, resolve_full_version
from coil.utils.compat import KNOWN_ISSUES


class CheckResult:
    """Result of a single diagnostic check."""

    def __init__(self, status: str, message: str) -> None:
        self.status = status  # "pass", "warn", "fail"
        self.message = message


def run_doctor(project_dir: Path, verbose: bool = False) -> int:
    """Run all diagnostic checks and print results.

    Returns 0 if all checks pass (warnings OK), 1 if any fail.
    """
    results: list[CheckResult] = []

    # 1. Python version
    results.append(_check_python_version())

    # 2. Embeddable runtime
    results.append(_check_runtime(verbose))

    # 3. Write permissions
    results.append(_check_output_writable(project_dir))
    results.append(_check_cache_writable())

    # 4. coil.toml validation (if present)
    results.extend(_check_config(project_dir))

    # 5. Known incompatible packages
    results.extend(_check_packages(project_dir))

    # Print results using rich for Unicode/color safety
    from rich.console import Console
    console = Console(highlight=False)

    for r in results:
        if r.status == "pass":
            console.print(f"[green]\\u2713[/green] {r.message}")
        elif r.status == "warn":
            console.print(f"[yellow]\\u26a0[/yellow] {r.message}")
        else:
            console.print(f"[red]\\u2717[/red] {r.message}")

    passed = sum(1 for r in results if r.status == "pass")
    warned = sum(1 for r in results if r.status == "warn")
    failed = sum(1 for r in results if r.status == "fail")

    parts = []
    if passed:
        parts.append(f"{passed} passed")
    if warned:
        parts.append(f"{warned} warning{'s' if warned != 1 else ''}")
    if failed:
        parts.append(f"{failed} error{'s' if failed != 1 else ''}")

    console.print(f"\n{', '.join(parts)}")

    return 1 if failed > 0 else 0


def _check_python_version() -> CheckResult:
    """Check that a compatible Python version is available."""
    ver = detect_python_version()
    major, minor = sys.version_info.major, sys.version_info.minor
    if major < 3 or (major == 3 and minor < 9):
        return CheckResult("fail", f"Python {ver} detected — Coil requires 3.9+")
    return CheckResult("pass", f"Python {ver} detected")


def _check_runtime(verbose: bool = False) -> CheckResult:
    """Check that the embeddable runtime is available or downloadable."""
    ver = detect_python_version()
    try:
        full_version = resolve_full_version(ver)
    except RuntimeError:
        return CheckResult("fail", f"No embeddable Python {ver} distribution found at python.org")

    zip_path = get_cached_zip_path(full_version)
    if zip_path.is_file():
        return CheckResult("pass", f"Embeddable runtime cached ({zip_path.name})")

    # Check if URL is reachable
    url = get_embed_url(full_version)
    try:
        req = urllib.request.Request(url, method="HEAD")
        resp = urllib.request.urlopen(req, timeout=10)
        if resp.status == 200:
            return CheckResult("pass", f"Embeddable runtime available for download ({full_version})")
    except Exception:
        pass

    return CheckResult("fail", f"Cannot reach embeddable runtime URL: {url}")


def _check_output_writable(project_dir: Path) -> CheckResult:
    """Check that the output directory is writable."""
    # Check default output dir relative to project
    out_dir = project_dir / "dist"
    # Try to write to the parent if dist doesn't exist
    test_dir = out_dir if out_dir.is_dir() else project_dir
    try:
        test_file = test_dir / ".coil_write_test"
        test_file.write_text("test")
        test_file.unlink()
        return CheckResult("pass", f"Output directory is writable")
    except OSError:
        return CheckResult("fail", f"Cannot write to output directory ({test_dir})")


def _check_cache_writable() -> CheckResult:
    """Check that the cache directory is writable."""
    cache = get_cache_dir()
    try:
        cache.mkdir(parents=True, exist_ok=True)
        test_file = cache / ".coil_write_test"
        test_file.write_text("test")
        test_file.unlink()
        return CheckResult("pass", "Cache directory writable")
    except OSError:
        return CheckResult("fail", f"Cannot write to cache directory ({cache})")


def _check_config(project_dir: Path) -> list[CheckResult]:
    """Validate coil.toml if present."""
    results: list[CheckResult] = []
    toml_path = project_dir / "coil.toml"

    if not toml_path.is_file():
        return results  # No toml is fine, not an error

    from coil.config import load_config, get_build_config

    try:
        raw = load_config(project_dir)
    except Exception as e:
        results.append(CheckResult("fail", f"coil.toml parse error: {e}"))
        return results

    if raw is None:
        return results

    results.append(CheckResult("pass", "coil.toml is valid"))

    try:
        config = get_build_config(raw)
    except Exception as e:
        results.append(CheckResult("fail", f"coil.toml config error: {e}"))
        return results

    # Check entry point exists
    entry = config.get("entry", "")
    if entry:
        entry_path = project_dir / entry
        if entry_path.is_file():
            results.append(CheckResult("pass", f"Entry point {entry} exists"))
        else:
            results.append(CheckResult("fail", f"Entry point {entry} not found"))
    else:
        # Try auto-detect
        if (project_dir / "__main__.py").is_file():
            results.append(CheckResult("pass", "Entry point __main__.py exists (auto-detected)"))
        elif (project_dir / "main.py").is_file():
            results.append(CheckResult("pass", "Entry point main.py exists (auto-detected)"))
        else:
            results.append(CheckResult("fail", "No entry point found — set entry in coil.toml or add __main__.py"))

    # Check icon exists
    icon = config.get("icon", "")
    if icon:
        icon_path = project_dir / icon if not Path(icon).is_absolute() else Path(icon)
        if icon_path.is_file():
            results.append(CheckResult("pass", f"Icon file {icon} exists"))
        else:
            results.append(CheckResult("fail", f"Icon file {icon} not found"))

    return results


def _check_packages(project_dir: Path) -> list[CheckResult]:
    """Check for known incompatible packages in the project's dependencies."""
    results: list[CheckResult] = []

    # Collect dependency names from requirements.txt or pyproject.toml
    packages: list[str] = []

    req_file = project_dir / "requirements.txt"
    if req_file.is_file():
        for line in req_file.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line and not line.startswith("#") and not line.startswith("-"):
                # Strip version specifiers
                name = line.split("==")[0].split(">=")[0].split("<=")[0].split("!=")[0].split("[")[0].strip()
                if name:
                    packages.append(name.lower())

    # Check against known issues
    for pkg in packages:
        for known_name, warning in KNOWN_ISSUES.items():
            if pkg == known_name.lower():
                results.append(CheckResult("warn", f"Package '{known_name}' {warning}"))

    return results

"""Packaging logic for portable and bundled build modes.

Portable: single self-extracting archive containing runtime + app + deps.
Bundled: clean directory structure with compiled files.
"""

import os
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

from coil.obfuscator import obfuscate_default, obfuscate_secure
from coil.platforms import get_handler


def package_bundled(
    project_dir: Path,
    output_dir: Path,
    runtime_dir: Path,
    entry_points: list[str],
    name: str,
    target_os: str,
    gui: bool = False,
    secure: bool = False,
    icon: str | None = None,
    deps_dir: Path | None = None,
    verbose: bool = False,
) -> Path:
    """Create a bundled build: directory with compiled files.

    Produces a directory containing:
    - Runtime (embedded Python)
    - Compiled application code (.pyc)
    - Bundled dependencies
    - Launcher for each entry point

    No loose .py files in output.

    Args:
        project_dir: Source project directory.
        output_dir: Where to create the bundle.
        runtime_dir: Path to the extracted embedded runtime.
        entry_points: List of entry point scripts.
        name: Application name.
        target_os: Target OS.
        gui: GUI mode (no console).
        secure: Secure obfuscation.
        icon: Icon file path.
        deps_dir: Directory containing installed dependencies.
        verbose: Verbose output.

    Returns:
        Path to the bundled output directory.
    """
    bundle_dir = output_dir / name
    if bundle_dir.exists():
        shutil.rmtree(bundle_dir)
    bundle_dir.mkdir(parents=True)

    if verbose:
        print(f"Creating bundled build in {bundle_dir}")

    # Copy runtime
    runtime_dest = bundle_dir / "runtime"
    shutil.copytree(runtime_dir, runtime_dest)
    if verbose:
        print("  Copied runtime")

    # Obfuscate and compile source
    if secure:
        app_dir = obfuscate_secure(project_dir, bundle_dir)
    else:
        app_dir = obfuscate_default(project_dir, bundle_dir)
    if verbose:
        print(f"  Compiled source ({'secure' if secure else 'default'} mode)")

    # Copy dependencies
    if deps_dir and deps_dir.is_dir():
        lib_dir = bundle_dir / "lib"
        shutil.copytree(deps_dir, lib_dir)
        _remove_py_files(lib_dir)
        if verbose:
            print("  Bundled dependencies")

    # Create launchers
    handler = get_handler(target_os)
    for entry in entry_points:
        entry_pyc = entry.replace(".py", ".pyc")
        entry_name = Path(entry).stem if len(entry_points) > 1 else name
        handler.create_launcher(
            output_dir=bundle_dir,
            entry_point=entry_pyc,
            name=entry_name,
            gui=gui,
            icon=icon,
        )
        if verbose:
            print(f"  Created launcher for {entry}")

    # Apply icon to the main exe if provided (bundled mode)
    if icon and sys.platform == "win32":
        # Find the exe created by the launcher — use the app name
        main_exe = bundle_dir / f"{name}.exe"
        if main_exe.is_file():
            from coil.platforms.windows import set_exe_icon
            try:
                set_exe_icon(main_exe, Path(icon))
                if verbose:
                    print(f"  Applied icon: {icon}")
            except Exception as e:
                if verbose:
                    print(f"  Warning: Could not apply icon: {e}")

    if verbose:
        print(f"Bundled build complete: {bundle_dir}")

    return bundle_dir


def package_portable(
    project_dir: Path,
    output_dir: Path,
    runtime_dir: Path,
    entry_points: list[str],
    name: str,
    target_os: str,
    gui: bool = False,
    secure: bool = False,
    icon: str | None = None,
    deps_dir: Path | None = None,
    verbose: bool = False,
) -> list[Path]:
    """Create a portable build: self-contained directory per entry point.

    Produces a directory containing the embedded Python runtime renamed
    to the application name, with all code and dependencies bundled.
    The .exe is a real Python interpreter so it runs natively on Windows.

    Args:
        project_dir: Source project directory.
        output_dir: Where to create the output.
        runtime_dir: Path to the extracted embedded runtime.
        entry_points: List of entry point scripts.
        name: Application name.
        target_os: Target OS.
        gui: GUI mode.
        secure: Secure obfuscation.
        icon: Icon file path.
        deps_dir: Directory containing installed dependencies.
        verbose: Verbose output.

    Returns:
        List of paths to created portable executables.
    """
    output_dir.mkdir(parents=True, exist_ok=True)
    results: list[Path] = []

    for entry in entry_points:
        entry_pyc = entry.replace(".py", ".pyc")
        entry_name = Path(entry).stem if len(entry_points) > 1 else name

        if verbose:
            print(f"Packaging portable exe for {entry}...")

        # Create the portable directory
        portable_dir = output_dir / entry_name
        if portable_dir.exists():
            shutil.rmtree(portable_dir)
        portable_dir.mkdir(parents=True)

        # Copy the entire runtime into the portable dir
        for item in runtime_dir.iterdir():
            dest = portable_dir / item.name
            if item.is_dir():
                shutil.copytree(item, dest)
            else:
                shutil.copy2(item, dest)

        # Pick the right Python exe: pythonw.exe for GUI, python.exe for console
        source_exe_name = "pythonw.exe" if gui else "python.exe"
        source_exe = portable_dir / source_exe_name
        target_exe = portable_dir / f"{entry_name}.exe"

        if source_exe.is_file():
            shutil.copy2(source_exe, target_exe)
        else:
            # Fallback: just copy python.exe
            fallback = portable_dir / "python.exe"
            if fallback.is_file():
                shutil.copy2(fallback, target_exe)

        # Remove the original python exes — only keep the renamed app exe
        for leftover in ("python.exe", "pythonw.exe"):
            lf = portable_dir / leftover
            if lf.is_file() and lf.name != target_exe.name:
                lf.unlink()

        # Remove unnecessary runtime files to keep the directory clean
        for junk in ("LICENSE.txt", "python.cat"):
            jf = portable_dir / junk
            if jf.is_file():
                jf.unlink()

        # Obfuscate and compile source into app/ subdirectory
        if secure:
            app_dir = obfuscate_secure(project_dir, portable_dir)
        else:
            app_dir = obfuscate_default(project_dir, portable_dir)
        if verbose:
            print(f"  Compiled source ({'secure' if secure else 'default'} mode)")

        # Copy dependencies into lib/ subdirectory
        if deps_dir and deps_dir.is_dir():
            lib_dir = portable_dir / "lib"
            shutil.copytree(deps_dir, lib_dir)
            _remove_py_files(lib_dir)
            if verbose:
                print("  Bundled dependencies")

        # Create the bootstrap __main__.py that the exe will run
        bootstrap = _generate_bootstrap_script(entry_pyc)
        bootstrap_path = portable_dir / f"_boot_{entry_name}.py"
        bootstrap_path.write_text(bootstrap, encoding="utf-8")

        # Configure the ._pth file so the exe finds everything
        _configure_portable_pth(portable_dir, entry_name)

        # If GUI mode, set PE subsystem on the copied exe
        if gui and target_exe.is_file():
            from coil.platforms.windows import set_pe_subsystem
            try:
                set_pe_subsystem(target_exe, gui=True)
            except Exception:
                pass  # Best effort — pythonw.exe already has GUI subsystem

        # Apply icon if provided
        if icon and target_exe.is_file() and sys.platform == "win32":
            from coil.platforms.windows import set_exe_icon
            try:
                set_exe_icon(target_exe, Path(icon))
                if verbose:
                    print(f"  Applied icon to exe: {icon}")
            except Exception as e:
                if verbose:
                    print(f"  Warning: Could not apply icon to exe: {e}")

        # Copy non-code project assets (icons, images, configs, etc.)
        # so the app can find them at runtime via relative paths
        _copy_project_assets(project_dir, portable_dir, verbose)

        results.append(target_exe)
        if verbose:
            total_size = sum(
                f.stat().st_size for f in portable_dir.rglob("*") if f.is_file()
            )
            size_mb = total_size / (1024 * 1024)
            print(f"  Created {target_exe} ({size_mb:.1f} MB)")

    return results


def install_dependencies(
    packages: list[str],
    dest_dir: Path,
    python_version: str | None = None,
    verbose: bool = False,
) -> Path:
    """Install dependencies into a directory for bundling.

    Args:
        packages: List of PyPI package names.
        dest_dir: Directory to install into.
        python_version: Target Python version.
        verbose: Verbose output.

    Returns:
        Path to the directory containing installed packages.
    """
    if not packages:
        return dest_dir

    dest_dir.mkdir(parents=True, exist_ok=True)

    cmd = [
        sys.executable, "-m", "pip", "install",
        "--target", str(dest_dir),
        "--no-user",
        "--disable-pip-version-check",
    ]

    if not verbose:
        cmd.append("--quiet")

    cmd.extend(packages)

    if verbose:
        print(f"Installing dependencies: {', '.join(packages)}")

    try:
        subprocess.run(cmd, check=True, capture_output=not verbose)
    except subprocess.CalledProcessError as e:
        raise RuntimeError(
            f"Failed to install dependencies: {e}\n"
            "Check that all package names are correct."
        ) from e

    # Remove unnecessary files to reduce size
    _strip_installed_packages(dest_dir)

    return dest_dir


def _add_dir_to_zip(
    zf: zipfile.ZipFile,
    source_dir: Path,
    archive_prefix: str,
) -> None:
    """Add a directory tree to a zip file."""
    for file_path in source_dir.rglob("*"):
        if file_path.is_file():
            arcname = f"{archive_prefix}/{file_path.relative_to(source_dir)}"
            zf.write(file_path, arcname)


def _generate_bootstrap_script(entry_point: str) -> str:
    """Generate the bootstrap script that runs the entry point.

    This script is imported by the ._pth file when the exe starts.
    It sets up sys.path, loads the entry point, and exits.
    """
    return f'''\
import os
import sys

_base = os.path.dirname(os.path.abspath(__file__))
os.chdir(_base)

_app = os.path.join(_base, "app")
_lib = os.path.join(_base, "lib")

if _app not in sys.path:
    sys.path.insert(0, _app)
if os.path.isdir(_lib) and _lib not in sys.path:
    sys.path.insert(0, _lib)

_entry = os.path.join(_app, "{entry_point}")

try:
    if _entry.endswith(".pyc"):
        import importlib.util
        spec = importlib.util.spec_from_file_location("__main__", _entry)
        if spec and spec.loader:
            mod = importlib.util.module_from_spec(spec)
            mod.__name__ = "__main__"
            mod.__file__ = _entry
            sys.modules["__main__"] = mod
            spec.loader.exec_module(mod)
    else:
        with open(_entry) as f:
            exec(compile(f.read(), _entry, "exec"), {{"__name__": "__main__", "__file__": _entry}})
except SystemExit:
    raise
except Exception as e:
    print(f"Error: {{e}}", file=sys.stderr)
    sys.exit(1)

sys.exit(0)
'''


def _configure_portable_pth(portable_dir: Path, entry_name: str) -> None:
    """Configure the ._pth file so the portable exe finds the bootstrap script.

    The ._pth file controls sys.path for the embeddable distribution.
    We configure it to include the app and lib directories, and to
    auto-run the bootstrap script via an import line.
    """
    pth_files = list(portable_dir.glob("python*._pth"))
    if not pth_files:
        return

    # Use the longest-named pth file (python313._pth, not python3._pth)
    pth_file = sorted(pth_files, key=lambda p: len(p.name), reverse=True)[0]
    ver_tag = _get_python_ver_tag(portable_dir)
    boot_module = f"_boot_{entry_name}"

    pth_file.write_text(
        f"python{ver_tag}.zip\n"
        f".\n"
        f"app\n"
        f"lib\n"
        f"import site\n",
        encoding="utf-8",
    )

    # Create sitecustomize.py — auto-imported by the site module.
    # This runs the bootstrap script when the exe starts.
    site_custom = portable_dir / "sitecustomize.py"
    site_custom.write_text(
        f"import os, sys\n"
        f"_base = os.path.dirname(os.path.abspath(__file__))\n"
        f"_boot = os.path.join(_base, '_boot_{entry_name}.py')\n"
        f"if os.path.isfile(_boot):\n"
        f"    exec(compile(open(_boot).read(), _boot, 'exec'))\n",
        encoding="utf-8",
    )


def _get_python_ver_tag(runtime_dir: Path) -> str:
    """Extract the python version tag (e.g. '313') from the runtime directory.

    Looks for the versioned DLL like python313.dll (not python3.dll).
    """
    best_tag = ""
    for f in runtime_dir.glob("python*.dll"):
        name = f.stem  # e.g. "python313"
        tag = name.replace("python", "")
        if tag and tag.isdigit() and len(tag) > len(best_tag):
            best_tag = tag
    return best_tag


def _copy_project_assets(project_dir: Path, dest_dir: Path, verbose: bool = False) -> None:
    """Copy non-code assets from the project directory into the output.

    Copies files like .ico, .png, .json, .cfg, etc. that the app may
    reference at runtime via relative paths. Skips .py files (already
    compiled) and common non-asset patterns.
    """
    skip_extensions = {".py", ".pyc", ".pyo", ".pyd"}
    skip_names = {"__pycache__", ".git", ".venv", "venv", ".env", "node_modules",
                  "dist", "build", ".mypy_cache", ".pytest_cache", ".tox"}

    for item in project_dir.iterdir():
        if item.name in skip_names:
            continue
        if item.is_file() and item.suffix.lower() not in skip_extensions:
            dest = dest_dir / item.name
            if not dest.exists():
                shutil.copy2(item, dest)
                if verbose:
                    print(f"  Copied asset: {item.name}")
        elif item.is_dir() and item.name not in skip_names:
            # Copy asset directories (e.g. assets/, images/, resources/)
            dest = dest_dir / item.name
            if not dest.exists():
                # Only copy if it contains non-Python files
                has_assets = any(
                    f.suffix.lower() not in skip_extensions
                    for f in item.rglob("*") if f.is_file()
                )
                if has_assets:
                    shutil.copytree(item, dest)
                    if verbose:
                        print(f"  Copied asset directory: {item.name}/")


def _remove_py_files(directory: Path) -> None:
    """Remove all .py files from a directory, keeping .pyc files."""
    for py_file in directory.rglob("*.py"):
        # Keep __init__.py files as some packages need them
        # but compile them to .pyc first
        pyc = py_file.with_suffix(".pyc")
        if not pyc.exists():
            import py_compile
            try:
                py_compile.compile(str(py_file), cfile=str(pyc), doraise=False)
            except Exception:
                pass
        py_file.unlink()


def _strip_installed_packages(dest_dir: Path) -> None:
    """Remove unnecessary files from installed packages to reduce size."""
    patterns_to_remove = [
        "*.dist-info",
        "__pycache__",
        "tests",
        "test",
        "docs",
        "doc",
        "examples",
    ]

    for pattern in patterns_to_remove:
        for match in dest_dir.rglob(pattern):
            if match.is_dir():
                shutil.rmtree(match, ignore_errors=True)

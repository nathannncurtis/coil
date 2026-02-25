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
    """Generate the bootstrap script that runs the entry point."""
    return f'''\
import os
import sys

_base = os.path.dirname(os.path.abspath(__file__))
_app = os.path.join(_base, "app")
_lib = os.path.join(_base, "lib")

if _app not in sys.path:
    sys.path.insert(0, _app)
if os.path.isdir(_lib) and _lib not in sys.path:
    sys.path.insert(0, _lib)

_entry = os.path.join(_app, "{entry_point}")
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
'''


def _configure_portable_pth(portable_dir: Path, entry_name: str) -> None:
    """Configure the ._pth file so the portable exe finds the bootstrap script.

    The ._pth file controls sys.path for the embeddable distribution.
    We configure it to include the app and lib directories, and to run
    the bootstrap script.
    """
    pth_files = list(portable_dir.glob("python*._pth"))
    if not pth_files:
        return

    pth_file = pth_files[0]
    boot_script = f"_boot_{entry_name}.py"

    pth_file.write_text(
        f"python{_get_python_ver_tag(portable_dir)}.zip\n"
        f".\n"
        f"app\n"
        f"lib\n"
        f"import site\n",
        encoding="utf-8",
    )

    # Create a sitecustomize.py that runs the bootstrap on startup
    site_custom = portable_dir / "sitecustomize.py"
    site_custom.write_text(
        f"import os, sys\n"
        f"_base = os.path.dirname(os.path.abspath(__file__))\n"
        f"_boot = os.path.join(_base, '{boot_script}')\n"
        f"if os.path.isfile(_boot) and '--help' not in sys.argv:\n"
        f"    exec(open(_boot).read())\n"
        f"    sys.exit(0)\n",
        encoding="utf-8",
    )


def _get_python_ver_tag(runtime_dir: Path) -> str:
    """Extract the python version tag (e.g. '313') from the runtime directory."""
    for f in runtime_dir.glob("python*.dll"):
        name = f.stem  # e.g. "python313"
        tag = name.replace("python", "")
        if tag and tag.isdigit():
            return tag
    return ""


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

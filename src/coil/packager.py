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
    """Create a portable build: single self-contained archive per entry point.

    Each entry point produces its own standalone executable that
    extracts to a temp directory and runs.

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
    import tempfile

    output_dir.mkdir(parents=True, exist_ok=True)
    results: list[Path] = []

    # Build in a temp directory first
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = Path(tmp)

        # Obfuscate source
        if secure:
            app_dir = obfuscate_secure(project_dir, tmp_path)
        else:
            app_dir = obfuscate_default(project_dir, tmp_path)

        for entry in entry_points:
            entry_pyc = entry.replace(".py", ".pyc")
            entry_name = Path(entry).stem if len(entry_points) > 1 else name

            if verbose:
                print(f"Packaging portable exe for {entry}...")

            # Create a zip containing everything
            zip_name = f"{entry_name}.zip"
            zip_path = output_dir / zip_name
            exe_path = output_dir / f"{entry_name}.exe"

            with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED) as zf:
                # Add runtime
                _add_dir_to_zip(zf, runtime_dir, "runtime")

                # Add compiled app
                _add_dir_to_zip(zf, app_dir, "app")

                # Add dependencies
                if deps_dir and deps_dir.is_dir():
                    _add_dir_to_zip(zf, deps_dir, "lib")

                # Add launcher script
                handler = get_handler(target_os)
                launcher_content = _generate_portable_launcher(
                    entry_pyc, gui
                )
                zf.writestr("__main__.py", launcher_content)

            # Create self-extracting exe by prepending a stub
            _create_self_extracting_exe(zip_path, exe_path, gui, verbose)

            # Remove the intermediate zip
            if zip_path.exists() and exe_path.exists():
                zip_path.unlink()

            results.append(exe_path)
            if verbose:
                size_mb = exe_path.stat().st_size / (1024 * 1024)
                print(f"  Created {exe_path} ({size_mb:.1f} MB)")

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


def _generate_portable_launcher(entry_point: str, gui: bool) -> str:
    """Generate the launcher script embedded in the portable archive."""
    return f'''\
import os
import sys
import tempfile
import zipfile

def main():
    # Get the path to this archive
    archive = os.path.dirname(os.path.abspath(__file__))
    if not os.path.isdir(os.path.join(archive, "runtime")):
        # We're inside a zip/exe, need to extract
        exe_path = sys.executable if getattr(sys, "frozen", False) else sys.argv[0]
        cache_dir = os.path.join(tempfile.gettempdir(), "coil_cache")
        app_name = os.path.splitext(os.path.basename(exe_path))[0]
        extract_dir = os.path.join(cache_dir, app_name)

        if not os.path.isdir(os.path.join(extract_dir, "runtime")):
            os.makedirs(extract_dir, exist_ok=True)
            with zipfile.ZipFile(exe_path, "r") as zf:
                zf.extractall(extract_dir)

        archive = extract_dir

    runtime_python = os.path.join(archive, "runtime", "python.exe")
    app_dir = os.path.join(archive, "app")
    lib_dir = os.path.join(archive, "lib")
    launcher = os.path.join(archive, "app", "{entry_point}")

    env = os.environ.copy()
    paths = [app_dir]
    if os.path.isdir(lib_dir):
        paths.append(lib_dir)
    env["PYTHONPATH"] = os.pathsep.join(paths)

    import subprocess
    cmd = [runtime_python, "-c",
        "import importlib.util, sys; "
        "spec = importlib.util.spec_from_file_location(\\'__main__\\', r\\'"+launcher+"\\'); "
        "mod = importlib.util.module_from_spec(spec); "
        "sys.modules[\\'__main__\\'] = mod; "
        "spec.loader.exec_module(mod)"
    ]

    sys.exit(subprocess.call(cmd, env=env))

if __name__ == "__main__":
    main()
'''


def _create_self_extracting_exe(
    zip_path: Path,
    exe_path: Path,
    gui: bool,
    verbose: bool,
) -> None:
    """Create a self-extracting executable from a zip file.

    Prepends a small stub to the zip that extracts and runs the content.
    """
    # For now, create a batch+zip combo that self-extracts
    stub = _generate_sfx_stub(gui)

    with open(exe_path, "wb") as out:
        out.write(stub.encode("utf-8"))
        with open(zip_path, "rb") as zf:
            shutil.copyfileobj(zf, out)


def _generate_sfx_stub(gui: bool) -> str:
    """Generate the self-extracting stub script."""
    return (
        '@echo off\n'
        'setlocal\n'
        'set "TMPDIR=%TEMP%\\coil_sfx_%~n0"\n'
        'if not exist "%TMPDIR%\\runtime\\python.exe" (\n'
        '    mkdir "%TMPDIR%" 2>nul\n'
        '    powershell -Command "'
        "Expand-Archive -Path '%~f0' -DestinationPath '%TMPDIR%' -Force"
        '"\n'
        ')\n'
        '"%TMPDIR%\\runtime\\python.exe" "%TMPDIR%\\__main__.py" %*\n'
        'exit /b %errorlevel%\n'
        '\n'
    )


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

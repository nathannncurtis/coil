"""Packaging logic for portable and bundled build modes.

Portable: single-file .exe (bootloader + zip of runtime/app/deps).
Bundled: directory containing the exe and all supporting files.
"""

import io
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from coil.obfuscator import obfuscate_default, obfuscate_secure
from coil.platforms import get_handler

# Trailer magic appended after the zip data in portable exes.
_COIL_MAGIC = 0x434F494C  # "COIL"

# DLLs the python exe links against at startup — must stay next to the exe.
_EXE_DLLS = {
    "python3.dll", "python313.dll", "python312.dll", "python311.dll",
    "python310.dll", "python39.dll", "vcruntime140.dll", "vcruntime140_1.dll",
}


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
    """Create a portable build: single self-contained .exe per entry point.

    Builds the full application directory in a temp location, zips it,
    and appends it to the pre-compiled bootloader stub. The resulting
    exe extracts on first launch and runs the app directly.

    Args:
        project_dir: Source project directory.
        output_dir: Where to place the output exe(s).
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
        entry_name = Path(entry).stem if len(entry_points) > 1 else name

        if verbose:
            print(f"Packaging portable exe for {entry}...")

        # Step 1: Build the full directory in a temp location
        with tempfile.TemporaryDirectory() as tmp:
            stage_dir = Path(tmp) / entry_name
            _build_app_directory(
                project_dir=project_dir,
                stage_dir=stage_dir,
                runtime_dir=runtime_dir,
                entry_point=entry,
                entry_name=entry_name,
                gui=gui,
                secure=secure,
                icon=icon,
                deps_dir=deps_dir,
                verbose=verbose,
            )

            # Step 2: Zip the staged directory
            if verbose:
                print("  Creating portable archive...")
            zip_data = _zip_directory(stage_dir)

        # Step 3: Combine bootloader stub + zip + trailer
        from coil.bootloader import BOOTLOADER_STUB
        stub = bytearray(BOOTLOADER_STUB)

        zip_offset = len(stub)
        trailer = struct.pack("<II", zip_offset, _COIL_MAGIC)
        exe_data = bytes(stub) + zip_data + trailer

        target_exe = output_dir / f"{entry_name}.exe"
        target_exe.write_bytes(exe_data)

        # Step 4: Apply icon to the final portable exe
        if icon and sys.platform == "win32":
            from coil.platforms.windows import set_exe_icon
            try:
                set_exe_icon(target_exe, Path(icon))
                if verbose:
                    print(f"  Applied icon: {icon}")
            except Exception as e:
                if verbose:
                    print(f"  Warning: Could not apply icon to exe: {e}")

        results.append(target_exe)
        if verbose:
            size_mb = target_exe.stat().st_size / (1024 * 1024)
            print(f"  Created {target_exe} ({size_mb:.1f} MB)")

    return results


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
    """Create a bundled build: directory with exe and supporting files.

    Produces a clean directory with the app exe at root, project assets
    alongside it, and all runtime internals tucked into _internal/.

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

    if verbose:
        print(f"Creating bundled build in {bundle_dir}")

    # Build the full directory for the first (primary) entry point
    entry = entry_points[0]
    entry_name = Path(entry).stem if len(entry_points) > 1 else name
    _build_app_directory(
        project_dir=project_dir,
        stage_dir=bundle_dir,
        runtime_dir=runtime_dir,
        entry_point=entry,
        entry_name=entry_name,
        gui=gui,
        secure=secure,
        icon=icon,
        deps_dir=deps_dir,
        verbose=verbose,
    )

    # For multiple entry points, create additional launchers
    if len(entry_points) > 1:
        for extra_entry in entry_points[1:]:
            extra_name = Path(extra_entry).stem
            extra_pyc = extra_entry.replace(".py", ".pyc")

            # Copy python exe as the extra entry
            source_exe_name = "pythonw.exe" if gui else "python.exe"
            source_exe = runtime_dir / source_exe_name
            extra_exe = bundle_dir / f"{extra_name}.exe"
            if source_exe.is_file():
                shutil.copy2(source_exe, extra_exe)

            # Create bootstrap for this entry
            bootstrap = _generate_bootstrap_script(extra_pyc)
            internal_dir = bundle_dir / "_internal"
            (internal_dir / f"_boot_{extra_name}.py").write_text(
                bootstrap, encoding="utf-8"
            )

            if icon and sys.platform == "win32":
                from coil.platforms.windows import set_exe_icon
                try:
                    set_exe_icon(extra_exe, Path(icon))
                except Exception:
                    pass

            if verbose:
                print(f"  Created launcher for {extra_entry}")

    if verbose:
        print(f"Bundled build complete: {bundle_dir}")

    return bundle_dir


def _build_app_directory(
    project_dir: Path,
    stage_dir: Path,
    runtime_dir: Path,
    entry_point: str,
    entry_name: str,
    gui: bool,
    secure: bool,
    icon: str | None,
    deps_dir: Path | None,
    verbose: bool,
) -> None:
    """Build the full application directory structure.

    Creates:
        stage_dir/
            AppName.exe           (renamed python.exe)
            python3.dll           (required next to exe)
            python3xx.dll         (required next to exe)
            python3xx._pth        (path config)
            vcruntime140.dll      (required next to exe)
            vcruntime140_1.dll    (required next to exe)
            feather.ico           (project assets)
            _internal/
                python3xx.zip     (stdlib)
                *.pyd             (extension modules)
                *.dll             (other libs)
                app/              (compiled source)
                lib/              (dependencies)
                sitecustomize.py  (bootstrap trigger)
                _boot_AppName.py  (bootstrap script)
    """
    stage_dir.mkdir(parents=True, exist_ok=True)
    internal_dir = stage_dir / "_internal"
    internal_dir.mkdir(exist_ok=True)

    entry_pyc = entry_point.replace(".py", ".pyc")
    ver_tag = _get_python_ver_tag(runtime_dir)
    pth_name = f"python{ver_tag}._pth" if ver_tag else ""

    # Copy runtime files — DLLs the exe needs go at root, everything else in _internal/
    for item in runtime_dir.iterdir():
        name_lower = item.name.lower()

        # Skip files we don't need
        if name_lower in ("python.exe", "pythonw.exe", "license.txt", "python.cat"):
            continue
        if name_lower.endswith(".exe"):
            continue

        if item.name in _EXE_DLLS or item.name == pth_name:
            shutil.copy2(item, stage_dir / item.name)
        elif item.is_dir():
            shutil.copytree(item, internal_dir / item.name)
        else:
            shutil.copy2(item, internal_dir / item.name)

    # Copy the right python exe as AppName.exe
    source_exe_name = "pythonw.exe" if gui else "python.exe"
    source_exe = runtime_dir / source_exe_name
    target_exe = stage_dir / f"{entry_name}.exe"
    if source_exe.is_file():
        shutil.copy2(source_exe, target_exe)
    else:
        shutil.copy2(runtime_dir / "python.exe", target_exe)

    # Compile source into _internal/app/
    if secure:
        obfuscate_secure(project_dir, internal_dir)
    else:
        obfuscate_default(project_dir, internal_dir)
    if verbose:
        print(f"  Compiled source ({'secure' if secure else 'default'} mode)")

    # Copy dependencies into _internal/lib/
    if deps_dir and deps_dir.is_dir():
        lib_dir = internal_dir / "lib"
        shutil.copytree(deps_dir, lib_dir)
        _remove_py_files(lib_dir)
        if verbose:
            print("  Bundled dependencies")

    # Create bootstrap script
    bootstrap = _generate_bootstrap_script(entry_pyc)
    (internal_dir / f"_boot_{entry_name}.py").write_text(bootstrap, encoding="utf-8")

    # Configure ._pth and sitecustomize.py
    _configure_pth(stage_dir, internal_dir, entry_name, ver_tag)

    # Set GUI subsystem if needed
    if gui and target_exe.is_file():
        from coil.platforms.windows import set_pe_subsystem
        try:
            set_pe_subsystem(target_exe, gui=True)
        except Exception:
            pass

    # Apply icon to the inner exe
    if icon and target_exe.is_file() and sys.platform == "win32":
        from coil.platforms.windows import set_exe_icon
        try:
            set_exe_icon(target_exe, Path(icon))
            if verbose:
                print(f"  Applied icon to inner exe: {icon}")
        except Exception as e:
            if verbose:
                print(f"  Warning: Could not apply icon: {e}")

    # Copy project assets (icons, configs, etc.) to root
    _copy_project_assets(project_dir, stage_dir, verbose)


def _zip_directory(directory: Path) -> bytes:
    """Zip an entire directory tree into an in-memory bytes object.

    Uses ZIP_STORED (no compression) so the bootloader can extract
    without needing a decompression library.
    """
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as zf:
        for file_path in sorted(directory.rglob("*")):
            if file_path.is_file():
                arcname = str(file_path.relative_to(directory))
                zf.write(file_path, arcname)
    return buf.getvalue()


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

    This script is exec'd by sitecustomize.py when the exe starts.
    It sets up sys.path, loads the entry point, and exits.
    """
    return f'''\
import os
import sys

_here = os.path.dirname(os.path.abspath(__file__))
_root = os.path.dirname(_here)
os.chdir(_root)

_app = os.path.join(_here, "app")
_lib = os.path.join(_here, "lib")

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


def _configure_pth(
    root_dir: Path,
    internal_dir: Path,
    entry_name: str,
    ver_tag: str,
) -> None:
    """Configure the ._pth file and sitecustomize.py.

    The ._pth file lives at root (next to the exe and python3xx.dll).
    Runtime files, app code, and deps live under _internal/.
    """
    pth_file = root_dir / f"python{ver_tag}._pth"
    if not pth_file.is_file():
        pth_files = list(root_dir.glob("python*._pth"))
        if not pth_files:
            return
        pth_file = sorted(pth_files, key=lambda p: len(p.name), reverse=True)[0]

    pth_file.write_text(
        f"_internal/python{ver_tag}.zip\n"
        f".\n"
        f"_internal\n"
        f"_internal/app\n"
        f"_internal/lib\n"
        f"import site\n",
        encoding="utf-8",
    )

    site_custom = internal_dir / "sitecustomize.py"
    site_custom.write_text(
        f"import os, sys\n"
        f"_here = os.path.dirname(os.path.abspath(__file__))\n"
        f"_boot = os.path.join(_here, '_boot_{entry_name}.py')\n"
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
        name = f.stem
        tag = name.replace("python", "")
        if tag and tag.isdigit() and len(tag) > len(best_tag):
            best_tag = tag
    return best_tag


def _copy_project_assets(
    project_dir: Path, dest_dir: Path, verbose: bool = False
) -> None:
    """Copy non-code assets from the project directory into the output.

    Copies files like .ico, .png, .json, .cfg, etc. that the app may
    reference at runtime via relative paths.
    """
    skip_extensions = {".py", ".pyc", ".pyo", ".pyd"}
    skip_names = {
        "__pycache__", ".git", ".venv", "venv", ".env", "node_modules",
        "dist", "build", ".mypy_cache", ".pytest_cache", ".tox",
    }

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
            dest = dest_dir / item.name
            if not dest.exists():
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

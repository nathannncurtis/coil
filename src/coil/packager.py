"""Packaging logic for portable and bundled build modes.

Portable: single-file .exe (bootloader + zip of runtime/app/deps).
Bundled: directory containing the exe and all supporting files.
"""

from __future__ import annotations

import io
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile
import zlib
from pathlib import Path
from typing import TYPE_CHECKING, Callable, Optional

from coil.obfuscator import obfuscate_default, obfuscate_secure
from coil.platforms import get_handler

if TYPE_CHECKING:
    from coil.ui import BuildUI

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
    icon: Optional[str] = None,
    deps_dir: Optional[Path] = None,
    verbose: bool = False,
    ui: Optional[BuildUI] = None,
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

        if ui is not None:
            ui.detail(f"Packaging portable exe for {entry}")
        elif verbose:
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
                ui=ui,
            )

            # Step 2: Zip the staged directory
            if ui is not None:
                # Count files for progress bar
                all_files = [f for f in stage_dir.rglob("*") if f.is_file()]
                total_files = len(all_files)
                with ui.file_progress("Creating archive", total=total_files) as progress:
                    task = progress.add_task("", total=total_files)

                    def _zip_cb(current: int, total: int) -> None:
                        progress.advance(task)

                    zip_data = _zip_directory(stage_dir, progress_callback=_zip_cb)
            else:
                if verbose:
                    print("  Creating portable archive...")
                zip_data = _zip_directory(stage_dir)

        # Step 3: Prepare bootloader stub (apply icon BEFORE appending zip,
        # because UpdateResource rewrites the PE and would strip appended data)
        from coil.bootloader import get_bootloader_stub
        bootloader_stub = get_bootloader_stub()

        if icon and sys.platform == "win32":
            from coil.platforms.windows import set_exe_icon
            with tempfile.NamedTemporaryFile(suffix=".exe", delete=False) as tf:
                tf.write(bootloader_stub)
                stub_path = Path(tf.name)
            try:
                set_exe_icon(stub_path, Path(icon))
                stub = stub_path.read_bytes()
                if ui is not None:
                    ui.detail(f"Applied icon: {icon}")
                elif verbose:
                    print(f"  Applied icon: {icon}")
            except Exception as e:
                stub = bootloader_stub
                if ui is not None:
                    ui.warning(f"Could not apply icon: {e}")
                elif verbose:
                    print(f"  Warning: Could not apply icon: {e}")
            finally:
                stub_path.unlink(missing_ok=True)
        else:
            stub = bootloader_stub

        # Step 4: Combine stub + zip + 12-byte trailer
        zip_offset = len(stub)
        build_hash = zlib.crc32(zip_data) & 0xFFFFFFFF
        trailer = struct.pack("<III", zip_offset, build_hash, _COIL_MAGIC)
        exe_data = bytes(stub) + zip_data + trailer

        target_exe = output_dir / f"{entry_name}.exe"
        target_exe.write_bytes(exe_data)

        results.append(target_exe)
        if ui is not None:
            from coil.ui import format_size
            ui.detail(f"Created {target_exe.name} ({format_size(target_exe.stat().st_size)})")
        elif verbose:
            size_mb = target_exe.stat().st_size / (1024 * 1024)
            print(f"  Created {target_exe.name} ({size_mb:.1f} MB)")

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
    icon: Optional[str] = None,
    deps_dir: Optional[Path] = None,
    verbose: bool = False,
    ui: Optional[BuildUI] = None,
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

    if ui is not None:
        ui.detail(f"Creating bundled build in {bundle_dir}")
    elif verbose:
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
        ui=ui,
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

            if ui is not None:
                ui.detail(f"Created launcher for {extra_entry}")
            elif verbose:
                print(f"  Created launcher for {extra_entry}")

    if ui is not None:
        ui.detail(f"Bundled build complete: {bundle_dir}")
    elif verbose:
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
    icon: Optional[str],
    deps_dir: Optional[Path],
    verbose: bool,
    ui: Optional[BuildUI] = None,
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
        obfuscate_secure(project_dir, internal_dir, ui=ui)
    else:
        obfuscate_default(project_dir, internal_dir, ui=ui)
    if ui is not None:
        ui.detail(f"Compiled source ({'secure' if secure else 'default'} mode)")
    elif verbose:
        print(f"  Compiled source ({'secure' if secure else 'default'} mode)")

    # Copy dependencies into _internal/lib/
    if deps_dir and deps_dir.is_dir():
        lib_dir = internal_dir / "lib"
        shutil.copytree(deps_dir, lib_dir)
        _remove_py_files(lib_dir)
        if ui is not None:
            ui.detail("Bundled dependencies")
        elif verbose:
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
            if ui is not None:
                ui.detail(f"Embedded icon: {Path(icon).name}")
            elif verbose:
                print(f"  Embedded icon: {Path(icon).name}")
        except Exception as e:
            if ui is not None:
                ui.warning(f"Could not embed icon: {e}")
            elif verbose:
                print(f"  Warning: Could not embed icon: {e}")

    # Copy project assets (icons, configs, etc.) to root
    _copy_project_assets(project_dir, stage_dir, verbose, ui=ui)


def _zip_directory(
    directory: Path,
    progress_callback: Optional[Callable[[int, int], None]] = None,
) -> bytes:
    """Zip an entire directory tree into an in-memory bytes object.

    Uses ZIP_STORED (no compression) so the bootloader can extract
    without needing a decompression library.
    """
    buf = io.BytesIO()
    files = sorted(f for f in directory.rglob("*") if f.is_file())
    total = len(files)
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_STORED) as zf:
        for i, file_path in enumerate(files):
            arcname = str(file_path.relative_to(directory))
            zf.write(file_path, arcname)
            if progress_callback is not None:
                progress_callback(i + 1, total)
    return buf.getvalue()


def install_dependencies(
    packages: list[str],
    dest_dir: Path,
    python_version: Optional[str] = None,
    verbose: bool = False,
    ui: Optional[BuildUI] = None,
) -> Path:
    """Install dependencies into a directory for bundling.

    Args:
        packages: List of PyPI package names.
        dest_dir: Directory to install into.
        python_version: Target Python version.
        verbose: Verbose output.
        ui: Optional BuildUI for progress display.

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

    if ui is not None:
        ui.detail(f"Installing: {', '.join(packages)}")
        with ui.spinner("Installing dependencies..."):
            try:
                subprocess.run(cmd, check=True, capture_output=True)
            except subprocess.CalledProcessError as e:
                raise RuntimeError(
                    f"Failed to install dependencies: {e}\n"
                    "Check that all package names are correct."
                ) from e
    else:
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

if not os.path.isfile(_entry):
    print(f"Fatal: Entry point not found: {{_entry}}", file=sys.stderr)
    print("The application may be corrupted. Try deleting the cache and re-launching.", file=sys.stderr)
    sys.exit(1)

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
except ImportError as e:
    print(f"Fatal: Missing module or extension: {{e}}", file=sys.stderr)
    if hasattr(e, "name") and e.name:
        print(f"  Module: {{e.name}}", file=sys.stderr)
        _ext = os.path.join(_here, e.name.replace(".", os.sep))
        _pyd = _ext + ".pyd"
        _so = _ext + ".so"
        if not os.path.isfile(_pyd) and not os.path.isfile(_so):
            print(f"  Extension file not found. Was this dependency included in the build?", file=sys.stderr)
    sys.exit(1)
except Exception as e:
    print(f"Fatal: {{type(e).__name__}}: {{e}}", file=sys.stderr)
    sys.exit(1)
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
    project_dir: Path,
    dest_dir: Path,
    verbose: bool = False,
    ui: Optional[BuildUI] = None,
) -> None:
    """Copy non-code assets from the project directory into the output.

    Copies files like .ico, .png, .json, .cfg, etc. that the app may
    reference at runtime via relative paths.
    """
    skip_extensions = {".py", ".pyc", ".pyo", ".pyd", ".pyi"}
    skip_files = {
        "requirements.txt", "pyproject.toml", "setup.py", "setup.cfg",
        "Makefile", "Dockerfile", ".dockerignore",
        ".gitignore", ".gitattributes", ".editorconfig",
        "tox.ini", "pytest.ini", ".flake8", ".pylintrc",
        "MANIFEST.in", "LICENSE", "CHANGELOG.md", "CONTRIBUTING.md",
    }
    skip_names = {
        "__pycache__", ".git", ".venv", "venv", ".env", "node_modules",
        "dist", "build", ".mypy_cache", ".pytest_cache", ".tox",
        ".github", ".vscode", ".idea", "egg-info",
    }

    for item in project_dir.iterdir():
        if item.name in skip_names or item.name.lower() in skip_files:
            continue
        if item.is_file() and item.suffix.lower() not in skip_extensions:
            dest = dest_dir / item.name
            if not dest.exists():
                shutil.copy2(item, dest)
                if ui is not None:
                    ui.detail(f"Copied asset: {item.name}")
                elif verbose:
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
                    if ui is not None:
                        ui.detail(f"Copied asset directory: {item.name}/")
                    elif verbose:
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

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
from coil.scanner import file_has_gui_imports

if TYPE_CHECKING:
    from coil.ui import BuildUI

# Trailer magic appended after the zip data in portable exes.
_COIL_MAGIC = 0x434F494C  # "COIL"

# DLLs the python exe links against at startup — must stay next to the exe.
_EXE_DLLS = {
    "python3.dll", "python313.dll", "python312.dll", "python311.dll",
    "python310.dll", "python39.dll", "vcruntime140.dll", "vcruntime140_1.dll",
}


def _lookup_subsystem(
    subsystems: Optional[dict[str, str]],
    *keys: str,
) -> Optional[str]:
    """Pick the explicit subsystem for the first matching key, or None.

    The CLI stores subsystem overrides under the entry stem and, for
    single-entry builds renamed via --name, also under the final exe name.
    Try both.
    """
    if not subsystems:
        return None
    for key in keys:
        if key in subsystems:
            return subsystems[key]
    return None


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
    optimize: Optional[int] = None,
    versioninfo: Optional[dict[str, dict[str, str]]] = None,
    subsystems: Optional[dict[str, str]] = None,
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
        entry_stem = Path(entry).stem
        explicit_sub = _lookup_subsystem(subsystems, entry_stem, entry_name)

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
                optimize=optimize,
                subsystem=explicit_sub,
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

        # Step 3: Prepare bootloader stub (apply icon + versioninfo BEFORE
        # appending zip — UpdateResource rewrites the PE and would strip
        # appended data).
        from coil.bootloader import get_bootloader_stub
        bootloader_stub = get_bootloader_stub()

        vi_fields = (versioninfo or {}).get(entry_name)

        if sys.platform == "win32" and (icon or vi_fields or explicit_sub is not None):
            with tempfile.NamedTemporaryFile(suffix=".exe", delete=False) as tf:
                tf.write(bootloader_stub)
                stub_path = Path(tf.name)
            try:
                if icon:
                    from coil.platforms.windows import set_exe_icon
                    try:
                        set_exe_icon(stub_path, Path(icon))
                        if ui is not None:
                            ui.detail(f"Applied icon: {icon}")
                        elif verbose:
                            print(f"  Applied icon: {icon}")
                    except Exception as e:
                        if ui is not None:
                            ui.warning(f"Could not apply icon: {e}")
                        elif verbose:
                            print(f"  Warning: Could not apply icon: {e}")

                if vi_fields:
                    from coil.platforms.windows import set_version_info
                    try:
                        set_version_info(stub_path, **vi_fields)
                    except Exception as e:
                        if ui is not None:
                            ui.warning(f"Could not set version info: {e}")
                        elif verbose:
                            print(f"  Warning: Could not set version info: {e}")

                if explicit_sub is not None:
                    from coil.platforms.windows import set_pe_subsystem
                    try:
                        set_pe_subsystem(stub_path, explicit_sub)
                        if ui is not None:
                            ui.detail(
                                f"Subsystem {explicit_sub!r} applied to entry "
                                f"{entry_stem!r} (explicit override)"
                            )
                    except Exception as e:
                        if ui is not None:
                            ui.warning(f"Could not set subsystem: {e}")
                        elif verbose:
                            print(f"  Warning: Could not set subsystem: {e}")

                stub = stub_path.read_bytes()
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
    optimize: Optional[int] = None,
    versioninfo: Optional[dict[str, dict[str, str]]] = None,
    subsystems: Optional[dict[str, str]] = None,
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
    # Look up by entry stem for multi-entry, or by the requested exe name
    # for single-entry (which may differ from the source filename via --name).
    primary_vi_key = Path(entry).stem if len(entry_points) > 1 else entry_name
    primary_vi = (versioninfo or {}).get(primary_vi_key)
    primary_sub = _lookup_subsystem(subsystems, Path(entry).stem, entry_name)
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
        optimize=optimize,
        versioninfo=primary_vi,
        subsystem=primary_sub,
    )

    # For multiple entry points, create additional launchers
    if len(entry_points) > 1:
        for extra_entry in entry_points[1:]:
            extra_name = Path(extra_entry).stem
            extra_pyc = extra_entry.replace(".py", ".pyc")

            # Explicit subsystem config wins over GUI-import autodetect.
            extra_sub = _lookup_subsystem(subsystems, extra_name, extra_name)
            extra_file = project_dir / extra_entry
            if extra_sub is not None:
                extra_gui = (extra_sub == "gui")
            else:
                extra_gui = (
                    file_has_gui_imports(extra_file) if extra_file.is_file() else gui
                )

            # Copy python exe as the extra entry
            source_exe_name = "pythonw.exe" if extra_gui else "python.exe"
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

            if sys.platform == "win32":
                if icon:
                    from coil.platforms.windows import set_exe_icon
                    try:
                        set_exe_icon(extra_exe, Path(icon))
                    except Exception:
                        pass
                from coil.platforms.windows import set_version_info
                extra_vi = (versioninfo or {}).get(extra_name)
                try:
                    if extra_vi:
                        set_version_info(extra_exe, **extra_vi)
                    else:
                        set_version_info(extra_exe, product_name=extra_name)
                except Exception:
                    pass

                if extra_sub is not None and extra_exe.is_file():
                    from coil.platforms.windows import set_pe_subsystem
                    try:
                        set_pe_subsystem(extra_exe, extra_sub)
                        if ui is not None:
                            ui.detail(
                                f"Subsystem {extra_sub!r} applied to entry "
                                f"{extra_name!r} (explicit override)"
                            )
                    except Exception as e:
                        if ui is not None:
                            ui.warning(f"Could not set subsystem: {e}")

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
    optimize: Optional[int] = None,
    versioninfo: Optional[dict[str, str]] = None,
    subsystem: Optional[str] = None,
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
    # Skip files not needed at runtime to reduce output size.
    _skip_runtime = {
        "python.exe", "pythonw.exe", "license.txt", "python.cat",
        "news.txt", "py.exe", "pyw.exe",
    }
    for item in runtime_dir.iterdir():
        name_lower = item.name.lower()

        if name_lower in _skip_runtime:
            continue
        if name_lower.endswith((".exe", ".pdb")):
            continue
        if item.is_dir() and name_lower == "__pycache__":
            continue

        if item.name in _EXE_DLLS or item.name == pth_name:
            shutil.copy2(item, stage_dir / item.name)
        elif item.is_dir():
            shutil.copytree(item, internal_dir / item.name)
        else:
            shutil.copy2(item, internal_dir / item.name)

    # Build one exclude matcher (DEFAULT_EXCLUDE_PATTERNS + .coilignore) and
    # apply it both to the project-asset copier and the obfuscator — a file
    # that doesn't belong at bundle root also doesn't belong as a .pyc under
    # _internal/app/.
    exclude_matcher = _build_exclude_matcher(project_dir)

    # Strip unused modules from stdlib zip
    from coil.scanner import scan_project
    project_imports = scan_project(project_dir)
    _strip_stdlib_zip(internal_dir, project_imports, ui=ui, verbose=verbose)

    # Copy the right python exe as AppName.exe. Explicit subsystem (from
    # [build.entries.<stem>].subsystem) wins over the top-level gui flag.
    if subsystem is not None:
        use_gui = (subsystem == "gui")
    else:
        use_gui = gui
    source_exe_name = "pythonw.exe" if use_gui else "python.exe"
    source_exe = runtime_dir / source_exe_name
    target_exe = stage_dir / f"{entry_name}.exe"
    if source_exe.is_file():
        shutil.copy2(source_exe, target_exe)
    else:
        shutil.copy2(runtime_dir / "python.exe", target_exe)

    # Compile source into _internal/app/
    # Use embedded python.exe for compilation so .pyc magic numbers match
    runtime_python = runtime_dir / "python.exe"
    if not runtime_python.is_file():
        runtime_python = None

    # Default optimize: 1 for default mode, 2 for secure mode
    opt_level = optimize if optimize is not None else (2 if secure else 1)
    if secure:
        obfuscate_secure(project_dir, internal_dir, ui=ui, optimize=opt_level, runtime_python=runtime_python, skip=exclude_matcher)
    else:
        obfuscate_default(project_dir, internal_dir, ui=ui, optimize=opt_level, runtime_python=runtime_python, skip=exclude_matcher)
    if ui is not None:
        ui.detail(f"Compiled source ({'secure' if secure else 'default'} mode, optimize={opt_level})")
    elif verbose:
        print(f"  Compiled source ({'secure' if secure else 'default'} mode, optimize={opt_level})")

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

    # Stamp subsystem. Explicit config wins; otherwise fall through to the
    # top-level gui flag (preserves existing behavior).
    if target_exe.is_file() and sys.platform == "win32":
        from coil.platforms.windows import set_pe_subsystem
        if subsystem is not None:
            try:
                set_pe_subsystem(target_exe, subsystem)
                if ui is not None:
                    ui.detail(
                        f"Subsystem {subsystem!r} applied to entry "
                        f"{entry_name!r} (explicit override)"
                    )
            except Exception as e:
                if ui is not None:
                    ui.warning(f"Could not set subsystem: {e}")
        elif gui:
            try:
                set_pe_subsystem(target_exe, True)
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

    # Set version info so Windows shows the correct app name
    if target_exe.is_file() and sys.platform == "win32":
        from coil.platforms.windows import set_version_info
        try:
            if versioninfo:
                set_version_info(target_exe, **versioninfo)
            else:
                set_version_info(target_exe, product_name=entry_name)
        except Exception:
            pass

    # Copy project assets (icons, configs, etc.) to root
    _copy_project_assets(project_dir, stage_dir, verbose, ui=ui, exclude_matcher=exclude_matcher)


def _zip_directory(
    directory: Path,
    progress_callback: Optional[Callable[[int, int], None]] = None,
    compress: bool = True,
) -> bytes:
    """Zip an entire directory tree into an in-memory bytes object.

    Args:
        directory: Directory to zip.
        progress_callback: Optional callback(current, total).
        compress: Use ZIP_DEFLATED compression (level 9). The v3 bootloader
                  supports both STORED and DEFLATED entries.
    """
    buf = io.BytesIO()
    method = zipfile.ZIP_DEFLATED if compress else zipfile.ZIP_STORED
    compresslevel = 9 if compress else None
    files = sorted(f for f in directory.rglob("*") if f.is_file())
    total = len(files)
    with zipfile.ZipFile(buf, "w", method, compresslevel=compresslevel) as zf:
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

    # If target Python differs from host, tell pip to fetch compatible wheels
    if python_version:
        import platform as _platform
        host_ver = f"{sys.version_info.major}.{sys.version_info.minor}"
        target_ver = python_version.strip().lstrip("v")
        # Normalize short versions (e.g. "3.13" stays, "3.13.1" becomes "3.13")
        target_short = ".".join(target_ver.split(".")[:2])
        if target_short != host_ver:
            cmd.extend([
                "--python-version", target_short,
                "--only-binary=:all:",
            ])

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
        "import os, sys, site, glob\n"
        "_here = os.path.dirname(os.path.abspath(__file__))\n"
        "_lib = os.path.join(_here, 'lib')\n"
        "# Process .pth files under _internal/lib (pywin32.pth, etc.). ._pth\n"
        "# governs top-level sys.path but does not parse nested .pth files;\n"
        "# site.addsitedir() does.\n"
        "if os.path.isdir(_lib):\n"
        "    site.addsitedir(_lib)\n"
        "# Register DLL search directories so native extensions find their\n"
        "# dependent DLLs (pywin32_system32, numpy.libs, etc.). Python 3.8+\n"
        "# blocks DLL loading from arbitrary paths.\n"
        "if hasattr(os, 'add_dll_directory') and os.path.isdir(_lib):\n"
        "    _seen = set()\n"
        "    for _d, _subs, _files in os.walk(_lib):\n"
        "        if any(f.lower().endswith(('.dll', '.pyd')) for f in _files):\n"
        "            _r = os.path.realpath(_d)\n"
        "            if _r in _seen:\n"
        "                continue\n"
        "            _seen.add(_r)\n"
        "            try:\n"
        "                os.add_dll_directory(_d)\n"
        "            except OSError:\n"
        "                pass\n"
        "_exe = os.path.splitext(os.path.basename(sys.executable))[0]\n"
        "# Try exact match first, then scan for matching boot script\n"
        "_boot = os.path.join(_here, f'_boot_{_exe}.py')\n"
        "if not os.path.isfile(_boot):\n"
        "    # Search boot scripts for one whose name is a substring of the exe name\n"
        "    for _b in sorted(glob.glob(os.path.join(_here, '_boot_*.py'))):\n"
        "        _stem = os.path.basename(_b)[6:-3]  # strip '_boot_' and '.py'\n"
        "        if _stem in _exe.lower():\n"
        "            _boot = _b\n"
        "            break\n"
        "    else:\n"
        f"        _boot = os.path.join(_here, '_boot_{entry_name}.py')\n"
        "if os.path.isfile(_boot):\n"
        "    # os._exit bypasses CPython's init-phase fatal-error handling.\n"
        "    # SystemExit raised during site import becomes\n"
        "    # 'Fatal Python error: init_import_site' + nonzero exit — we\n"
        "    # want a clean numeric exit matching what the boot script asked\n"
        "    # for (or 0 on clean return, to avoid REPL/stdin block).\n"
        "    _code = 0\n"
        "    try:\n"
        "        exec(compile(open(_boot).read(), _boot, 'exec'))\n"
        "    except SystemExit as _e:\n"
        "        _c = _e.code\n"
        "        _code = _c if isinstance(_c, int) else (0 if _c is None else 1)\n"
        "    try: sys.stdout.flush()\n"
        "    except Exception: pass\n"
        "    try: sys.stderr.flush()\n"
        "    except Exception: pass\n"
        "    os._exit(_code)\n",
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


# Baseline project-root items that should never end up in a Coil bundle:
# build/packaging metadata, VCS state, caches, virtualenvs, installer
# scripts, and common docs. Each downstream project's build.bat used to
# delete these by hand after `coil build`; they're excluded at the source
# now so that tail can shrink to zero.
DEFAULT_EXCLUDE_PATTERNS: list[str] = [
    # Python caches and bytecode artifacts
    "__pycache__/",
    ".pytest_cache/",
    ".mypy_cache/",
    ".ruff_cache/",
    ".tox/",
    "*.egg-info/",
    # Build output directories
    "build/",
    "dist/",
    "Output/",
    # VCS / editor metadata
    ".git/",
    ".github/",
    ".vscode/",
    ".idea/",
    ".cursor/",
    ".gitignore",
    ".gitattributes",
    ".editorconfig",
    ".coilignore",
    # Test scaffolding and AI-assistant scratch
    "tests/",
    "test/",
    "testing/",
    "memory/",
    # Virtualenvs / dependency trees
    ".venv/",
    "venv/",
    "node_modules/",
    ".env",
    # Build system / packaging config
    "coil.toml",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "MANIFEST.in",
    "build.bat",
    "Makefile",
    "tox.ini",
    "pytest.ini",
    ".flake8",
    ".pylintrc",
    "Dockerfile",
    ".dockerignore",
    # Requirements / lock files
    "requirements*.txt",
    "req.txt",
    "req-*.txt",
    # Installer artifacts
    "*.iss",
    # Common top-level docs
    "README*",
    "LICENSE*",
    "CHANGELOG*",
    "CONTRIBUTING*",
    "CODE_OF_CONDUCT*",
    "SECURITY*",
    # Stray logs / prompt scratch
    "*.log",
    "prompt.md",
]


def _load_coilignore(project_dir: Path) -> list[str]:
    """Load .coilignore patterns from the project directory.

    Works like .gitignore — one glob pattern per line, # for comments,
    blank lines ignored, `!pattern` to re-include a file the defaults (or
    an earlier pattern) would exclude. `\\!` / `\\#` escape literal leading
    chars.
    """
    ignore_file = project_dir / ".coilignore"
    if not ignore_file.is_file():
        return []
    patterns: list[str] = []
    for line in ignore_file.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("\\!") or line.startswith("\\#"):
            line = line[1:]
        patterns.append(line)
    return patterns


def _single_pattern_match(pattern: str, name: str, rel: str, is_dir: bool) -> bool:
    """Match one fnmatch pattern against a path's name and project-relative form.

    Directory patterns end with "/" and only match directories.
    """
    import fnmatch
    if pattern.endswith("/"):
        if not is_dir:
            return False
        stripped = pattern.rstrip("/")
        return fnmatch.fnmatch(name, stripped) or fnmatch.fnmatch(rel, stripped)
    return fnmatch.fnmatch(name, pattern) or fnmatch.fnmatch(rel, pattern)


def _eval_patterns(
    name: str,
    rel: str,
    is_dir: bool,
    patterns: list[str],
) -> bool:
    """Apply last-match-wins exclusion evaluation against one path identity."""
    excluded = False
    for raw in patterns:
        negate = raw.startswith("!")
        pattern = raw[1:] if negate else raw
        if _single_pattern_match(pattern, name, rel, is_dir):
            excluded = not negate
    return excluded


def _build_exclude_matcher(project_dir: Path) -> Callable[[Path], bool]:
    """Build a predicate that returns True when a path should be excluded.

    Evaluates DEFAULT_EXCLUDE_PATTERNS first, then the user's .coilignore,
    last-match-wins per gitignore semantics. Two extensions:
      * `!pattern` re-includes a leaf an earlier pattern would have excluded.
      * An excluded ancestor directory cascades to its whole subtree — a
        `!` pattern on a child cannot override a parent exclusion
        (matches git's documented behavior).
    """
    patterns = list(DEFAULT_EXCLUDE_PATTERNS) + _load_coilignore(project_dir)

    def matches(path: Path) -> bool:
        try:
            rel_parts = path.relative_to(project_dir).parts
        except ValueError:
            return False
        for i in range(1, len(rel_parts)):
            ancestor_name = rel_parts[i - 1]
            ancestor_rel = "/".join(rel_parts[:i])
            if _eval_patterns(ancestor_name, ancestor_rel, True, patterns):
                return True
        return _eval_patterns(
            path.name,
            "/".join(rel_parts),
            path.is_dir(),
            patterns,
        )

    return matches


def _copy_project_assets(
    project_dir: Path,
    dest_dir: Path,
    verbose: bool = False,
    ui: Optional[BuildUI] = None,
    exclude_matcher: Optional[Callable[[Path], bool]] = None,
) -> None:
    """Copy non-code assets from the project directory into the output.

    Copies files like .ico, .png, .json, .cfg, etc. that the app may
    reference at runtime via relative paths. Code (.py/.pyc/etc.) lives in
    _internal/app/; everything in DEFAULT_EXCLUDE_PATTERNS plus the user's
    .coilignore is dropped.
    """
    code_extensions = {".py", ".pyc", ".pyo", ".pyd", ".pyi"}
    if exclude_matcher is None:
        exclude_matcher = _build_exclude_matcher(project_dir)

    for item in project_dir.iterdir():
        if exclude_matcher(item):
            continue
        if item.is_file() and item.suffix.lower() not in code_extensions:
            dest = dest_dir / item.name
            if not dest.exists():
                shutil.copy2(item, dest)
                if ui is not None:
                    ui.detail(f"Copied asset: {item.name}")
                elif verbose:
                    print(f"  Copied asset: {item.name}")
        elif item.is_dir():
            dest = dest_dir / item.name
            if not dest.exists():
                has_assets = any(
                    f.suffix.lower() not in code_extensions
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
    """Remove unnecessary files from installed packages to reduce size.

    Keeps .dist-info directories since packages may use importlib.metadata
    at runtime to read their own version info.
    """
    patterns_to_remove = [
        "__pycache__",
        "tests",
        "test",
        "docs",
        "doc",
    ]

    for pattern in patterns_to_remove:
        for match in dest_dir.rglob(pattern):
            if match.is_dir():
                shutil.rmtree(match, ignore_errors=True)


def _strip_stdlib_zip(
    internal_dir: Path,
    project_imports: Optional[set[str]] = None,
    ui: Optional[BuildUI] = None,
    verbose: bool = False,
) -> int:
    """Strip unnecessary modules from the bundled stdlib zip.

    Returns bytes saved.
    """
    from coil.utils.stdlib_strip import ALWAYS_STRIP, STRIP_UNLESS_USED

    stdlib_zips = list(internal_dir.glob("python*.zip"))
    if not stdlib_zips:
        return 0

    stdlib_zip = stdlib_zips[0]
    original_size = stdlib_zip.stat().st_size

    project_imports = project_imports or set()

    # Build set of entries to remove
    strip_prefixes: set[str] = set()
    for mod in ALWAYS_STRIP:
        if mod.endswith(".pyc"):
            strip_prefixes.add(mod)
        else:
            strip_prefixes.add(mod + "/")
            strip_prefixes.add(mod + ".pyc")

    for mod_path, import_name in STRIP_UNLESS_USED.items():
        if import_name not in project_imports:
            if mod_path.endswith(".pyc"):
                strip_prefixes.add(mod_path)
            else:
                strip_prefixes.add(mod_path + "/")
                strip_prefixes.add(mod_path + ".pyc")

    # Rewrite the zip without stripped entries
    import tempfile
    with tempfile.NamedTemporaryFile(suffix=".zip", delete=False) as tf:
        tmp_path = Path(tf.name)

    try:
        with zipfile.ZipFile(stdlib_zip, "r") as src:
            with zipfile.ZipFile(tmp_path, "w", zipfile.ZIP_DEFLATED) as dst:
                for item in src.infolist():
                    should_strip = False
                    for prefix in strip_prefixes:
                        if item.filename == prefix or item.filename.startswith(prefix):
                            should_strip = True
                            break
                    if not should_strip:
                        dst.writestr(item, src.read(item.filename))

        shutil.move(str(tmp_path), str(stdlib_zip))
    except Exception:
        tmp_path.unlink(missing_ok=True)
        return 0

    new_size = stdlib_zip.stat().st_size
    saved = original_size - new_size

    if saved > 0:
        if ui is not None:
            from coil.ui import format_size
            ui.detail(f"Stripped stdlib: saved {format_size(saved)}")
        elif verbose:
            print(f"  Stripped stdlib: saved {saved / 1024:.0f} KB")

    return saved

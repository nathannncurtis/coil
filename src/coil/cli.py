"""Command-line interface for Coil."""

import argparse
import os
import shutil
import sys
from pathlib import Path

from coil import __version__


def create_parser() -> argparse.ArgumentParser:
    """Create and return the argument parser."""
    parser = argparse.ArgumentParser(
        prog="coil",
        description="Coil: A Python-to-executable compiler that just works.",
    )
    parser.add_argument(
        "--version",
        action="version",
        version=f"coil {__version__}",
    )

    subparsers = parser.add_subparsers(dest="command")

    # --- build subcommand ---
    build_parser = subparsers.add_parser(
        "build",
        help="Build a Python project into an executable.",
    )
    build_parser.add_argument(
        "project",
        type=str,
        help="Path to the Python project directory.",
    )
    build_parser.add_argument(
        "--entry",
        type=str,
        action="append",
        default=None,
        help="Entry point script relative to project dir. Can be specified multiple times.",
    )
    build_parser.add_argument(
        "--mode",
        type=str,
        choices=["portable", "bundled"],
        default=None,
        help="Build mode: portable (single exe) or bundled (directory). Default: portable.",
    )
    build_parser.add_argument(
        "--os",
        type=str,
        choices=["windows", "macos", "linux"],
        default=None,
        help="Target OS. Default: current OS.",
    )
    build_parser.add_argument(
        "--python",
        type=str,
        default=None,
        help="Target Python version (e.g. 3.12). Default: auto-detect.",
    )

    gui_group = build_parser.add_mutually_exclusive_group()
    gui_group.add_argument(
        "--gui",
        action="store_true",
        default=False,
        help="Suppress console window (GUI app).",
    )
    gui_group.add_argument(
        "--console",
        action="store_true",
        default=True,
        help="Show console window (default).",
    )

    build_parser.add_argument(
        "--secure",
        action="store_true",
        default=False,
        help="Heavy obfuscation. Cannot be reversed by coil decompile.",
    )
    build_parser.add_argument(
        "--exclude",
        type=str,
        default=None,
        help="Comma-separated packages to exclude.",
    )
    build_parser.add_argument(
        "--include",
        type=str,
        default=None,
        help="Comma-separated packages to force-include.",
    )
    build_parser.add_argument(
        "--output",
        type=str,
        default="./dist",
        help="Output directory. Default: ./dist.",
    )
    build_parser.add_argument(
        "--name",
        type=str,
        default=None,
        help="Output executable name. Default: project directory name.",
    )
    build_parser.add_argument(
        "--icon",
        type=str,
        default=None,
        help="Icon file path (.ico for Windows).",
    )
    build_parser.add_argument(
        "--requirements",
        type=str,
        default=None,
        help="Explicit path to requirements.txt or pyproject.toml.",
    )
    build_parser.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Detailed build output.",
    )
    build_parser.add_argument(
        "--dry-run",
        action="store_true",
        default=False,
        help="Show what would be built without building.",
    )
    build_parser.add_argument(
        "--profile",
        type=str,
        default=None,
        help="Build profile from coil.toml (e.g. dev, release).",
    )
    build_parser.add_argument(
        "--clean",
        action="store_true",
        default=False,
        help="Build in a clean virtual environment with only declared dependencies.",
    )
    build_parser.add_argument(
        "--clear-cache",
        action="store_true",
        default=False,
        help=argparse.SUPPRESS,  # Hidden flag
    )

    # --- doctor subcommand ---
    doctor_parser = subparsers.add_parser(
        "doctor",
        help="Run pre-build diagnostics.",
    )
    doctor_parser.add_argument(
        "project",
        type=str,
        nargs="?",
        default=".",
        help="Project directory to check. Default: current directory.",
    )
    doctor_parser.add_argument(
        "--verbose",
        action="store_true",
        default=False,
        help="Show additional diagnostic detail.",
    )

    # --- init subcommand ---
    init_parser = subparsers.add_parser(
        "init",
        help="Initialize a coil.toml config file for your project.",
    )
    init_parser.add_argument(
        "project",
        type=str,
        nargs="?",
        default=".",
        help="Project directory. Default: current directory.",
    )

    # --- cache subcommand ---
    cache_parser = subparsers.add_parser(
        "cache",
        help="Manage the portable exe runtime cache.",
    )
    cache_sub = cache_parser.add_subparsers(dest="cache_action")
    cache_sub.add_parser(
        "clear",
        help="Delete all cached runtimes.",
    )
    cache_sub.add_parser(
        "info",
        help="Show cache location and size.",
    )

    # --- decompile subcommand ---
    decompile_parser = subparsers.add_parser(
        "decompile",
        help="Decompile a Coil-built executable back to source.",
    )
    decompile_parser.add_argument(
        "executable",
        type=str,
        help="Path to the Coil-built executable.",
    )
    decompile_parser.add_argument(
        "--output",
        type=str,
        default="./recovered",
        help="Output directory for recovered source. Default: ./recovered.",
    )

    return parser


def detect_os() -> str:
    """Detect the current operating system."""
    if sys.platform == "win32":
        return "windows"
    elif sys.platform == "darwin":
        return "macos"
    elif sys.platform.startswith("linux"):
        return "linux"
    else:
        return sys.platform


def detect_python_version() -> str:
    """Detect the current Python version as major.minor."""
    return f"{sys.version_info.major}.{sys.version_info.minor}"


def resolve_entry_points(project_dir: Path, entries: list[str] | None) -> list[str]:
    """Resolve entry points for the build.

    If entries are specified, validate they exist. Otherwise, auto-detect
    by looking for __main__.py then main.py in the project root.
    """
    if entries:
        for entry in entries:
            entry_path = project_dir / entry
            if not entry_path.is_file():
                print(f"Error: Entry point '{entry}' not found in {project_dir}")
                sys.exit(1)
        return entries

    if (project_dir / "__main__.py").is_file():
        return ["__main__.py"]
    if (project_dir / "main.py").is_file():
        return ["main.py"]

    print(
        "Error: No entry point found. Use --entry to specify your main script, "
        "or add a __main__.py to your project root."
    )
    sys.exit(1)


def get_cache_dir() -> Path:
    """Get the Coil runtime cache directory."""
    local_app = os.environ.get("LOCALAPPDATA", "")
    if local_app:
        return Path(local_app) / "coil"
    temp = os.environ.get("TEMP", os.environ.get("TMP", ""))
    if temp:
        return Path(temp) / "coil"
    return Path.home() / ".coil" / "cache"


def clear_cache() -> None:
    """Delete the entire Coil runtime cache."""
    cache = get_cache_dir()
    if cache.is_dir():
        size = sum(f.stat().st_size for f in cache.rglob("*") if f.is_file())
        shutil.rmtree(cache, ignore_errors=True)
        print(f"Cleared cache: {cache} ({size / 1024 / 1024:.1f} MB freed)")
    else:
        print(f"Cache is empty: {cache}")


def show_cache_info() -> None:
    """Show cache location, entries, and total size."""
    cache = get_cache_dir()
    if not cache.is_dir():
        print(f"Cache directory: {cache}")
        print("  (empty)")
        return

    total_size = 0
    entries = 0
    for app_dir in sorted(cache.iterdir()):
        if not app_dir.is_dir():
            continue
        for hash_dir in sorted(app_dir.iterdir()):
            if not hash_dir.is_dir():
                continue
            dir_size = sum(f.stat().st_size for f in hash_dir.rglob("*") if f.is_file())
            total_size += dir_size
            entries += 1
            marker = hash_dir / ".coil_ready"
            status = "ready" if marker.is_file() else "incomplete"
            print(f"  {app_dir.name}/{hash_dir.name}  ({dir_size / 1024 / 1024:.1f} MB, {status})")

    print(f"\nCache directory: {cache}")
    print(f"  {entries} cached build(s), {total_size / 1024 / 1024:.1f} MB total")


def validate_build_args(args: argparse.Namespace) -> None:
    """Validate build subcommand arguments."""
    project = Path(args.project)
    if not project.is_dir():
        print(f"Error: Project directory '{args.project}' does not exist or is not a directory.")
        sys.exit(1)

    if args.icon and not Path(args.icon).is_file():
        print(f"Error: Icon file '{args.icon}' not found.")
        sys.exit(1)

    if args.requirements and not Path(args.requirements).is_file():
        print(f"Error: Requirements file '{args.requirements}' not found.")
        sys.exit(1)


def run_init(project_path: str) -> None:
    """Run the coil init subcommand."""
    project_dir = Path(project_path).resolve()
    if not project_dir.is_dir():
        print(f"Error: '{project_path}' is not a directory.")
        sys.exit(1)

    toml_path = project_dir / "coil.toml"
    if toml_path.is_file():
        answer = input("coil.toml already exists. Overwrite? [y/N] ").strip().lower()
        if answer not in ("y", "yes"):
            print("Aborted.")
            sys.exit(0)

    # Detect entry point
    entry = ""
    if (project_dir / "__main__.py").is_file():
        entry = "__main__.py"
        print(f"  Detected entry point: {entry}")
    elif (project_dir / "main.py").is_file():
        entry = "main.py"
        print(f"  Detected entry point: {entry}")
    else:
        py_files = sorted(f.name for f in project_dir.glob("*.py"))
        if py_files:
            print(f"  Python files found: {', '.join(py_files)}")
        entry = input("  Entry point script: ").strip()
        if not entry:
            print("Error: No entry point specified.")
            sys.exit(1)

    # Console or GUI?
    gui_answer = input("  Console or GUI app? [console/gui] (default: console): ").strip().lower()
    console = gui_answer != "gui"

    # Icon path?
    icon = ""
    ico_files = list(project_dir.glob("*.ico"))
    if ico_files:
        print(f"  Found .ico file(s): {', '.join(f.name for f in ico_files)}")
    icon_answer = input("  Icon path (press Enter to skip): ").strip()
    if icon_answer:
        icon = icon_answer

    # Detect dependency files
    has_requirements = (project_dir / "requirements.txt").is_file()
    has_pyproject = (project_dir / "pyproject.toml").is_file()

    name = project_dir.name
    python_version = detect_python_version()

    from coil.config import generate_toml
    toml_content = generate_toml(
        entry=entry,
        name=name,
        console=console,
        icon=icon,
        python_version=python_version,
        has_requirements=has_requirements,
        has_pyproject=has_pyproject,
    )

    toml_path.write_text(toml_content, encoding="utf-8")
    print(f"\nCreated coil.toml. Run `coil build` to build your project.")


def _apply_toml_config(args: argparse.Namespace, project_dir: Path) -> None:
    """Load coil.toml and apply its values as defaults under CLI args.

    CLI flags take priority over toml values.
    """
    from coil.config import load_config, get_build_config

    raw = load_config(project_dir)
    if raw is None:
        return

    try:
        config = get_build_config(raw, profile=args.profile)
    except ValueError as e:
        print(f"Error: {e}")
        sys.exit(1)

    # Only apply toml values where the CLI didn't explicitly set something.
    # argparse defaults: entry=None, mode="portable", os=None, python=None,
    # gui=False, secure=False, exclude=None, include=None, output="./dist",
    # name=None, icon=None, verbose=False, clean=False

    if args.entry is None and config.get("entry"):
        args.entry = [config["entry"]]

    if args.name is None and config.get("name"):
        args.name = config["name"]

    # mode: None means user didn't pass --mode on CLI
    if args.mode is None and config.get("mode"):
        args.mode = config["mode"]

    # os
    if args.os is None and config.get("os"):
        args.os = config["os"]

    # python
    if args.python is None and config.get("python"):
        args.python = config["python"]

    # gui: toml uses console=true/false, CLI uses --gui/--console
    if not args.gui and not config.get("console", True):
        args.gui = True

    # secure
    if not args.secure and config.get("secure"):
        args.secure = True

    # verbose
    if not args.verbose and config.get("verbose"):
        args.verbose = True

    # clean
    if not args.clean and config.get("clean"):
        args.clean = True

    # output: only if user didn't change from default
    if args.output == "./dist" and config.get("output_dir") and config["output_dir"] != "./dist":
        args.output = config["output_dir"]

    # icon
    if args.icon is None and config.get("icon"):
        args.icon = config["icon"]

    # exclude/include
    if args.exclude is None and config.get("exclude"):
        args.exclude = ",".join(config["exclude"])

    if args.include is None and config.get("include"):
        args.include = ",".join(config["include"])


def main(argv: list[str] | None = None) -> None:
    """Main entry point for the Coil CLI."""
    parser = create_parser()
    args = parser.parse_args(argv)

    if args.command is None:
        parser.print_help()
        sys.exit(0)

    if args.command == "doctor":
        from coil.doctor import run_doctor
        project_dir = Path(args.project).resolve()
        if not project_dir.is_dir():
            print(f"Error: '{args.project}' is not a directory.")
            sys.exit(1)
        sys.exit(run_doctor(project_dir, verbose=args.verbose))

    if args.command == "init":
        run_init(args.project)
        sys.exit(0)

    if args.command == "cache":
        if args.cache_action == "clear":
            clear_cache()
        elif args.cache_action == "info":
            show_cache_info()
        else:
            print("Usage: coil cache {clear,info}")
        sys.exit(0)

    if args.command == "build":
        if args.clear_cache:
            clear_cache()

        validate_build_args(args)

        project_dir = Path(args.project).resolve()

        # Load coil.toml defaults (CLI flags override)
        _apply_toml_config(args, project_dir)

        # Apply final defaults for values that may still be unset
        if args.mode is None:
            args.mode = "portable"

        target_os = args.os or detect_os()
        python_version = args.python or detect_python_version()
        entry_points = resolve_entry_points(project_dir, args.entry)
        name = args.name or project_dir.name
        exclude = [p.strip() for p in args.exclude.split(",")] if args.exclude else []
        include = [p.strip() for p in args.include.split(",")] if args.include else []

        if not args.python:
            print(
                f"Warning: No --python version specified. Using detected version {python_version}."
            )

        if target_os != "windows":
            print(f"Error: {target_os} support is not yet implemented. Only Windows is supported.")
            sys.exit(1)

        # Auto-detect icon: if --icon not specified, look for .ico in project dir
        icon = args.icon
        if not icon:
            ico_files = list(project_dir.glob("*.ico"))
            if ico_files:
                # Prefer one matching the project/app name, else first alphabetically
                name_match = [f for f in ico_files if f.stem.lower() == name.lower()]
                icon = str(name_match[0] if name_match else sorted(ico_files)[0])
                print(f"Found icon: {icon}")

        if args.dry_run:
            print("Dry run - would build with the following settings:")
            print(f"  Project:      {project_dir}")
            print(f"  Entry points: {', '.join(entry_points)}")
            print(f"  Mode:         {args.mode}")
            print(f"  Target OS:    {target_os}")
            print(f"  Python:       {python_version}")
            print(f"  GUI:          {args.gui}")
            print(f"  Secure:       {args.secure}")
            print(f"  Output:       {args.output}")
            print(f"  Name:         {name}")
            if icon:
                print(f"  Icon:         {icon}")
            if exclude:
                print(f"  Exclude:      {', '.join(exclude)}")
            if include:
                print(f"  Include:      {', '.join(include)}")
            if args.requirements:
                print(f"  Requirements: {args.requirements}")
            sys.exit(0)

        try:
            from coil.builder import build
            build(
                project_dir=project_dir,
                entry_points=entry_points,
                mode=args.mode,
                target_os=target_os,
                python_version=python_version,
                gui=args.gui,
                secure=args.secure,
                exclude=exclude,
                include=include,
                output_dir=args.output,
                name=name,
                icon=icon,
                requirements=args.requirements,
                verbose=args.verbose,
            )
        except Exception as e:
            if args.verbose:
                raise
            print(f"Error: {e}")
            sys.exit(1)

    elif args.command == "decompile":
        exe_path = Path(args.executable)
        if not exe_path.is_file():
            print(f"Error: Executable '{args.executable}' not found.")
            sys.exit(1)

        from coil.decompiler import decompile
        output = Path(args.output)
        success = decompile(exe_path, output)
        if not success:
            sys.exit(1)

"""Command-line interface for Coil."""

import argparse
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
        default="portable",
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


def main(argv: list[str] | None = None) -> None:
    """Main entry point for the Coil CLI."""
    parser = create_parser()
    args = parser.parse_args(argv)

    if args.command is None:
        parser.print_help()
        sys.exit(0)

    if args.command == "build":
        validate_build_args(args)

        project_dir = Path(args.project).resolve()
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

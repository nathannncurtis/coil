"""Dependency resolution for Python projects."""

import re
from pathlib import Path

from coil.scanner import scan_project
from coil.utils.package_map import resolve_package_name
from coil.utils.stdlib_list import get_stdlib_modules


def parse_requirements_txt(path: Path) -> list[str]:
    """Parse a requirements.txt file and return package names.

    Strips version specifiers, comments, and blank lines.
    """
    packages: list[str] = []
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or line.startswith("-"):
            continue
        # Strip version specifiers, extras, and environment markers
        name = re.split(r"[>=<!~;\[\]]", line)[0].strip()
        if name:
            packages.append(name)
    return packages


def parse_pyproject_toml(path: Path) -> list[str]:
    """Parse dependencies from a pyproject.toml file.

    Looks for [project] dependencies array.
    """
    try:
        import tomllib
    except ImportError:
        import tomli as tomllib  # type: ignore[no-redef]

    content = path.read_text(encoding="utf-8")
    data = tomllib.loads(content)

    deps = data.get("project", {}).get("dependencies", [])
    packages: list[str] = []
    for dep in deps:
        name = re.split(r"[>=<!~;\[\]]", dep)[0].strip()
        if name:
            packages.append(name)
    return packages


def resolve_dependencies(
    project_dir: Path,
    python_version: str,
    requirements_path: str | None = None,
    exclude: list[str] | None = None,
    include: list[str] | None = None,
) -> list[str]:
    """Resolve all third-party dependencies for a project.

    Priority order:
    1. Explicit requirements path (--requirements flag)
    2. requirements.txt in project dir
    3. pyproject.toml with dependencies in project dir
    4. AST-based import scanning

    Args:
        project_dir: Path to the project directory.
        python_version: Target Python version (e.g. "3.12").
        requirements_path: Explicit path to requirements file.
        exclude: Package names to exclude.
        include: Package names to force-include.

    Returns:
        Sorted list of PyPI package names.
    """
    exclude = exclude or []
    include = include or []
    packages: list[str] = []

    # Priority 1: explicit requirements path
    if requirements_path:
        req_path = Path(requirements_path)
        if req_path.name.endswith(".toml"):
            packages = parse_pyproject_toml(req_path)
        else:
            packages = parse_requirements_txt(req_path)

    # Priority 2: requirements.txt in project dir
    elif (project_dir / "requirements.txt").is_file():
        packages = parse_requirements_txt(project_dir / "requirements.txt")

    # Priority 3: pyproject.toml in project dir
    elif (project_dir / "pyproject.toml").is_file():
        packages = parse_pyproject_toml(project_dir / "pyproject.toml")

    # Priority 4: AST scan
    else:
        stdlib = get_stdlib_modules(python_version)
        all_imports = scan_project(project_dir)

        # Filter out stdlib and local project modules
        local_modules = _get_local_modules(project_dir)
        third_party = all_imports - stdlib - local_modules

        packages = [resolve_package_name(name) for name in third_party]

    # Apply exclude/include
    exclude_lower = {e.lower() for e in exclude}
    packages = [p for p in packages if p.lower() not in exclude_lower]

    for inc in include:
        if inc not in packages:
            packages.append(inc)

    return sorted(set(packages))


def _get_local_modules(project_dir: Path) -> set[str]:
    """Get module names that are local to the project (not third-party)."""
    local: set[str] = set()
    for item in project_dir.iterdir():
        if item.is_file() and item.suffix == ".py":
            local.add(item.stem)
        elif item.is_dir() and (item / "__init__.py").is_file():
            local.add(item.name)
    return local

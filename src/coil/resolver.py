"""Dependency resolution for Python projects."""

import importlib.metadata
import re
from collections.abc import Callable
from pathlib import Path

from coil.scanner import scan_project
from coil.utils.package_map import IMPORT_TO_PYPI
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


def _build_dist_map() -> dict[str, list[str]]:
    """Return {top_level_module: sorted_distribution_names} for the current env.

    Wraps ``importlib.metadata.packages_distributions()`` and sorts each value
    so that multi-distribution resolution is deterministic. Returns an empty
    dict if metadata lookup fails.
    """
    try:
        raw = importlib.metadata.packages_distributions()
    except Exception:
        return {}
    return {mod: sorted(dists) for mod, dists in raw.items()}


def resolve_from_imports(
    project_dir: Path,
    python_version: str,
    dist_map: dict[str, list[str]] | None = None,
    warn: Callable[[str], None] | None = None,
) -> list[str]:
    """Resolve third-party distribution names from the project's imports.

    Walks every ``.py`` file under ``project_dir``, filters out stdlib and
    first-party modules, and maps each remaining top-level module name to a
    PyPI distribution name.

    Mapping strategy (per module):

    1. If ``dist_map`` has an entry for the module, use the first distribution
       in sorted order. If there is more than one, call ``warn`` with a
       one-line message so the caller can surface the ambiguity.
    2. Otherwise, fall back to the hand-maintained ``IMPORT_TO_PYPI`` map.
       This only fires for modules that aren't installed in Coil's own env
       — the typical fresh-clone case.
    3. If neither has an entry, assume the distribution name equals the
       module name.

    A single distribution that exposes several top-level modules (e.g. pywin32
    ships ``win32api``, ``win32file``, ``win32pipe``, ``pywintypes``, ...) is
    naturally deduplicated because all of its modules map to the same name.
    """
    if dist_map is None:
        dist_map = _build_dist_map()

    stdlib = get_stdlib_modules(python_version)
    local = _get_local_modules(project_dir)
    imports = scan_project(project_dir) - stdlib - local

    result: set[str] = set()
    for mod in sorted(imports):
        dists = dist_map.get(mod)
        if dists:
            chosen = dists[0]
            if len(dists) > 1 and warn is not None:
                warn(
                    f"Module '{mod}' is provided by multiple distributions "
                    f"({', '.join(dists)}); using '{chosen}'."
                )
            result.add(chosen)
            continue
        mapped = IMPORT_TO_PYPI.get(mod)
        if mapped is not None:
            result.add(mapped)
        else:
            result.add(mod)
    return sorted(result)


def resolve_dependencies(
    project_dir: Path,
    python_version: str,
    requirements_path: str | None = None,
    exclude: list[str] | None = None,
    include: list[str] | None = None,
    auto: bool = True,
    dist_map: dict[str, list[str]] | None = None,
    warn: Callable[[str], None] | None = None,
) -> list[str]:
    """Resolve all third-party dependencies for a project.

    Sources, in order:

    1. An explicit ``requirements_path`` — exclusive source (legacy escape
       hatch). Still unioned with ``include`` at the end.
    2. ``requirements.txt`` in ``project_dir`` — same: exclusive when present.
    3. Otherwise, unions:
       - ``[project].dependencies`` from ``pyproject.toml`` (if present),
       - Imports auto-detected from project source (when ``auto`` is True),
       - Anything in ``include``.

    ``exclude`` trims the final set as the last step, regardless of source.

    Auto-detection is additive: it never drops version pins declared in
    ``[project].dependencies``.

    Args:
        project_dir: Path to the project directory.
        python_version: Target Python version (e.g. "3.12").
        requirements_path: Explicit path to a requirements file.
        exclude: Package names to exclude (case-insensitive).
        include: Package names to force-include.
        auto: When True, auto-detect imports and union with declared deps.
        dist_map: Override for ``importlib.metadata.packages_distributions()``
            (primarily for tests). When None, introspects the current env.
        warn: Optional one-arg callable for non-fatal warnings (e.g.
            ambiguous multi-distribution resolutions).

    Returns:
        Sorted list of PyPI distribution names.
    """
    exclude = exclude or []
    include = include or []
    packages: set[str] = set()

    if requirements_path:
        req_path = Path(requirements_path)
        if req_path.name.endswith(".toml"):
            packages.update(parse_pyproject_toml(req_path))
        else:
            packages.update(parse_requirements_txt(req_path))
    elif (project_dir / "requirements.txt").is_file():
        packages.update(parse_requirements_txt(project_dir / "requirements.txt"))
    else:
        pyproject = project_dir / "pyproject.toml"
        if pyproject.is_file():
            packages.update(parse_pyproject_toml(pyproject))
        if auto:
            packages.update(
                resolve_from_imports(
                    project_dir,
                    python_version,
                    dist_map=dist_map,
                    warn=warn,
                )
            )

    packages.update(include)

    exclude_lower = {e.lower() for e in exclude}
    return sorted({p for p in packages if p.lower() not in exclude_lower})


def _get_local_modules(project_dir: Path) -> set[str]:
    """Get module names that are local to the project (not third-party)."""
    local: set[str] = set()
    for item in project_dir.iterdir():
        if item.is_file() and item.suffix == ".py":
            local.add(item.stem)
        elif item.is_dir() and (item / "__init__.py").is_file():
            local.add(item.name)
    return local

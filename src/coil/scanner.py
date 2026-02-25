"""AST-based import scanner for Python projects."""

import ast
from pathlib import Path

from coil.utils.gui_frameworks import GUI_IMPORTS


def find_py_files(project_dir: Path) -> list[Path]:
    """Recursively find all .py files in a project directory."""
    return sorted(project_dir.rglob("*.py"))


def extract_imports(source: str) -> set[str]:
    """Extract top-level module names from Python source code using AST.

    Returns the root module name for each import. For example:
    - ``import os.path`` returns ``os``
    - ``from collections.abc import Mapping`` returns ``collections``
    - ``import numpy`` returns ``numpy``
    """
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return set()

    modules: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                root = alias.name.split(".")[0]
                modules.add(root)
        elif isinstance(node, ast.ImportFrom):
            if node.module is not None and node.level == 0:
                root = node.module.split(".")[0]
                modules.add(root)

    return modules


def scan_project(project_dir: Path) -> set[str]:
    """Scan all .py files in a project and return all imported module names.

    Parses every .py file recursively using the ast module. Collects every
    import and from...import statement regardless of where it appears in the
    file. Returns the set of top-level module names.
    """
    all_imports: set[str] = set()
    py_files = find_py_files(project_dir)

    for py_file in py_files:
        try:
            source = py_file.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        all_imports.update(extract_imports(source))

    return all_imports


def detect_gui_imports(project_dir: Path) -> list[str]:
    """Detect GUI framework imports in a project.

    Returns sorted list of GUI import names found, or empty list if none.
    """
    all_imports = scan_project(project_dir)
    found = sorted(all_imports & GUI_IMPORTS)
    return found

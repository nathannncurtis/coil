"""AST-based import scanner for Python projects."""

import ast
from pathlib import Path

from coil.utils.gui_frameworks import GUI_IMPORTS


def find_py_files(project_dir: Path) -> list[Path]:
    """Recursively find all .py files in a project directory."""
    return sorted(project_dir.rglob("*.py"))


_DYNAMIC_IMPORT_CALLEES = {"import_module", "__import__"}


def _dynamic_import_literal(node: ast.Call) -> str | None:
    """Return the first positional arg of a dynamic-import call if it's a string literal.

    Matches:
    - ``importlib.import_module("x")``
    - ``import_module("x")`` (when ``from importlib import import_module`` is in scope)
    - ``__import__("x")``

    Non-literal first args (variables, expressions) return None.
    """
    func = node.func
    if isinstance(func, ast.Attribute):
        if func.attr != "import_module":
            return None
    elif isinstance(func, ast.Name):
        if func.id not in _DYNAMIC_IMPORT_CALLEES:
            return None
    else:
        return None

    if not node.args:
        return None
    first = node.args[0]
    if isinstance(first, ast.Constant) and isinstance(first.value, str):
        return first.value
    return None


def extract_imports(source: str) -> set[str]:
    """Extract top-level module names from Python source code using AST.

    Returns the root module name for each import. For example:
    - ``import os.path`` returns ``os``
    - ``from collections.abc import Mapping`` returns ``collections``
    - ``import numpy`` returns ``numpy``

    Also detects dynamic imports with a string literal argument:
    - ``importlib.import_module("win32pipe")`` returns ``win32pipe``
    - ``__import__("win32file")`` returns ``win32file``

    Dynamic imports with non-literal arguments are skipped silently.
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
        elif isinstance(node, ast.Call):
            literal = _dynamic_import_literal(node)
            if literal:
                root = literal.split(".")[0]
                if root:
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


def file_has_gui_imports(file_path: Path) -> bool:
    """Check if a single .py file imports any GUI frameworks."""
    try:
        source = file_path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return False
    imports = extract_imports(source)
    return bool(imports & GUI_IMPORTS)

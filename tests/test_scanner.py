"""Tests for the AST import scanner."""

from pathlib import Path

from coil.scanner import extract_imports, find_py_files, scan_project

FIXTURES = Path(__file__).parent / "fixtures" / "sample_project"


def test_extract_imports_basic():
    source = "import os\nimport sys\n"
    result = extract_imports(source)
    assert result == {"os", "sys"}


def test_extract_imports_from():
    source = "from collections.abc import Mapping\n"
    result = extract_imports(source)
    assert result == {"collections"}


def test_extract_imports_dotted():
    source = "import os.path\n"
    result = extract_imports(source)
    assert result == {"os"}


def test_extract_imports_third_party():
    source = "import requests\nimport numpy as np\nfrom PIL import Image\n"
    result = extract_imports(source)
    assert result == {"requests", "numpy", "PIL"}


def test_extract_imports_relative_ignored():
    source = "from . import foo\nfrom .bar import baz\n"
    result = extract_imports(source)
    assert result == set()


def test_extract_imports_syntax_error():
    source = "this is not valid python {{{"
    result = extract_imports(source)
    assert result == set()


def test_extract_imports_deep_in_file():
    source = (
        "x = 1\n" * 100
        + "import late_import\n"
        + "y = 2\n" * 100
    )
    result = extract_imports(source)
    assert "late_import" in result


def test_find_py_files():
    files = find_py_files(FIXTURES)
    names = {f.name for f in files}
    assert "main.py" in names
    assert "utils.py" in names


def test_scan_project():
    result = scan_project(FIXTURES)
    assert "os" in result
    assert "sys" in result
    assert "requests" in result
    assert "numpy" in result
    assert "PIL" in result
    assert "flask" in result
    assert "bs4" in result
    assert "json" in result
    assert "csv" in result
    assert "collections" in result

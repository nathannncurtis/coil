"""Tests for the AST import scanner."""

from pathlib import Path

from coil.scanner import extract_imports, find_py_files, scan_project, detect_gui_imports

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


def test_extract_imports_importlib_literal():
    source = (
        "import importlib\n"
        "win32pipe = importlib.import_module('win32pipe')\n"
    )
    result = extract_imports(source)
    assert "win32pipe" in result


def test_extract_imports_importlib_aliased():
    source = (
        "from importlib import import_module\n"
        "mod = import_module('pywintypes')\n"
    )
    result = extract_imports(source)
    assert "pywintypes" in result


def test_extract_imports_dunder_import_literal():
    source = "mod = __import__('win32file')\n"
    result = extract_imports(source)
    assert "win32file" in result


def test_extract_imports_dynamic_non_literal_skipped():
    source = (
        "import importlib\n"
        "name = get_name()\n"
        "mod = importlib.import_module(name)\n"
    )
    result = extract_imports(source)
    assert "importlib" in result  # the plain import is still picked up
    # But no spurious module should be added from the dynamic call.
    assert result == {"importlib"}


def test_extract_imports_importlib_dotted_literal():
    source = "import importlib\nm = importlib.import_module('a.b.c')\n"
    result = extract_imports(source)
    assert "a" in result
    assert "b" not in result
    assert "c" not in result


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


def test_detect_gui_imports_tkinter(tmp_path: Path):
    (tmp_path / "app.py").write_text("import tkinter\nfrom tkinter import ttk\n")
    found = detect_gui_imports(tmp_path)
    assert "tkinter" in found


def test_detect_gui_imports_pyqt5(tmp_path: Path):
    (tmp_path / "app.py").write_text("from PyQt5.QtWidgets import QApplication\n")
    found = detect_gui_imports(tmp_path)
    assert "PyQt5" in found


def test_detect_gui_imports_pygame(tmp_path: Path):
    (tmp_path / "game.py").write_text("import pygame\npygame.init()\n")
    found = detect_gui_imports(tmp_path)
    assert "pygame" in found


def test_detect_gui_imports_wx(tmp_path: Path):
    (tmp_path / "app.py").write_text("import wx\n")
    found = detect_gui_imports(tmp_path)
    assert "wx" in found


def test_detect_gui_imports_pystray(tmp_path: Path):
    (tmp_path / "tray.py").write_text("import pystray\n")
    found = detect_gui_imports(tmp_path)
    assert "pystray" in found


def test_detect_gui_imports_multiple(tmp_path: Path):
    (tmp_path / "app.py").write_text("import tkinter\nimport pygame\n")
    found = detect_gui_imports(tmp_path)
    assert "tkinter" in found
    assert "pygame" in found


def test_detect_gui_imports_none(tmp_path: Path):
    (tmp_path / "app.py").write_text("import os\nimport sys\nimport requests\n")
    found = detect_gui_imports(tmp_path)
    assert found == []


def test_detect_gui_imports_empty(tmp_path: Path):
    found = detect_gui_imports(tmp_path)
    assert found == []

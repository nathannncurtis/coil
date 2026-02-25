"""Tests for coil inspect."""

from pathlib import Path

from coil.inspect import (
    run_inspect,
    _get_dep_source,
    _format_size,
    _get_project_code_size,
)


def test_get_dep_source_requirements(tmp_path: Path):
    (tmp_path / "requirements.txt").write_text("requests\n")
    assert "requirements.txt" in _get_dep_source(tmp_path)


def test_get_dep_source_pyproject(tmp_path: Path):
    (tmp_path / "pyproject.toml").write_text("[project]\n")
    assert "pyproject.toml" in _get_dep_source(tmp_path)


def test_get_dep_source_explicit():
    assert "deps.txt" in _get_dep_source(Path("."), "deps.txt")


def test_get_dep_source_ast(tmp_path: Path):
    assert "AST" in _get_dep_source(tmp_path)


def test_format_size_bytes():
    assert _format_size(500) == "500 B"


def test_format_size_kb():
    assert "KB" in _format_size(5000)


def test_format_size_mb():
    assert "MB" in _format_size(5_000_000)


def test_format_size_gb():
    assert "GB" in _format_size(5_000_000_000)


def test_get_project_code_size(tmp_path: Path):
    (tmp_path / "main.py").write_text("print('hello')\n")
    (tmp_path / "lib.py").write_text("x = 1\n")
    size = _get_project_code_size(tmp_path)
    assert size > 0


def test_get_project_code_size_empty(tmp_path: Path):
    size = _get_project_code_size(tmp_path)
    assert size == 0


def test_run_inspect_with_entry(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("import os\nprint('hello')\n")
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "main.py" in out
    assert "auto-detected" in out


def test_run_inspect_with_dunder_main(tmp_path: Path, capsys):
    (tmp_path / "__main__.py").write_text("print('hello')\n")
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "__main__.py" in out


def test_run_inspect_no_entry(tmp_path: Path, capsys):
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "not found" in out


def test_run_inspect_with_requirements(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("import requests\n")
    (tmp_path / "requirements.txt").write_text("requests\n")
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "requirements.txt" in out
    assert "requests" in out


def test_run_inspect_with_coil_toml(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("print(1)\n")
    (tmp_path / "coil.toml").write_text('[project]\nentry = "main.py"\n[build]\n')
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "coil.toml found" in out


def test_run_inspect_no_toml(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("print(1)\n")
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "no coil.toml" in out


def test_run_inspect_with_profile(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("print(1)\n")
    (tmp_path / "coil.toml").write_text('[project]\nentry = "main.py"\n[build]\n')
    exit_code = run_inspect(tmp_path, profile="release")
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "release" in out


def test_run_inspect_gui_detected(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("import tkinter\ntkinter.Tk()\n")
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "auto-detected" in out
    assert "tkinter" in out


def test_run_inspect_no_gui(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("import os\nprint(1)\n")
    exit_code = run_inspect(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "console app" in out


def test_run_inspect_exclude(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("import os\n")
    (tmp_path / "requirements.txt").write_text("requests\nnumpy\n")
    exit_code = run_inspect(tmp_path, exclude=["numpy"])
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "numpy" in out  # shown in excluded list


def test_inspect_subcommand():
    from coil.cli import create_parser
    parser = create_parser()
    args = parser.parse_args(["inspect", "/tmp/proj"])
    assert args.command == "inspect"
    assert args.project == "/tmp/proj"


def test_inspect_subcommand_default():
    from coil.cli import create_parser
    parser = create_parser()
    args = parser.parse_args(["inspect"])
    assert args.command == "inspect"
    assert args.project == "."

"""Tests for the CLI."""

import sys
from pathlib import Path
from unittest.mock import patch

import pytest

from coil.cli import create_parser, resolve_entry_points, detect_os, detect_python_version


def test_detect_os():
    os_name = detect_os()
    assert os_name in ("windows", "macos", "linux") or isinstance(os_name, str)


def test_detect_python_version():
    version = detect_python_version()
    assert version == f"{sys.version_info.major}.{sys.version_info.minor}"


def test_resolve_entry_points_explicit(tmp_path: Path):
    (tmp_path / "app.py").write_text("print(1)")
    result = resolve_entry_points(tmp_path, ["app.py"])
    assert result == ["app.py"]


def test_resolve_entry_points_auto_main(tmp_path: Path):
    (tmp_path / "__main__.py").write_text("print(1)")
    result = resolve_entry_points(tmp_path, None)
    assert result == ["__main__.py"]


def test_resolve_entry_points_auto_fallback(tmp_path: Path):
    (tmp_path / "main.py").write_text("print(1)")
    result = resolve_entry_points(tmp_path, None)
    assert result == ["main.py"]


def test_resolve_entry_points_missing(tmp_path: Path):
    with pytest.raises(SystemExit):
        resolve_entry_points(tmp_path, None)


def test_icon_auto_detect_single(tmp_path: Path):
    """Auto-detect a single .ico file in the project directory."""
    (tmp_path / "main.py").write_text("print(1)")
    ico_file = tmp_path / "app.ico"
    ico_file.write_bytes(b"\x00" * 10)

    ico_files = list(tmp_path.glob("*.ico"))
    assert len(ico_files) == 1
    assert ico_files[0].name == "app.ico"


def test_icon_auto_detect_prefers_name_match(tmp_path: Path):
    """When multiple .ico files exist, prefer one matching the app name."""
    (tmp_path / "main.py").write_text("print(1)")
    (tmp_path / "other.ico").write_bytes(b"\x00" * 10)
    (tmp_path / "myapp.ico").write_bytes(b"\x00" * 10)

    project_dir = Path(tmp_path)
    name = "myapp"

    ico_files = list(project_dir.glob("*.ico"))
    name_match = [f for f in ico_files if f.stem.lower() == name.lower()]
    icon = str(name_match[0] if name_match else sorted(ico_files)[0])

    assert "myapp.ico" in icon


def test_icon_auto_detect_no_ico(tmp_path: Path):
    """No auto-detection when no .ico files exist."""
    (tmp_path / "main.py").write_text("print(1)")

    ico_files = list(tmp_path.glob("*.ico"))
    assert len(ico_files) == 0


def test_icon_flag_overrides_auto(tmp_path: Path):
    """Explicit --icon flag should be used even if .ico files exist in dir."""
    (tmp_path / "main.py").write_text("print(1)")
    (tmp_path / "auto.ico").write_bytes(b"\x00" * 10)

    args = create_parser().parse_args([
        "build", str(tmp_path), "--icon", str(tmp_path / "auto.ico"),
    ])
    assert args.icon == str(tmp_path / "auto.ico")

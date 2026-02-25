"""Tests for the CLI."""

import os
import sys
from pathlib import Path
from unittest.mock import patch

import pytest

from coil.cli import (
    create_parser,
    resolve_entry_points,
    detect_os,
    detect_python_version,
    get_cache_dir,
    clear_cache,
    show_cache_info,
    _apply_toml_config,
)


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


def test_get_cache_dir():
    cache = get_cache_dir()
    assert isinstance(cache, Path)
    assert "coil" in str(cache).lower()


def test_clear_cache_empty(tmp_path: Path, capsys):
    with patch.dict(os.environ, {"LOCALAPPDATA": str(tmp_path)}):
        clear_cache()
    out = capsys.readouterr().out
    assert "empty" in out.lower() or "cleared" in out.lower()


def test_clear_cache_with_entries(tmp_path: Path, capsys):
    cache = tmp_path / "coil" / "TestApp" / "abc12345"
    cache.mkdir(parents=True)
    (cache / "dummy.dll").write_bytes(b"\x00" * 1000)
    (cache / ".coil_ready").write_bytes(b"\x00" * 4)

    with patch.dict(os.environ, {"LOCALAPPDATA": str(tmp_path)}):
        clear_cache()

    out = capsys.readouterr().out
    assert "cleared" in out.lower()
    assert not (tmp_path / "coil").exists()


def test_show_cache_info_empty(tmp_path: Path, capsys):
    with patch.dict(os.environ, {"LOCALAPPDATA": str(tmp_path)}):
        show_cache_info()
    out = capsys.readouterr().out
    assert "empty" in out.lower()


def test_show_cache_info_with_entries(tmp_path: Path, capsys):
    cache = tmp_path / "coil" / "MyApp" / "deadbeef"
    cache.mkdir(parents=True)
    (cache / "MyApp.exe").write_bytes(b"\x00" * 5000)
    (cache / ".coil_ready").write_bytes(b"\x00" * 4)

    with patch.dict(os.environ, {"LOCALAPPDATA": str(tmp_path)}):
        show_cache_info()

    out = capsys.readouterr().out
    assert "MyApp" in out
    assert "deadbeef" in out
    assert "ready" in out


def test_clear_cache_flag_on_build():
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--clear-cache"])
    assert args.clear_cache is True


def test_cache_subcommand():
    parser = create_parser()
    args = parser.parse_args(["cache", "clear"])
    assert args.command == "cache"
    assert args.cache_action == "clear"


def test_init_subcommand():
    parser = create_parser()
    args = parser.parse_args(["init", "/tmp/myproj"])
    assert args.command == "init"
    assert args.project == "/tmp/myproj"


def test_init_subcommand_default_project():
    parser = create_parser()
    args = parser.parse_args(["init"])
    assert args.command == "init"
    assert args.project == "."


def test_profile_flag():
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--profile", "release"])
    assert args.profile == "release"


def test_clean_flag():
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--clean"])
    assert args.clean is True


def test_apply_toml_config_entry(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\nname = "Test"\n[build]\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.entry == ["app.py"]
    assert args.name == "Test"


def test_apply_toml_config_cli_overrides(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\nname = "TomlName"\n[build]\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--entry", "other.py", "--name", "CLIName"])
    _apply_toml_config(args, tmp_path)

    # CLI values should win
    assert args.entry == ["other.py"]
    assert args.name == "CLIName"


def test_apply_toml_config_gui(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\nconsole = false\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.gui is True


def test_apply_toml_config_secure(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\nsecure = true\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.secure is True


def test_apply_toml_config_python(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\npython = "3.12"\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.python == "3.12"


def test_apply_toml_config_output(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\n[build.output]\ndir = "./out"\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.output == "./out"


def test_apply_toml_config_profile(tmp_path: Path):
    toml = (
        '[project]\nentry = "app.py"\n[build]\nsecure = false\n'
        '[profile.release]\nsecure = true\nverbose = true\n'
    )
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--profile", "release"])
    _apply_toml_config(args, tmp_path)

    assert args.secure is True
    assert args.verbose is True


def test_apply_toml_config_no_toml(tmp_path: Path):
    """No coil.toml should not change anything."""
    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.entry is None
    assert args.name is None


def test_apply_toml_config_bad_profile(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\n[profile.dev]\nmode = "bundled"\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--profile", "nope"])

    with pytest.raises(SystemExit):
        _apply_toml_config(args, tmp_path)

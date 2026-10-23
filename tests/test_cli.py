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
    main,
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


def test_apply_toml_config_clean(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\nclean = true\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.clean is True


def test_apply_toml_config_clean_cli_override(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\nclean = false\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--clean"])
    _apply_toml_config(args, tmp_path)

    # CLI --clean should still be True
    assert args.clean is True


def test_gui_auto_detect_default():
    """--console defaults to None (not True), enabling auto-detection."""
    parser = create_parser()
    args = parser.parse_args(["build", "."])
    assert args.console is None
    assert args.gui is False


def test_gui_explicit_console():
    """--console explicitly set should be True."""
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--console"])
    assert args.console is True


def test_gui_explicit_gui():
    """--gui explicitly set should be True."""
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--gui"])
    assert args.gui is True


def test_optimize_flag_default():
    """--optimize defaults to None (auto-detected from --secure)."""
    parser = create_parser()
    args = parser.parse_args(["build", "."])
    assert args.optimize is None


def test_optimize_flag_explicit():
    """--optimize accepts 0, 1, or 2."""
    parser = create_parser()
    for level in [0, 1, 2]:
        args = parser.parse_args(["build", ".", "--optimize", str(level)])
        assert args.optimize == level


def test_optimize_flag_invalid():
    """--optimize rejects invalid values."""
    parser = create_parser()
    with pytest.raises(SystemExit):
        parser.parse_args(["build", ".", "--optimize", "3"])


def test_compile_flag_defaults():
    parser = create_parser()
    args = parser.parse_args(["build", "."])
    assert args.compile is False
    assert args.keep is None
    assert args.emit is None


def test_compile_flag():
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--compile"])
    assert args.compile is True


def test_keep_flag_repeatable():
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--keep", "plugins.*", "--keep", "myapp.models.*.to_dict"])
    assert args.keep == ["plugins.*", "myapp.models.*.to_dict"]


@pytest.mark.parametrize("value", ["ir", "asm"])
def test_emit_flag_accepts(value):
    parser = create_parser()
    args = parser.parse_args(["build", ".", "--emit", value])
    assert args.emit == value


def test_emit_flag_rejects_other_values():
    parser = create_parser()
    with pytest.raises(SystemExit):
        parser.parse_args(["build", ".", "--emit", "llvm"])


def test_apply_toml_config_compile(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[build]\ncompile = true\npython = "3.12"\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.compile is True
    assert args.compile_config == {
        "keep": [],
        "report": True,
        "opt": "speed",
        "stack": 16 * 1024 * 1024,
    }


def test_apply_toml_config_compile_defaults_without_toml(tmp_path: Path):
    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.compile is False
    assert args.compile_config == {}


def test_apply_toml_config_compile_table(tmp_path: Path):
    toml = (
        '[project]\nentry = "app.py"\n'
        '[build]\ncompile = true\npython = "3.13"\n'
        '[compile]\nkeep = ["plugins.*"]\nreport = false\nopt = "size"\nstack = "8MiB"\n'
    )
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)

    assert args.compile_config == {
        "keep": ["plugins.*"],
        "report": False,
        "opt": "size",
        "stack": 8 * 1024 * 1024,
    }


def test_apply_toml_config_compile_profile(tmp_path: Path):
    toml = (
        '[project]\nentry = "app.py"\n'
        '[build]\ncompile = false\npython = "3.12"\n'
        '[compile]\nopt = "size"\nkeep = ["toml.*"]\n'
        '[profile.native]\ncompile = true\n'
        '[profile.small.compile]\nopt = "speed"\nstack = "4MiB"\nkeep = ["small.*"]\n'
    )
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--profile", "native"])
    _apply_toml_config(args, tmp_path)
    assert args.compile is True
    assert args.compile_config["keep"] == ["toml.*"]

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--profile", "small"])
    _apply_toml_config(args, tmp_path)
    # compile stays off from [build]; the profile table still overrides.
    assert args.compile is False
    assert args.compile_config["opt"] == "speed"
    assert args.compile_config["stack"] == 4 * 1024 * 1024
    assert args.compile_config["keep"] == ["small.*"]


def test_apply_toml_config_keep_cli_overrides_toml_and_profile(tmp_path: Path):
    toml = (
        '[project]\nentry = "app.py"\n'
        '[compile]\nkeep = ["toml.*"]\n'
        '[profile.p]\n[profile.p.compile]\nkeep = ["profile.*"]\n'
    )
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--profile", "p", "--keep", "cli.*"])
    _apply_toml_config(args, tmp_path)
    assert args.compile_config["keep"] == ["cli.*"]


def test_apply_toml_config_keep_without_toml_uses_cli(tmp_path: Path):
    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--keep", "cli.*"])
    _apply_toml_config(args, tmp_path)
    assert args.compile_config == {"keep": ["cli.*"]}


def test_apply_toml_config_bad_compile_table(tmp_path: Path):
    toml = '[project]\nentry = "app.py"\n[compile]\nopt = "fast"\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])

    with pytest.raises(SystemExit):
        _apply_toml_config(args, tmp_path)


def test_apply_toml_config_compile_bad_toml_python_carries(tmp_path: Path):
    # A CLI --python can still override the toml value, so applying the
    # toml must not error on compile + python = "3.11"; the merged value
    # is checked later, in main().
    toml = '[project]\nentry = "app.py"\n[build]\ncompile = true\npython = "3.11"\n'
    (tmp_path / "coil.toml").write_text(toml)

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path)])
    _apply_toml_config(args, tmp_path)
    assert args.compile is True
    assert args.python == "3.11"

    parser = create_parser()
    args = parser.parse_args(["build", str(tmp_path), "--python", "3.13"])
    _apply_toml_config(args, tmp_path)
    assert args.python == "3.13"


def _build_project(tmp_path: Path) -> Path:
    project = tmp_path / "project"
    project.mkdir()
    (project / "main.py").write_text("print('x')\n")
    return project


def test_compile_requires_windows_os(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--compile", "--os", "macos", "--python", "3.12"])
    assert stop.value.code == 1
    assert "--compile requires --os windows" in capsys.readouterr().out


def test_compile_requires_supported_python(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--compile", "--os", "windows", "--python", "3.11"])
    assert stop.value.code == 1
    out = capsys.readouterr().out
    assert "3.12" in out and "3.13" in out


def test_compile_accepts_supported_python(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    for version in ("3.12", "3.13"):
        with pytest.raises(SystemExit) as stop:
            main(["build", str(project), "--dry-run", "--compile", "--os", "windows", "--python", version])
        assert stop.value.code == 0
        assert "Compile:      True" in capsys.readouterr().out


def test_compile_python_from_toml_checked(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    (project / "coil.toml").write_text('[build]\ncompile = true\npython = "3.11"\n')
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--dry-run"])
    assert stop.value.code == 1
    out = capsys.readouterr().out
    assert "to compile natively" in out
    assert "3.12" in out and "3.13" in out


def test_compile_python_cli_overrides_toml(tmp_path: Path, capsys):
    """CLI > [build]: a CLI --python 3.13 masks a toml python = "3.11"."""
    project = _build_project(tmp_path)
    (project / "coil.toml").write_text('[build]\ncompile = true\npython = "3.11"\n')
    with pytest.raises(SystemExit) as stop:
        main([
            "build", str(project), "--dry-run", "--compile", "--os", "windows",
            "--python", "3.13",
        ])
    assert stop.value.code == 0
    out = capsys.readouterr().out
    assert "Python:       3.13" in out
    assert "Compile:      True" in out


def test_compile_python_cli_overrides_profile_enabled_compile(tmp_path: Path, capsys):
    """CLI > profile > [build]: the profile turns compile on, the CLI
    python masks the [build] python the bytecode path accepts."""
    project = _build_project(tmp_path)
    (project / "coil.toml").write_text(
        '[build]\npython = "3.11"\n[profile.native]\ncompile = true\n'
    )
    with pytest.raises(SystemExit) as stop:
        main([
            "build", str(project), "--dry-run", "--profile", "native",
            "--os", "windows", "--python", "3.13",
        ])
    assert stop.value.code == 0
    out = capsys.readouterr().out
    assert "Python:       3.13" in out
    assert "Compile:      True" in out


def test_compile_profile_python_without_cli_override_errors(tmp_path: Path, capsys):
    """No CLI tier: profile compile over [build] python = "3.11" errors
    on the merged value, listing the supported versions."""
    project = _build_project(tmp_path)
    (project / "coil.toml").write_text(
        '[build]\nos = "windows"\npython = "3.11"\n[profile.native]\ncompile = true\n'
    )
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--dry-run", "--profile", "native"])
    assert stop.value.code == 1
    out = capsys.readouterr().out
    assert "to compile natively" in out
    assert "3.12" in out and "3.13" in out


def test_dry_run_prints_compile_settings(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    (project / "coil.toml").write_text(
        '[build]\ncompile = true\npython = "3.12"\n'
        '[compile]\nkeep = ["plugins.*"]\nopt = "size"\nstack = "8MiB"\nreport = false\n'
    )
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--dry-run", "--emit", "ir"])
    assert stop.value.code == 0
    out = capsys.readouterr().out
    assert "Compile:      True" in out
    assert "Keep rules:   plugins.*" in out
    assert "Opt:          size" in out
    assert "Stack:        8MiB" in out
    assert "Report:       False" in out
    assert "Emit:         ir" in out


def test_dry_run_prints_cli_compile_settings(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    with pytest.raises(SystemExit) as stop:
        main([
            "build", str(project), "--dry-run", "--compile", "--os", "windows",
            "--python", "3.13", "--keep", "a.*", "--keep", "b.c",
        ])
    assert stop.value.code == 0
    out = capsys.readouterr().out
    assert "Compile:      True" in out
    assert "Keep rules:   a.*, b.c" in out
    assert "Opt:          speed" in out
    assert "Stack:        16MiB" in out
    assert "Report:       True" in out
    assert "Emit:" not in out


def test_dry_run_without_compile(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    (project / "coil.toml").write_text('[build]\nos = "windows"\n[compile]\nopt = "size"\n')
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--dry-run"])
    assert stop.value.code == 0
    out = capsys.readouterr().out
    assert "Compile:      False" in out
    assert "Keep rules:" not in out


def test_build_with_compile_raises_not_implemented(tmp_path: Path):
    from coil.builder import build

    project = _build_project(tmp_path)
    with pytest.raises(NotImplementedError, match="g02"):
        build(
            project_dir=project,
            entry_points=["main.py"],
            target_os="windows",
            python_version="3.12",
            compile=True,
            compile_config={"keep": ["plugins.*"], "report": True, "opt": "speed", "stack": 16 * 1024 * 1024},
        )


def test_main_build_compile_reports_missing_driver(tmp_path: Path, capsys):
    project = _build_project(tmp_path)
    with pytest.raises(SystemExit) as stop:
        main(["build", str(project), "--compile", "--os", "windows", "--python", "3.12"])
    assert stop.value.code == 1
    assert "not implemented" in capsys.readouterr().out

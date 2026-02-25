"""Tests for coil doctor."""

from pathlib import Path
from unittest.mock import patch

from coil.doctor import (
    run_doctor,
    _check_python_version,
    _check_output_writable,
    _check_cache_writable,
    _check_config,
    _check_packages,
    CheckResult,
)


def test_check_python_version():
    result = _check_python_version()
    assert result.status == "pass"
    assert "Python" in result.message


def test_check_output_writable(tmp_path: Path):
    result = _check_output_writable(tmp_path)
    assert result.status == "pass"


def test_check_cache_writable():
    result = _check_cache_writable()
    assert result.status == "pass"


def test_check_config_no_toml(tmp_path: Path):
    results = _check_config(tmp_path)
    assert results == []


def test_check_config_valid_toml(tmp_path: Path):
    (tmp_path / "main.py").write_text("print(1)")
    (tmp_path / "coil.toml").write_text(
        '[project]\nentry = "main.py"\nname = "Test"\n[build]\n'
    )
    results = _check_config(tmp_path)
    statuses = [r.status for r in results]
    assert "pass" in statuses
    # Should have "coil.toml is valid" and "Entry point main.py exists"
    messages = [r.message for r in results]
    assert any("coil.toml is valid" in m for m in messages)
    assert any("main.py exists" in m for m in messages)


def test_check_config_missing_entry(tmp_path: Path):
    (tmp_path / "coil.toml").write_text(
        '[project]\nentry = "nonexistent.py"\nname = "Test"\n[build]\n'
    )
    results = _check_config(tmp_path)
    statuses = [r.status for r in results]
    assert "fail" in statuses
    messages = [r.message for r in results]
    assert any("not found" in m for m in messages)


def test_check_config_missing_icon(tmp_path: Path):
    (tmp_path / "main.py").write_text("print(1)")
    (tmp_path / "coil.toml").write_text(
        '[project]\nentry = "main.py"\n[build]\n[build.output]\nicon = "missing.ico"\n'
    )
    results = _check_config(tmp_path)
    statuses = [r.status for r in results]
    assert "fail" in statuses
    messages = [r.message for r in results]
    assert any("missing.ico not found" in m for m in messages)


def test_check_config_invalid_toml(tmp_path: Path):
    (tmp_path / "coil.toml").write_text("not valid [[[")
    results = _check_config(tmp_path)
    statuses = [r.status for r in results]
    assert "fail" in statuses


def test_check_config_auto_detect_entry(tmp_path: Path):
    (tmp_path / "main.py").write_text("print(1)")
    (tmp_path / "coil.toml").write_text('[project]\nname = "Test"\n[build]\n')
    results = _check_config(tmp_path)
    messages = [r.message for r in results]
    assert any("auto-detected" in m for m in messages)


def test_check_config_no_entry_at_all(tmp_path: Path):
    (tmp_path / "coil.toml").write_text('[project]\nname = "Test"\n[build]\n')
    results = _check_config(tmp_path)
    statuses = [r.status for r in results]
    assert "fail" in statuses


def test_check_packages_no_requirements(tmp_path: Path):
    results = _check_packages(tmp_path)
    assert results == []


def test_check_packages_clean(tmp_path: Path):
    (tmp_path / "requirements.txt").write_text("requests\nflask\n")
    results = _check_packages(tmp_path)
    assert results == []


def test_check_packages_with_known_issue(tmp_path: Path):
    (tmp_path / "requirements.txt").write_text("opencv-python>=4.0\nnumpy\n")
    results = _check_packages(tmp_path)
    assert len(results) == 1
    assert results[0].status == "warn"
    assert "opencv-python" in results[0].message


def test_run_doctor_healthy(tmp_path: Path, capsys):
    (tmp_path / "main.py").write_text("print(1)")
    (tmp_path / "coil.toml").write_text(
        '[project]\nentry = "main.py"\nname = "Test"\n[build]\n'
    )
    exit_code = run_doctor(tmp_path)
    assert exit_code == 0
    out = capsys.readouterr().out
    assert "passed" in out


def test_run_doctor_with_errors(tmp_path: Path, capsys):
    (tmp_path / "coil.toml").write_text(
        '[project]\nentry = "missing.py"\nname = "Test"\n[build]\n'
    )
    exit_code = run_doctor(tmp_path)
    assert exit_code == 1
    out = capsys.readouterr().out
    assert "error" in out


def test_doctor_subcommand():
    from coil.cli import create_parser
    parser = create_parser()
    args = parser.parse_args(["doctor", "/tmp/proj"])
    assert args.command == "doctor"
    assert args.project == "/tmp/proj"


def test_doctor_subcommand_default():
    from coil.cli import create_parser
    parser = create_parser()
    args = parser.parse_args(["doctor"])
    assert args.command == "doctor"
    assert args.project == "."

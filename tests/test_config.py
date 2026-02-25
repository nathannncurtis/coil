"""Tests for the config module."""

from pathlib import Path

import pytest

from coil.config import load_config, get_build_config, generate_toml


def test_load_config_no_file(tmp_path: Path):
    assert load_config(tmp_path) is None


def test_load_config_valid(tmp_path: Path):
    (tmp_path / "coil.toml").write_text(
        '[project]\nentry = "main.py"\nname = "Test"\n'
    )
    config = load_config(tmp_path)
    assert config is not None
    assert config["project"]["entry"] == "main.py"
    assert config["project"]["name"] == "Test"


def test_load_config_invalid_toml(tmp_path: Path):
    (tmp_path / "coil.toml").write_text("this is not valid toml [[[")
    with pytest.raises(Exception):
        load_config(tmp_path)


def test_get_build_config_defaults():
    raw = {"project": {"entry": "app.py", "name": "App"}, "build": {}}
    config = get_build_config(raw)
    assert config["entry"] == "app.py"
    assert config["name"] == "App"
    assert config["mode"] == "portable"
    assert config["os"] == "windows"
    assert config["console"] is True
    assert config["secure"] is False
    assert config["output_dir"] == "./dist"


def test_get_build_config_overrides():
    raw = {
        "project": {"entry": "app.py", "name": "App"},
        "build": {
            "mode": "bundled",
            "secure": True,
            "output": {"dir": "./out", "icon": "app.ico"},
            "dependencies": {"exclude": ["numpy"], "include": ["extra"]},
        },
    }
    config = get_build_config(raw)
    assert config["mode"] == "bundled"
    assert config["secure"] is True
    assert config["output_dir"] == "./out"
    assert config["icon"] == "app.ico"
    assert config["exclude"] == ["numpy"]
    assert config["include"] == ["extra"]


def test_get_build_config_with_profile():
    raw = {
        "project": {"entry": "app.py", "name": "App"},
        "build": {"mode": "portable", "secure": False},
        "profile": {
            "release": {"mode": "portable", "secure": True, "icon": "icon.ico"},
            "dev": {"mode": "bundled", "verbose": True},
        },
    }
    config = get_build_config(raw, profile="release")
    assert config["mode"] == "portable"
    assert config["secure"] is True
    assert config["icon"] == "icon.ico"

    config = get_build_config(raw, profile="dev")
    assert config["mode"] == "bundled"
    assert config["verbose"] is True
    assert config["secure"] is False  # from [build]


def test_get_build_config_missing_profile():
    raw = {
        "project": {"entry": "app.py", "name": "App"},
        "build": {},
        "profile": {"dev": {"mode": "bundled"}},
    }
    with pytest.raises(ValueError, match="Profile 'prod' not found"):
        get_build_config(raw, profile="prod")


def test_get_build_config_missing_profile_shows_available():
    raw = {
        "project": {"entry": "app.py", "name": "App"},
        "build": {},
        "profile": {"dev": {}, "release": {}},
    }
    with pytest.raises(ValueError, match="Available profiles: dev, release"):
        get_build_config(raw, profile="staging")


def test_generate_toml_basic():
    toml = generate_toml("main.py", "MyApp")
    assert 'entry = "main.py"' in toml
    assert 'name = "MyApp"' in toml
    assert "console = true" in toml
    assert 'mode = "portable"' in toml


def test_generate_toml_gui():
    toml = generate_toml("app.py", "GUI", console=False)
    assert "console = false" in toml


def test_generate_toml_with_icon():
    toml = generate_toml("main.py", "App", icon="app.ico")
    assert 'icon = "app.ico"' in toml


def test_generate_toml_with_requirements():
    toml = generate_toml("main.py", "App", has_requirements=True)
    assert "requirements.txt" in toml


def test_generate_toml_with_pyproject():
    toml = generate_toml("main.py", "App", has_pyproject=True)
    assert "pyproject.toml" in toml


def test_generate_toml_with_python():
    toml = generate_toml("main.py", "App", python_version="3.12")
    assert 'python = "3.12"' in toml


def test_generate_toml_commented_profiles():
    toml = generate_toml("main.py", "App")
    assert "profile.dev" in toml
    assert "profile.release" in toml
    # Should be commented out
    assert "# [profile.dev]" in toml


def test_roundtrip_generate_then_load(tmp_path: Path):
    """Generate a toml, write it, load it back, and verify."""
    toml = generate_toml("main.py", "TestApp", python_version="3.12", icon="test.ico")
    (tmp_path / "coil.toml").write_text(toml)

    raw = load_config(tmp_path)
    assert raw is not None
    config = get_build_config(raw)
    assert config["entry"] == "main.py"
    assert config["name"] == "TestApp"
    assert config["python"] == "3.12"
    assert config["icon"] == "test.ico"
    assert config["mode"] == "portable"

"""Tests for the config module."""

from pathlib import Path

import pytest

from coil.config import (
    load_config,
    get_build_config,
    generate_toml,
    get_versioninfo_config,
    get_subsystem_config,
    _pad_version,
    _resolve_project_version,
)


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


def test_pad_version_short():
    assert _pad_version("1.2") == "1.2.0.0"


def test_pad_version_full():
    assert _pad_version("1.2.3.4") == "1.2.3.4"


def test_pad_version_too_long():
    assert _pad_version("1.2.3.4.5") == "1.2.3.4"


def test_pad_version_empty():
    assert _pad_version("") == "1.0.0.0"


def test_pad_version_nonnumeric():
    assert _pad_version("1.2.rc1") == "1.2.0.0"


def test_pad_version_hyphen():
    assert _pad_version("1.2-3") == "1.2.3.0"


def test_resolve_project_version_from_toml(tmp_path: Path):
    raw = {"project": {"version": "2.0.1"}}
    assert _resolve_project_version(raw, tmp_path) == "2.0.1"


def test_resolve_project_version_from_version_txt(tmp_path: Path):
    (tmp_path / "version.txt").write_text("3.4.5\n", encoding="utf-8")
    raw = {"project": {}}
    assert _resolve_project_version(raw, tmp_path) == "3.4.5"


def test_resolve_project_version_toml_wins_over_version_txt(tmp_path: Path):
    (tmp_path / "version.txt").write_text("3.4.5", encoding="utf-8")
    raw = {"project": {"version": "9.9.9"}}
    assert _resolve_project_version(raw, tmp_path) == "9.9.9"


def test_resolve_project_version_absent(tmp_path: Path):
    raw = {"project": {}}
    assert _resolve_project_version(raw, tmp_path) == ""


def test_versioninfo_no_config_defaults():
    """With no [build.versioninfo] block, everything is derived."""
    raw = {"project": {"name": "MyApp"}}
    vi = get_versioninfo_config(raw, entry_name="main", project_name="MyApp")
    assert vi["product_name"] == "MyApp"
    assert vi["file_description"] == "main"
    assert vi["internal_name"] == "main"
    assert vi["original_filename"] == "main.exe"
    assert vi["file_version"] == "1.0.0.0"
    assert vi["product_version"] == "1.0.0.0"
    assert vi["company_name"] == ""
    assert vi["legal_copyright"] == ""
    assert vi["comments"] == ""


def test_versioninfo_shared_fields_applied_to_all_entries():
    raw = {
        "project": {"name": "Suite", "version": "2.1"},
        "build": {
            "versioninfo": {
                "company_name": "Acme",
                "legal_copyright": "(c) 2026 Acme",
            }
        },
    }
    a = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    b = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    assert a["company_name"] == "Acme"
    assert b["company_name"] == "Acme"
    assert a["legal_copyright"] == "(c) 2026 Acme"
    assert b["legal_copyright"] == "(c) 2026 Acme"
    # Version padded from project.version
    assert a["file_version"] == "2.1.0.0"
    assert b["product_version"] == "2.1.0.0"


def test_versioninfo_per_entry_override():
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "product_name": "Acme Suite",
                "entries": {
                    "main": {"file_description": "Acme Main"},
                    "worker": {
                        "file_description": "Acme Worker",
                        "internal_name": "acme-worker",
                    },
                },
            }
        },
    }
    a = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    b = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    assert a["product_name"] == "Acme Suite"
    assert a["file_description"] == "Acme Main"
    assert a["internal_name"] == "main"  # default — not overridden
    assert b["product_name"] == "Acme Suite"  # shared
    assert b["file_description"] == "Acme Worker"
    assert b["internal_name"] == "acme-worker"


def test_versioninfo_comments_shared():
    """Shared [build.versioninfo].comments applies to all entries."""
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "comments": "Built by the platform team",
            }
        },
    }
    a = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    b = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    assert a["comments"] == "Built by the platform team"
    assert b["comments"] == "Built by the platform team"


def test_versioninfo_comments_per_entry_override():
    """Per-entry comments wins over shared comments."""
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "comments": "Shared comment",
                "entries": {
                    "main": {"comments": "Main-only comment"},
                },
            }
        },
    }
    a = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    b = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    assert a["comments"] == "Main-only comment"
    assert b["comments"] == "Shared comment"


def test_versioninfo_comments_default_empty():
    """When no comments configured anywhere, resolved value is empty string."""
    raw = {"project": {"name": "App"}}
    vi = get_versioninfo_config(raw, entry_name="main", project_name="App")
    assert vi["comments"] == ""


def test_versioninfo_per_entry_empty_overrides_shared_comments():
    """Per-entry comments = "" explicitly blanks out an inherited shared value."""
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "comments": "Shared comment",
                "entries": {
                    "helper": {"comments": ""},
                },
            }
        },
    }
    helper = get_versioninfo_config(raw, entry_name="helper", project_name="Suite")
    worker = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    # Explicit empty override wins.
    assert helper["comments"] == ""
    # Sibling entry with no per-entry key still inherits shared.
    assert worker["comments"] == "Shared comment"


def test_versioninfo_per_entry_empty_overrides_shared_company_name():
    """Empty-override semantics apply uniformly to company_name too."""
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "company_name": "Acme Corp",
                "entries": {
                    "helper": {"company_name": ""},
                },
            }
        },
    }
    helper = get_versioninfo_config(raw, entry_name="helper", project_name="Suite")
    worker = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    assert helper["company_name"] == ""
    assert worker["company_name"] == "Acme Corp"


def test_versioninfo_per_entry_empty_overrides_shared_legal_copyright():
    """Empty-override semantics apply uniformly to legal_copyright too."""
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "legal_copyright": "Copyright (c) 2026 Acme Corp",
                "entries": {
                    "helper": {"legal_copyright": ""},
                },
            }
        },
    }
    helper = get_versioninfo_config(raw, entry_name="helper", project_name="Suite")
    worker = get_versioninfo_config(raw, entry_name="worker", project_name="Suite")
    assert helper["legal_copyright"] == ""
    assert worker["legal_copyright"] == "Copyright (c) 2026 Acme Corp"


def test_versioninfo_version_txt_fallback(tmp_path: Path):
    (tmp_path / "version.txt").write_text("4.5.6", encoding="utf-8")
    raw = {"project": {"name": "App"}}
    vi = get_versioninfo_config(
        raw, entry_name="main", project_name="App", project_dir=tmp_path
    )
    assert vi["file_version"] == "4.5.6.0"
    assert vi["product_version"] == "4.5.6.0"


def test_versioninfo_quoted_stem_with_spaces():
    """TOML requires quoted keys for stems containing spaces."""
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "entries": {
                    "With Spaces": {"file_description": "Spaced Tool"},
                }
            }
        },
    }
    vi = get_versioninfo_config(
        raw, entry_name="With Spaces", project_name="Suite"
    )
    assert vi["file_description"] == "Spaced Tool"
    # Defaults still derive from the stem verbatim
    assert vi["internal_name"] == "With Spaces"
    assert vi["original_filename"] == "With Spaces.exe"


def test_versioninfo_quoted_stem_parses_from_toml(tmp_path: Path):
    """Roundtrip: write TOML with a quoted spaced-stem key, parse it back."""
    toml = (
        '[project]\nname = "Suite"\nentry = ["With Spaces.py"]\n'
        '[build.versioninfo.entries."With Spaces"]\n'
        'file_description = "Spaced"\n'
    )
    (tmp_path / "coil.toml").write_text(toml, encoding="utf-8")
    raw = load_config(tmp_path)
    assert raw is not None
    vi = get_versioninfo_config(
        raw, entry_name="With Spaces", project_name="Suite"
    )
    assert vi["file_description"] == "Spaced"


def test_versioninfo_entry_overrides_shared():
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "product_name": "Shared Product",
                "company_name": "Shared Co",
                "entries": {
                    "main": {
                        "product_name": "Main Only Product",
                        "company_name": "Main Only Co",
                    },
                },
            }
        },
    }
    vi = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    assert vi["product_name"] == "Main Only Product"
    assert vi["company_name"] == "Main Only Co"


def test_versioninfo_product_name_defaults_to_project_name():
    raw = {"project": {"name": "MyProject"}}
    vi = get_versioninfo_config(raw, entry_name="helper", project_name="MyProject")
    assert vi["product_name"] == "MyProject"


def test_versioninfo_product_version_defaults_to_file_version():
    raw = {
        "project": {"name": "Suite"},
        "build": {"versioninfo": {"file_version": "5.6.7.8"}},
    }
    vi = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    assert vi["file_version"] == "5.6.7.8"
    assert vi["product_version"] == "5.6.7.8"


def test_versioninfo_explicit_product_version_wins():
    raw = {
        "project": {"name": "Suite"},
        "build": {
            "versioninfo": {
                "file_version": "1.0",
                "product_version": "2.0",
            }
        },
    }
    vi = get_versioninfo_config(raw, entry_name="main", project_name="Suite")
    assert vi["file_version"] == "1.0.0.0"
    assert vi["product_version"] == "2.0.0.0"


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


def test_subsystem_absent_returns_none():
    """No [build.entries] config at all — resolver returns None."""
    raw = {"project": {"name": "X"}, "build": {}}
    assert get_subsystem_config(raw, "main") is None


def test_subsystem_entry_missing_returns_none():
    """Other entries have config but this one doesn't — returns None."""
    raw = {"build": {"entries": {"worker": {"subsystem": "console"}}}}
    assert get_subsystem_config(raw, "main") is None


def test_subsystem_explicit_console_returned():
    raw = {"build": {"entries": {"update_checker": {"subsystem": "console"}}}}
    assert get_subsystem_config(raw, "update_checker") == "console"


def test_subsystem_explicit_gui_returned():
    raw = {"build": {"entries": {"main": {"subsystem": "gui"}}}}
    assert get_subsystem_config(raw, "main") == "gui"


def test_subsystem_invalid_value_raises():
    raw = {"build": {"entries": {"helper": {"subsystem": "neither"}}}}
    with pytest.raises(ValueError) as exc:
        get_subsystem_config(raw, "helper")
    # Error message names the entry and the bad value.
    assert "helper" in str(exc.value)
    assert "neither" in str(exc.value)


def test_subsystem_empty_string_raises():
    raw = {"build": {"entries": {"helper": {"subsystem": ""}}}}
    with pytest.raises(ValueError):
        get_subsystem_config(raw, "helper")


def test_subsystem_load_from_toml(tmp_path: Path):
    """Full round-trip: write toml, load, resolve per entry."""
    (tmp_path / "coil.toml").write_text(
        '[project]\n'
        'entry = ["MyApp.py", "update_checker.py"]\n'
        'name = "MyApp"\n'
        '\n'
        '[build]\n'
        'mode = "bundled"\n'
        '\n'
        '[build.entries.update_checker]\n'
        'subsystem = "console"\n'
    )
    raw = load_config(tmp_path)
    assert raw is not None
    assert get_subsystem_config(raw, "update_checker") == "console"
    assert get_subsystem_config(raw, "MyApp") is None

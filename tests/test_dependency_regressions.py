"""Regression coverage for dependency decisions that produced broken exes."""
from pathlib import Path

import pytest

from coil.builder import _compute_deps_hash
from coil.resolver import parse_requirements_txt, resolve_dependencies
from coil.scanner import scan_project
from coil.utils.stdlib_list import get_stdlib_modules


def test_declared_markers_extras_and_pins_win_over_auto_scan(tmp_path: Path):
    (tmp_path / "main.py").write_text("import fakepkg\n")
    (tmp_path / "pyproject.toml").write_text(
        '[project]\ndependencies = ["fake-pkg[extra]==1.0; python_version >= \'3.12\'"]\n'
    )
    result = resolve_dependencies(tmp_path, "3.12.10", dist_map={"fakepkg": ["Fake_Pkg"]})
    assert result == ["fake-pkg[extra]==1.0; python_version >= '3.12'"]
    assert resolve_dependencies(tmp_path, "3.12.10", exclude=["FAKE.PKG"], dist_map={"fakepkg": ["Fake_Pkg"]}) == []


def test_nested_requirements_and_url_fragments(tmp_path: Path):
    (tmp_path / "requirements.txt").write_text("-r deps/base.txt\n")
    (tmp_path / "deps").mkdir()
    (tmp_path / "deps/base.txt").write_text(
        "package @ https://example.invalid/wheel.whl#sha256=abc # comment\n"
    )
    assert parse_requirements_txt(tmp_path / "requirements.txt") == [
        "package @ https://example.invalid/wheel.whl#sha256=abc"
    ]
    (tmp_path / "deps/base.txt").write_text("-r ../requirements.txt\n")
    with pytest.raises(ValueError, match="Recursive"):
        parse_requirements_txt(tmp_path / "requirements.txt")


def test_unsupported_pip_directive_is_never_silently_dropped(tmp_path: Path):
    path = tmp_path / "requirements.txt"
    path.write_text("-e ./package\n")
    with pytest.raises(ValueError, match="Unsupported requirements directive"):
        parse_requirements_txt(path)


def test_scanner_and_local_namespace_honor_package_exclusions(tmp_path: Path):
    (tmp_path / "main.py").write_text("from localns import helper\n")
    (tmp_path / "localns").mkdir()
    (tmp_path / "localns/helper.py").write_text("import json\n")
    for directory in ("tests", "ignored"):
        (tmp_path / directory).mkdir()
        (tmp_path / directory / "helper.py").write_text("import nonexistent_test_dependency\n")
    (tmp_path / ".coilignore").write_text("ignored/\n")
    assert scan_project(tmp_path) == {"localns", "json"}
    assert resolve_dependencies(tmp_path, "3.12.10", dist_map={}) == []


def test_pep263_source_is_scanned(tmp_path: Path):
    (tmp_path / "main.py").write_bytes(b"# coding: latin-1\n# caf\xe9\nimport requests\n")
    assert scan_project(tmp_path) == {"requests"}


def test_clean_cache_separates_target_abis_and_case_sensitive_requirements():
    assert _compute_deps_hash(["dep"], "3.12") != _compute_deps_hash(["dep"], "3.13")
    assert _compute_deps_hash(["dep"], "3.12", arch="amd64") != _compute_deps_hash(["dep"], "3.12", arch="win32")
    assert _compute_deps_hash(["dep @ https://example.invalid/A.whl"]) != _compute_deps_hash(["dep @ https://example.invalid/a.whl"])


def test_stdlib_removal_versions_and_full_patch_versions():
    assert "cgi" in get_stdlib_modules("3.12.10")
    assert "cgi" not in get_stdlib_modules("3.13.7")
    assert "tomllib" not in get_stdlib_modules("3.10")
    assert "tomllib" in get_stdlib_modules("3.11")

"""Tests for default exclude patterns, .coilignore parsing, and the
obfuscator's respect for the same matcher.

Covers:
  * _single_pattern_match behavior (file globs, dir patterns)
  * _load_coilignore (comments, blank lines, negation preserved, escapes)
  * _build_exclude_matcher (defaults, user union, negation, ancestor cascade)
  * Integration: package_bundled doesn't leak common noise into the bundle
  * Integration: obfuscator skips ignored helper scripts
"""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

import pytest

from coil.packager import (
    DEFAULT_EXCLUDE_PATTERNS,
    _build_exclude_matcher,
    _load_coilignore,
    _single_pattern_match,
    package_bundled,
)


# ---------------------------------------------------------------------------
# _single_pattern_match
# ---------------------------------------------------------------------------


def test_single_pattern_match_literal_filename():
    assert _single_pattern_match("build.bat", "build.bat", "build.bat", False)
    assert not _single_pattern_match("build.bat", "Other.bat", "Other.bat", False)


def test_single_pattern_match_glob():
    assert _single_pattern_match("*.log", "app.log", "app.log", False)
    assert _single_pattern_match("req-*.txt", "req-dev.txt", "req-dev.txt", False)
    assert not _single_pattern_match("*.log", "app.txt", "app.txt", False)


def test_single_pattern_match_dir_pattern_matches_only_dirs():
    # Directory pattern: must be a dir
    assert _single_pattern_match("Output/", "Output", "Output", True)
    assert not _single_pattern_match("Output/", "Output", "Output", False)


def test_single_pattern_match_egg_info_glob():
    assert _single_pattern_match("*.egg-info/", "pkg.egg-info", "pkg.egg-info", True)
    assert not _single_pattern_match("*.egg-info/", "egg-info", "egg-info", False)


def test_single_pattern_match_rel_path():
    # When name doesn't match but path-relative form does
    assert _single_pattern_match(
        "docs/*.md", "intro.md", "docs/intro.md", False
    )


# ---------------------------------------------------------------------------
# _load_coilignore
# ---------------------------------------------------------------------------


def test_load_coilignore_missing_file_returns_empty(tmp_path: Path):
    assert _load_coilignore(tmp_path) == []


def test_load_coilignore_basic(tmp_path: Path):
    (tmp_path / ".coilignore").write_text(
        "foo.txt\n*.bak\n\n# comment\n  spaces-trimmed  \n",
        encoding="utf-8",
    )
    assert _load_coilignore(tmp_path) == ["foo.txt", "*.bak", "spaces-trimmed"]


def test_load_coilignore_preserves_negation_prefix(tmp_path: Path):
    (tmp_path / ".coilignore").write_text("!keep.md\n", encoding="utf-8")
    assert _load_coilignore(tmp_path) == ["!keep.md"]


def test_load_coilignore_escape_bang(tmp_path: Path):
    (tmp_path / ".coilignore").write_text("\\!literal_bang.txt\n", encoding="utf-8")
    # Leading \! strips to !literal_bang.txt (the literal name)
    assert _load_coilignore(tmp_path) == ["!literal_bang.txt"]


def test_load_coilignore_escape_hash(tmp_path: Path):
    (tmp_path / ".coilignore").write_text("\\#hashed.py\n", encoding="utf-8")
    assert _load_coilignore(tmp_path) == ["#hashed.py"]


# ---------------------------------------------------------------------------
# _build_exclude_matcher — default coverage + user extensions
# ---------------------------------------------------------------------------


def _touch(root: Path, *rels: str) -> list[Path]:
    paths = []
    for rel in rels:
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        if rel.endswith("/"):
            (root / rel.rstrip("/")).mkdir(parents=True, exist_ok=True)
            paths.append(root / rel.rstrip("/"))
        else:
            p.write_text("", encoding="utf-8")
            paths.append(p)
    return paths


def test_defaults_exclude_common_leakage(tmp_path: Path):
    """The items downstream build.bat scripts delete by hand should all be
    caught by DEFAULT_EXCLUDE_PATTERNS — no .coilignore needed."""
    _touch(
        tmp_path,
        "build.bat",
        "coil.toml",
        "MyAppSetup.iss",
        "req.txt",
        "req-dev.txt",
        "requirements.txt",
        "README.md",
        "LICENSE",
        "LICENSE.txt",
        "CHANGELOG.md",
        "app.log",
        "prompt.md",
        ".gitignore",
        ".coilignore",
        "pyproject.toml",
        "setup.py",
        "Makefile",
        "Output/",
        "__pycache__/",
        "pkg.egg-info/",
        "src.egg-info/",
        ".github/",
        ".venv/",
        "build/",
        "dist/",
    )
    matches = _build_exclude_matcher(tmp_path)
    for rel in [
        "build.bat", "coil.toml", "MyAppSetup.iss", "req.txt", "req-dev.txt",
        "requirements.txt", "README.md", "LICENSE", "LICENSE.txt",
        "CHANGELOG.md", "app.log", "prompt.md", ".gitignore", ".coilignore",
        "pyproject.toml", "setup.py", "Makefile", "Output", "__pycache__",
        "pkg.egg-info", "src.egg-info", ".github", ".venv", "build", "dist",
    ]:
        assert matches(tmp_path / rel), f"expected default exclude for {rel}"


def test_defaults_allow_legitimate_assets(tmp_path: Path):
    """Regular project assets should NOT be default-excluded."""
    _touch(
        tmp_path,
        "app.ico",
        "config.json",
        "data.csv",
        "assets/logo.png",
        "main.py",
        "helpers.py",
        "mypkg/__init__.py",
    )
    matches = _build_exclude_matcher(tmp_path)
    for rel in [
        "app.ico", "config.json", "data.csv", "assets",
        "assets/logo.png", "main.py", "helpers.py", "mypkg",
        "mypkg/__init__.py",
    ]:
        assert not matches(tmp_path / rel), f"unexpected exclude: {rel}"


def test_user_negation_reincludes_default_exclude(tmp_path: Path):
    """A user who really does want README.md in the bundle can !README* in
    .coilignore to override the default."""
    _touch(tmp_path, "README.md")
    (tmp_path / ".coilignore").write_text("!README*\n", encoding="utf-8")
    matches = _build_exclude_matcher(tmp_path)
    assert not matches(tmp_path / "README.md")


def test_user_coilignore_adds_to_defaults(tmp_path: Path):
    """User can add project-specific exclusions on top of defaults."""
    _touch(tmp_path, "main.py", "set_console.py", "fix_sitecustomize.py")
    (tmp_path / ".coilignore").write_text(
        "set_console.py\nfix_sitecustomize.py\n", encoding="utf-8"
    )
    matches = _build_exclude_matcher(tmp_path)
    assert matches(tmp_path / "set_console.py")
    assert matches(tmp_path / "fix_sitecustomize.py")
    assert not matches(tmp_path / "main.py")


def test_ancestor_directory_exclusion_cascades(tmp_path: Path):
    """Files under an excluded directory are themselves excluded, even if
    they don't match any pattern directly (gitignore semantics)."""
    _touch(tmp_path, "build/internal/artifact.bin")
    matches = _build_exclude_matcher(tmp_path)
    # build/ is in defaults; the nested file inherits the exclusion
    assert matches(tmp_path / "build" / "internal" / "artifact.bin")


def test_negation_cannot_reinclude_from_excluded_dir(tmp_path: Path):
    """Git's rule: once a parent dir is excluded, you can't re-include a
    child with !pattern. We follow that."""
    _touch(tmp_path, "build/keepme.txt")
    (tmp_path / ".coilignore").write_text("!build/keepme.txt\n", encoding="utf-8")
    matches = _build_exclude_matcher(tmp_path)
    assert matches(tmp_path / "build" / "keepme.txt")


def test_defaults_exclude_tests_memory_and_ai_state(tmp_path: Path):
    """tests/, test/, memory/, .cursor/ — plus .vscode/, .idea/ as regression
    guards — should all be caught by DEFAULT_EXCLUDE_PATTERNS."""
    _touch(
        tmp_path,
        "tests/",
        "test/",
        "memory/",
        ".cursor/",
        ".vscode/",
        ".idea/",
    )
    matches = _build_exclude_matcher(tmp_path)
    for rel in ["tests", "test", "memory", ".cursor", ".vscode", ".idea"]:
        assert matches(tmp_path / rel), f"expected default exclude for {rel}"


def test_user_negation_reincludes_tests_directory(tmp_path: Path):
    """Escape hatch: a project that legitimately ships a fixture-driven
    self-test path can `!tests/` in .coilignore to re-include it — both the
    directory itself and everything under it."""
    _touch(tmp_path, "tests/fixtures/data.json")
    (tmp_path / ".coilignore").write_text("!tests/\n", encoding="utf-8")
    matches = _build_exclude_matcher(tmp_path)
    assert not matches(tmp_path / "tests")
    assert not matches(tmp_path / "tests" / "fixtures")
    assert not matches(tmp_path / "tests" / "fixtures" / "data.json")


def test_dir_pattern_doesnt_match_file_of_same_name(tmp_path: Path):
    """A pattern like `Output/` should not exclude a file literally named
    `Output` (no extension)."""
    _touch(tmp_path, "Output")  # not a dir
    matches = _build_exclude_matcher(tmp_path)
    # The file `Output` is not a directory, so `Output/` doesn't match.
    # But note: nothing else in defaults matches plain "Output" file either.
    assert not matches(tmp_path / "Output")


# ---------------------------------------------------------------------------
# Integration: package_bundled doesn't leak common noise
# ---------------------------------------------------------------------------


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only bundle build")
def test_bundled_build_does_not_leak_project_noise(tmp_path: Path, real_runtime: Path):
    """End-to-end: a project full of typical root-level noise produces a
    bundle with only the legitimate assets at root and only declared code
    under _internal/app/."""
    project = tmp_path / "proj"
    project.mkdir()

    # Legitimate code
    (project / "main.py").write_text("def run(): pass\n", encoding="utf-8")
    (project / "helpers.py").write_text("def util(): pass\n", encoding="utf-8")
    # Legitimate asset
    (project / "feather.ico").write_bytes(b"\x00\x00\x01\x00")
    (project / "config.json").write_text("{}", encoding="utf-8")

    # Noise that should be excluded by defaults
    for name in [
        "build.bat",
        "coil.toml",
        "MyAppSetup.iss",
        "req.txt",
        "requirements.txt",
        "README.md",
        "LICENSE",
        "CHANGELOG.md",
        "app.log",
        "prompt.md",
        ".gitignore",
        "pyproject.toml",
        "setup.py",
        "Makefile",
    ]:
        (project / name).write_text("noise\n", encoding="utf-8")
    for dname in [
        ".github", "__pycache__", "Output", "pkg.egg-info", ".venv", "build", "dist",
        "tests", "test", "memory", ".cursor",
    ]:
        (project / dname).mkdir()
        (project / dname / "leaked.txt").write_text("leak\n", encoding="utf-8")

    # User .coilignore adds a project-specific helper
    (project / "set_console.py").write_text("# helper\n", encoding="utf-8")
    (project / ".coilignore").write_text("set_console.py\n", encoding="utf-8")

    output = tmp_path / "dist"

    bundle = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=real_runtime,
        entry_points=["main.py"],
        name="MyApp",
        target_os="windows",
    )
    assert bundle.is_dir()

    root_files = {p.name for p in bundle.iterdir() if p.is_file()}
    root_dirs = {p.name for p in bundle.iterdir() if p.is_dir()}

    # Legitimate assets copied
    assert "feather.ico" in root_files
    assert "config.json" in root_files

    # All default-excluded noise is gone from bundle root
    for should_miss in [
        "build.bat", "coil.toml", "MyAppSetup.iss", "req.txt", "requirements.txt",
        "README.md", "LICENSE", "CHANGELOG.md", "app.log", "prompt.md",
        ".gitignore", "pyproject.toml", "setup.py", "Makefile", ".coilignore",
        "set_console.py",
    ]:
        assert should_miss not in root_files, f"{should_miss} leaked into bundle root"
    for should_miss in [
        ".github", "__pycache__", "Output", "pkg.egg-info", ".venv", "build", "dist",
        "tests", "test", "memory", ".cursor",
    ]:
        assert should_miss not in root_dirs, f"{should_miss}/ leaked into bundle root"

    # Obfuscator respects the same matcher: set_console.pyc must not be in _internal/app/
    app_dir = bundle / "_internal" / "app"
    assert app_dir.is_dir()
    compiled = {p.stem for p in app_dir.glob("*.pyc")}
    assert "main" in compiled
    assert "helpers" in compiled
    assert "set_console" not in compiled, "user-ignored helper was compiled"

    # Recovery archive also shouldn't contain the excluded helper
    from coil.obfuscator import COIL_SOURCE_ARCHIVE
    archive = app_dir / COIL_SOURCE_ARCHIVE
    if archive.is_file():
        with zipfile.ZipFile(archive, "r") as zf:
            archived = set(zf.namelist())
        assert "set_console.py" not in archived


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only bundle build")
def test_user_negation_reincludes_readme_in_bundle(tmp_path: Path, real_runtime: Path):
    """README.md is default-excluded, but `!README.md` in .coilignore re-adds it."""
    project = tmp_path / "proj"
    project.mkdir()
    (project / "main.py").write_text("\n", encoding="utf-8")
    (project / "README.md").write_text("docs\n", encoding="utf-8")
    (project / ".coilignore").write_text("!README.md\n", encoding="utf-8")

    output = tmp_path / "dist"

    bundle = package_bundled(
        project_dir=project,
        output_dir=output,
        runtime_dir=real_runtime,
        entry_points=["main.py"],
        name="App",
        target_os="windows",
    )

    assert (bundle / "README.md").is_file(), "user-negated README.md was still excluded"

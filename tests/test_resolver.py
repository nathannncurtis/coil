"""Tests for the dependency resolver."""

import textwrap
from pathlib import Path

import pytest

from coil.resolver import (
    parse_requirements_txt,
    parse_pyproject_toml,
    resolve_dependencies,
    resolve_from_imports,
)

FIXTURES = Path(__file__).parent / "fixtures" / "sample_project"


def test_parse_requirements_txt(tmp_path: Path):
    req = tmp_path / "requirements.txt"
    req.write_text(textwrap.dedent("""\
        requests>=2.28.0
        numpy==1.24.0
        # this is a comment
        flask~=2.3

        Pillow>=9.0
        -e ./local_package
    """))
    result = parse_requirements_txt(req)
    assert result == ["requests", "numpy", "flask", "Pillow"]


def test_parse_pyproject_toml(tmp_path: Path):
    toml = tmp_path / "pyproject.toml"
    toml.write_text(textwrap.dedent("""\
        [project]
        name = "myapp"
        version = "1.0.0"
        dependencies = [
            "requests>=2.28",
            "click>=8.0",
            "rich",
        ]
    """))
    result = parse_pyproject_toml(toml)
    assert result == ["requests", "click", "rich"]


def test_resolve_with_requirements_txt(tmp_path: Path):
    req = tmp_path / "requirements.txt"
    req.write_text("requests\nflask\n")
    main = tmp_path / "main.py"
    main.write_text("import os\n")

    result = resolve_dependencies(tmp_path, "3.12")
    assert result == ["flask", "requests"]


def test_resolve_with_pyproject_toml(tmp_path: Path):
    toml = tmp_path / "pyproject.toml"
    toml.write_text(textwrap.dedent("""\
        [project]
        name = "test"
        dependencies = ["click", "rich"]
    """))
    main = tmp_path / "main.py"
    main.write_text("import os\n")

    result = resolve_dependencies(tmp_path, "3.12")
    assert result == ["click", "rich"]


def test_resolve_with_ast_scan(tmp_path: Path):
    main = tmp_path / "main.py"
    main.write_text("import os\nimport requests\nimport numpy\n")

    result = resolve_dependencies(tmp_path, "3.12")
    assert "requests" in result
    assert "numpy" in result
    assert "os" not in result


def test_resolve_exclude(tmp_path: Path):
    req = tmp_path / "requirements.txt"
    req.write_text("requests\nflask\nnumpy\n")
    main = tmp_path / "main.py"
    main.write_text("")

    result = resolve_dependencies(tmp_path, "3.12", exclude=["numpy"])
    assert "numpy" not in result
    assert "requests" in result
    assert "flask" in result


def test_resolve_include(tmp_path: Path):
    req = tmp_path / "requirements.txt"
    req.write_text("requests\n")
    main = tmp_path / "main.py"
    main.write_text("")

    result = resolve_dependencies(tmp_path, "3.12", include=["extra-package"])
    assert "requests" in result
    assert "extra-package" in result


def test_resolve_explicit_requirements_path(tmp_path: Path):
    reqs = tmp_path / "custom-reqs.txt"
    reqs.write_text("pandas\nscipy\n")
    main = tmp_path / "main.py"
    main.write_text("import requests\n")

    result = resolve_dependencies(
        tmp_path, "3.12", requirements_path=str(reqs)
    )
    assert result == ["pandas", "scipy"]
    assert "requests" not in result


def test_resolve_package_name_mapping(tmp_path: Path):
    main = tmp_path / "main.py"
    main.write_text("from PIL import Image\nimport cv2\nimport bs4\n")

    # packages_distributions() returns the canonical distribution name from
    # installed metadata, which may be case-normalized (e.g. 'pillow' on newer
    # Pillow releases). pip treats distribution names case-insensitively, so
    # match case-insensitively.
    result_lower = {p.lower() for p in resolve_dependencies(tmp_path, "3.12")}
    assert "pillow" in result_lower
    assert "opencv-python" in result_lower
    assert "beautifulsoup4" in result_lower


def test_resolve_ignores_local_modules(tmp_path: Path):
    main = tmp_path / "main.py"
    main.write_text("import myutil\nimport requests\n")
    util = tmp_path / "myutil.py"
    util.write_text("x = 1\n")

    result = resolve_dependencies(tmp_path, "3.12")
    assert "requests" in result
    assert "myutil" not in result


# ---------------------------------------------------------------------------
# resolve_from_imports — driven by a fake dist_map so tests don't depend on
# what happens to be installed in the CI env.
# ---------------------------------------------------------------------------


FAKE_DIST_MAP: dict[str, list[str]] = {
    "psutil": ["psutil"],
    "pypdf": ["pypdf"],
    "PIL": ["Pillow"],
    "PyQt5": ["PyQt5"],
    "windows_toasts": ["windows-toasts"],
    "win32api": ["pywin32"],
    "win32com": ["pywin32"],
    "win32file": ["pywin32"],
    "win32pipe": ["pywin32"],
    "pywintypes": ["pywin32"],
    "requests": ["requests"],
}


def test_resolve_from_imports_module_equals_distribution(tmp_path: Path):
    (tmp_path / "app.py").write_text("import psutil\nimport pypdf\n")
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["psutil", "pypdf"]


def test_resolve_from_imports_from_dotted_resolves_top_level(tmp_path: Path):
    (tmp_path / "app.py").write_text("from PyQt5.QtWidgets import QApplication\n")
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["PyQt5"]


def test_resolve_from_imports_hyphen_underscore_distribution(tmp_path: Path):
    (tmp_path / "app.py").write_text("from windows_toasts import InteractableWindowsToaster\n")
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["windows-toasts"]


def test_resolve_from_imports_distribution_differs_from_module(tmp_path: Path):
    (tmp_path / "app.py").write_text("from PIL import Image\n")
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["Pillow"]


def test_resolve_from_imports_multi_top_level_dedupes(tmp_path: Path):
    (tmp_path / "app.py").write_text(
        "import win32api\n"
        "import win32file\n"
        "from win32com.client import Dispatch\n"
        "import pywintypes\n"
    )
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["pywin32"]


def test_resolve_from_imports_skips_stdlib(tmp_path: Path):
    (tmp_path / "app.py").write_text(
        "import os\n"
        "import sys\n"
        "import pathlib\n"
        "import importlib\n"
    )
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == []


def test_resolve_from_imports_dynamic_literal_resolves(tmp_path: Path):
    # The explicit bridge that retires the downstream
    # `win32pipe = importlib.import_module('win32pipe')` workaround.
    (tmp_path / "app.py").write_text(
        "import importlib\n"
        "win32pipe = importlib.import_module('win32pipe')\n"
    )
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["pywin32"]


def test_resolve_from_imports_dunder_import_literal_resolves(tmp_path: Path):
    (tmp_path / "app.py").write_text('mod = __import__("win32pipe")\n')
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == ["pywin32"]


def test_resolve_from_imports_dynamic_non_literal_skipped(tmp_path: Path):
    # Non-literal arg — can't be resolved statically, must be skipped silently.
    (tmp_path / "app.py").write_text(
        "import importlib\n"
        "name = some_runtime_value()\n"
        "mod = importlib.import_module(name)\n"
    )
    result = resolve_from_imports(tmp_path, "3.12", dist_map=FAKE_DIST_MAP)
    assert result == []


def test_resolve_from_imports_multi_dist_picks_first_and_warns(tmp_path: Path):
    (tmp_path / "app.py").write_text("import shared\n")
    dist_map = {"shared": ["alpha-pkg", "zeta-pkg"]}
    warnings: list[str] = []
    result = resolve_from_imports(
        tmp_path, "3.12", dist_map=dist_map, warn=warnings.append
    )
    assert result == ["alpha-pkg"]
    assert len(warnings) == 1
    assert "shared" in warnings[0]
    assert "alpha-pkg" in warnings[0]
    assert "zeta-pkg" in warnings[0]


def test_resolve_from_imports_fallback_to_hand_map(tmp_path: Path):
    # Empty dist_map simulates the fresh-clone case where the dep isn't yet
    # installed in Coil's own env. IMPORT_TO_PYPI should kick in.
    (tmp_path / "app.py").write_text("from PIL import Image\nimport win32file\n")
    result = resolve_from_imports(tmp_path, "3.12", dist_map={})
    assert "Pillow" in result
    assert "pywin32" in result
    # win32file should NOT leak through as a standalone distribution — that's
    # the false-positive bug this whole change is about.
    assert "win32file" not in result


def test_resolve_dependencies_auto_unions_with_project_dependencies(tmp_path: Path):
    # Additive semantics: [project].dependencies keeps its version pins AND
    # auto-detected imports are added on top.
    (tmp_path / "pyproject.toml").write_text(textwrap.dedent("""\
        [project]
        name = "app"
        dependencies = ["pinned-lib>=1.0"]
    """))
    (tmp_path / "app.py").write_text("import psutil\n")
    result = resolve_dependencies(
        tmp_path, "3.12", auto=True, dist_map=FAKE_DIST_MAP
    )
    assert "pinned-lib" in result
    assert "psutil" in result


def test_resolve_dependencies_auto_false_skips_import_scan(tmp_path: Path):
    (tmp_path / "pyproject.toml").write_text(textwrap.dedent("""\
        [project]
        name = "app"
        dependencies = ["pinned-lib"]
    """))
    (tmp_path / "app.py").write_text("import psutil\n")
    result = resolve_dependencies(
        tmp_path, "3.12", auto=False, dist_map=FAKE_DIST_MAP
    )
    assert result == ["pinned-lib"]


def test_resolve_dependencies_include_unions_with_auto(tmp_path: Path):
    (tmp_path / "app.py").write_text("import psutil\n")
    result = resolve_dependencies(
        tmp_path,
        "3.12",
        auto=True,
        dist_map=FAKE_DIST_MAP,
        include=["extra-manual"],
    )
    assert "psutil" in result
    assert "extra-manual" in result


def test_resolve_dependencies_exclude_applies_last(tmp_path: Path):
    (tmp_path / "pyproject.toml").write_text(textwrap.dedent("""\
        [project]
        name = "app"
        dependencies = ["pinned-lib"]
    """))
    (tmp_path / "app.py").write_text("import psutil\n")
    result = resolve_dependencies(
        tmp_path,
        "3.12",
        auto=True,
        dist_map=FAKE_DIST_MAP,
        include=["extra-manual"],
        exclude=["psutil", "extra-manual"],
    )
    assert "pinned-lib" in result
    assert "psutil" not in result
    assert "extra-manual" not in result


def test_resolve_dependencies_warn_invoked_on_multi_dist(tmp_path: Path):
    (tmp_path / "app.py").write_text("import shared\n")
    warnings: list[str] = []
    resolve_dependencies(
        tmp_path,
        "3.12",
        auto=True,
        dist_map={"shared": ["alpha-pkg", "zeta-pkg"]},
        warn=warnings.append,
    )
    assert any("shared" in w for w in warnings)


# ---------------------------------------------------------------------------
# Integration: run against the real env's packages_distributions(), but skip
# any assertion whose package isn't installed so CI stays green.
# ---------------------------------------------------------------------------


def _has_distribution(name: str) -> bool:
    import importlib.metadata
    try:
        importlib.metadata.distribution(name)
        return True
    except importlib.metadata.PackageNotFoundError:
        return False


def test_resolve_from_imports_integration_real_env(tmp_path: Path):
    (tmp_path / "app.py").write_text(textwrap.dedent("""\
        from PIL import Image
        from PyQt5.QtWidgets import QApplication
        from windows_toasts import InteractableWindowsToaster
        import win32api
        import win32file
        from win32com.client import Dispatch
        import pywintypes
    """))

    result = resolve_from_imports(tmp_path, "3.12")
    result_lower = {p.lower() for p in result}

    # pip treats distribution names case-insensitively, so assert on lowercase.
    expectations = ("pillow", "pyqt5", "windows-toasts", "pywin32")
    checked = False
    for dist in expectations:
        if not _has_distribution(dist):
            continue
        checked = True
        assert dist in result_lower, (
            f"expected {dist!r} in resolver output, got {result!r}"
        )

    if not checked:
        pytest.skip(
            "none of Pillow/PyQt5/windows-toasts/pywin32 installed; skipping"
        )

    # Regardless of what's installed, the false-positive bug must stay fixed:
    # win32file/win32pipe/win32api must never appear as standalone distributions.
    for false_positive in ("win32file", "win32pipe", "win32api", "pywintypes"):
        assert false_positive not in result_lower

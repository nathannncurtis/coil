"""Tests for the dependency resolver."""

import textwrap
from pathlib import Path

from coil.resolver import (
    parse_requirements_txt,
    parse_pyproject_toml,
    resolve_dependencies,
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

    result = resolve_dependencies(tmp_path, "3.12")
    assert "Pillow" in result
    assert "opencv-python" in result
    assert "beautifulsoup4" in result


def test_resolve_ignores_local_modules(tmp_path: Path):
    main = tmp_path / "main.py"
    main.write_text("import myutil\nimport requests\n")
    util = tmp_path / "myutil.py"
    util.write_text("x = 1\n")

    result = resolve_dependencies(tmp_path, "3.12")
    assert "requests" in result
    assert "myutil" not in result

"""Tests for obfuscation and decompilation."""

from pathlib import Path

from coil.obfuscator import (
    compile_to_pyc,
    compile_directory,
    obfuscate_default,
    obfuscate_secure,
    COIL_METADATA_FILENAME,
    COIL_SOURCE_ARCHIVE,
)
from coil.decompiler import decompile


def _make_sample_project(base: Path) -> Path:
    """Create a sample project for testing."""
    project = base / "project"
    project.mkdir()
    (project / "main.py").write_text('print("hello")\n')
    (project / "utils.py").write_text('def add(a, b):\n    return a + b\n')
    sub = project / "sub"
    sub.mkdir()
    (sub / "__init__.py").write_text("")
    (sub / "module.py").write_text("x = 42\n")
    return project


def test_compile_to_pyc(tmp_path: Path):
    source = tmp_path / "test.py"
    source.write_text('x = 1\n')
    output = tmp_path / "test.pyc"

    compile_to_pyc(source, output)
    assert output.is_file()
    assert output.stat().st_size > 0


def test_compile_directory(tmp_path: Path):
    project = _make_sample_project(tmp_path)
    out = tmp_path / "compiled"

    result = compile_directory(project, out)
    assert len(result) >= 3
    assert all(p.suffix == ".pyc" for p in result)
    assert (out / "main.pyc").is_file()
    assert (out / "utils.pyc").is_file()
    assert (out / "sub" / "module.pyc").is_file()


def test_obfuscate_default(tmp_path: Path):
    project = _make_sample_project(tmp_path)
    out = tmp_path / "build"

    app_dir = obfuscate_default(project, out)

    assert (app_dir / COIL_METADATA_FILENAME).is_file()
    assert (app_dir / COIL_SOURCE_ARCHIVE).is_file()
    assert (app_dir / "main.pyc").is_file()

    import json
    meta = json.loads((app_dir / COIL_METADATA_FILENAME).read_text())
    assert meta["secure"] is False
    assert "source_archive" in meta


def test_obfuscate_secure(tmp_path: Path):
    project = _make_sample_project(tmp_path)
    out = tmp_path / "build"

    app_dir = obfuscate_secure(project, out)

    assert (app_dir / COIL_METADATA_FILENAME).is_file()
    assert not (app_dir / COIL_SOURCE_ARCHIVE).exists()
    assert (app_dir / "main.pyc").is_file()

    import json
    meta = json.loads((app_dir / COIL_METADATA_FILENAME).read_text())
    assert meta["secure"] is True


def test_decompile_default_build(tmp_path: Path):
    project = _make_sample_project(tmp_path)
    build_dir = tmp_path / "build"
    obfuscate_default(project, build_dir)

    recovered = tmp_path / "recovered"
    result = decompile(build_dir, recovered)

    assert result is True
    assert (recovered / "main.py").is_file()
    assert (recovered / "utils.py").is_file()
    assert (recovered / "sub" / "module.py").is_file()

    # Verify content is identical
    assert (recovered / "main.py").read_text() == 'print("hello")\n'
    assert (recovered / "utils.py").read_text() == 'def add(a, b):\n    return a + b\n'


def test_decompile_secure_build_refused(tmp_path: Path, capsys):
    project = _make_sample_project(tmp_path)
    build_dir = tmp_path / "build"
    obfuscate_secure(project, build_dir)

    recovered = tmp_path / "recovered"
    result = decompile(build_dir, recovered)

    assert result is False
    captured = capsys.readouterr()
    assert "--secure" in captured.out
    assert "cannot be decompiled" in captured.out


def test_decompile_not_coil_build(tmp_path: Path, capsys):
    fake_dir = tmp_path / "not_coil"
    fake_dir.mkdir()
    (fake_dir / "random.exe").write_bytes(b"not a coil build")

    recovered = tmp_path / "recovered"
    result = decompile(fake_dir / "random.exe", recovered)

    assert result is False
    captured = capsys.readouterr()
    assert "not appear to be a Coil-built" in captured.out

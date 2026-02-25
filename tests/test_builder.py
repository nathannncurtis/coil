"""Tests for the build orchestration."""

from pathlib import Path
from unittest.mock import patch, MagicMock

from coil.builder import build, _log


def _make_project(base: Path) -> Path:
    """Create a sample project for testing."""
    project = base / "testproject"
    project.mkdir()
    (project / "main.py").write_text('print("hello world")\n')
    return project


def test_log_verbose(capsys):
    _log("test message", verbose=True)
    assert "test message" in capsys.readouterr().out


def test_log_not_verbose(capsys):
    _log("test message", verbose=False)
    assert capsys.readouterr().out == ""


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_bundled")
@patch("coil.builder.resolve_dependencies")
def test_build_bundled_no_deps(
    mock_resolve, mock_bundle, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = []
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_bundle.return_value = tmp_path / "dist" / "testproject"

    result = build(
        project_dir=project,
        entry_points=["main.py"],
        mode="bundled",
        target_os="windows",
        python_version="3.12",
        output_dir=str(tmp_path / "dist"),
    )

    mock_resolve.assert_called_once()
    mock_runtime.assert_called_once()
    mock_install.assert_not_called()
    mock_bundle.assert_called_once()
    assert len(result) == 1


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_portable")
@patch("coil.builder.resolve_dependencies")
def test_build_portable_with_deps(
    mock_resolve, mock_portable, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = ["requests", "flask"]
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_install.return_value = tmp_path / "lib"
    mock_portable.return_value = [tmp_path / "dist" / "testproject.exe"]

    result = build(
        project_dir=project,
        entry_points=["main.py"],
        mode="portable",
        target_os="windows",
        python_version="3.12",
        output_dir=str(tmp_path / "dist"),
    )

    mock_resolve.assert_called_once()
    mock_runtime.assert_called_once()
    mock_install.assert_called_once()
    mock_portable.assert_called_once()
    assert len(result) == 1


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_bundled")
@patch("coil.builder.resolve_dependencies")
def test_build_passes_secure_flag(
    mock_resolve, mock_bundle, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = []
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_bundle.return_value = tmp_path / "dist" / "testproject"

    build(
        project_dir=project,
        entry_points=["main.py"],
        mode="bundled",
        target_os="windows",
        python_version="3.12",
        secure=True,
        output_dir=str(tmp_path / "dist"),
    )

    call_kwargs = mock_bundle.call_args[1]
    assert call_kwargs["secure"] is True


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_bundled")
@patch("coil.builder.resolve_dependencies")
def test_build_passes_gui_flag(
    mock_resolve, mock_bundle, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = []
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_bundle.return_value = tmp_path / "dist" / "testproject"

    build(
        project_dir=project,
        entry_points=["main.py"],
        mode="bundled",
        target_os="windows",
        python_version="3.12",
        gui=True,
        output_dir=str(tmp_path / "dist"),
    )

    call_kwargs = mock_bundle.call_args[1]
    assert call_kwargs["gui"] is True

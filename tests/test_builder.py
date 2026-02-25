"""Tests for the build orchestration."""

from pathlib import Path
from unittest.mock import patch, MagicMock

from coil.builder import build, _log, _compute_deps_hash, _get_clean_env_dir


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


def test_compute_deps_hash_deterministic():
    h1 = _compute_deps_hash(["requests", "flask"])
    h2 = _compute_deps_hash(["flask", "requests"])
    assert h1 == h2  # order shouldn't matter


def test_compute_deps_hash_case_insensitive():
    h1 = _compute_deps_hash(["Requests", "Flask"])
    h2 = _compute_deps_hash(["requests", "flask"])
    assert h1 == h2


def test_compute_deps_hash_different():
    h1 = _compute_deps_hash(["requests"])
    h2 = _compute_deps_hash(["flask"])
    assert h1 != h2


def test_get_clean_env_dir():
    d = _get_clean_env_dir("abc123")
    assert "_clean_envs" in str(d)
    assert "abc123" in str(d)


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_portable")
@patch("coil.builder.resolve_dependencies")
def test_build_clean_creates_cached_env(
    mock_resolve, mock_portable, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = ["requests", "flask"]
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_install.return_value = tmp_path / "lib"
    mock_portable.return_value = [tmp_path / "dist" / "testproject.exe"]

    # Patch cache dir to use tmp_path
    with patch("coil.builder.get_cache_dir", return_value=tmp_path / "cache"):
        build(
            project_dir=project,
            entry_points=["main.py"],
            mode="portable",
            target_os="windows",
            python_version="3.12",
            output_dir=str(tmp_path / "dist"),
            clean=True,
        )

    # install_dependencies should have been called with the cached clean dir
    mock_install.assert_called_once()
    call_kwargs = mock_install.call_args[1]
    assert "_clean_envs" in str(call_kwargs["dest_dir"])

    # Marker file should exist
    deps_hash = _compute_deps_hash(["requests", "flask"])
    marker = tmp_path / "cache" / "_clean_envs" / deps_hash / ".coil_ready"
    assert marker.is_file()


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_portable")
@patch("coil.builder.resolve_dependencies")
def test_build_clean_reuses_cached_env(
    mock_resolve, mock_portable, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = ["requests", "flask"]
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_portable.return_value = [tmp_path / "dist" / "testproject.exe"]

    # Pre-create the cached clean env
    deps_hash = _compute_deps_hash(["requests", "flask"])
    clean_dir = tmp_path / "cache" / "_clean_envs" / deps_hash
    clean_dir.mkdir(parents=True)
    (clean_dir / ".coil_ready").write_text(deps_hash, encoding="utf-8")

    with patch("coil.builder.get_cache_dir", return_value=tmp_path / "cache"):
        build(
            project_dir=project,
            entry_points=["main.py"],
            mode="portable",
            target_os="windows",
            python_version="3.12",
            output_dir=str(tmp_path / "dist"),
            clean=True,
        )

    # install_dependencies should NOT have been called (cache hit)
    mock_install.assert_not_called()


@patch("coil.builder.prepare_runtime")
@patch("coil.builder.install_dependencies")
@patch("coil.builder.package_portable")
@patch("coil.builder.resolve_dependencies")
def test_build_no_clean_uses_temp_dir(
    mock_resolve, mock_portable, mock_install, mock_runtime, tmp_path: Path
):
    project = _make_project(tmp_path)
    mock_resolve.return_value = ["requests"]
    mock_runtime.return_value = tmp_path / "runtime"
    (tmp_path / "runtime").mkdir()
    mock_install.return_value = tmp_path / "lib"
    mock_portable.return_value = [tmp_path / "dist" / "testproject.exe"]

    build(
        project_dir=project,
        entry_points=["main.py"],
        mode="portable",
        target_os="windows",
        python_version="3.12",
        output_dir=str(tmp_path / "dist"),
        clean=False,
    )

    # install_dependencies should be called with a temp dir (not _clean_envs)
    mock_install.assert_called_once()
    call_kwargs = mock_install.call_args[1]
    assert "_clean_envs" not in str(call_kwargs["dest_dir"])

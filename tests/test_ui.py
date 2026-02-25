"""Tests for the build UI."""

from io import StringIO
from pathlib import Path

from rich.console import Console

from coil.ui import BuildUI, format_size


def _make_ui(verbose: bool = False) -> tuple[BuildUI, StringIO]:
    """Create a BuildUI with captured output."""
    buf = StringIO()
    console = Console(file=buf, force_terminal=False, highlight=False, width=120)
    ui = BuildUI(verbose=verbose, console=console)
    return ui, buf


def test_step_always_visible():
    ui, buf = _make_ui(verbose=False)
    ui.step("Resolving dependencies...")
    output = buf.getvalue()
    assert "Resolving dependencies..." in output


def test_detail_hidden_when_not_verbose():
    ui, buf = _make_ui(verbose=False)
    ui.detail("Found 3 dependencies")
    output = buf.getvalue()
    assert output == ""


def test_detail_visible_when_verbose():
    ui, buf = _make_ui(verbose=True)
    ui.detail("Found 3 dependencies")
    output = buf.getvalue()
    assert "Found 3 dependencies" in output


def test_success_always_visible():
    ui, buf = _make_ui(verbose=False)
    ui.success("Build complete")
    output = buf.getvalue()
    assert "Build complete" in output


def test_warning_always_visible():
    ui, buf = _make_ui(verbose=False)
    ui.warning("Could not apply icon")
    output = buf.getvalue()
    assert "Could not apply icon" in output
    assert "warning:" in output


def test_build_header():
    ui, buf = _make_ui()
    ui.build_header("MyApp", "portable")
    output = buf.getvalue()
    assert "Compiling" in output
    assert "MyApp" in output
    assert "portable" in output


def test_build_summary_file(tmp_path: Path):
    ui, buf = _make_ui()
    exe = tmp_path / "MyApp.exe"
    exe.write_bytes(b"\x00" * 5000)

    ui.build_summary([exe])
    output = buf.getvalue()
    assert "Finished" in output
    assert "MyApp.exe" in output
    assert "4.9 KB" in output


def test_build_summary_directory(tmp_path: Path):
    ui, buf = _make_ui()
    app_dir = tmp_path / "MyApp"
    app_dir.mkdir()
    (app_dir / "a.txt").write_bytes(b"\x00" * 2000)
    (app_dir / "b.txt").write_bytes(b"\x00" * 3000)

    ui.build_summary([app_dir])
    output = buf.getvalue()
    assert "Finished" in output
    assert "MyApp" in output
    assert "4.9 KB" in output


def test_format_size_bytes():
    assert format_size(500) == "500 B"


def test_format_size_kb():
    assert format_size(2048) == "2.0 KB"


def test_format_size_mb():
    assert format_size(5 * 1024 * 1024) == "5.0 MB"


def test_format_size_gb():
    assert format_size(2 * 1024 * 1024 * 1024) == "2.0 GB"


def test_file_progress_returns_context_manager():
    ui, _ = _make_ui()
    progress = ui.file_progress("Compiling", total=10)
    # Should be usable as a context manager
    with progress:
        task = progress.add_task("", total=10)
        for _ in range(10):
            progress.advance(task)


def test_download_progress_returns_context_manager():
    ui, _ = _make_ui()
    progress = ui.download_progress()
    with progress:
        task = progress.add_task("download", total=100)
        progress.update(task, completed=100)


def test_spinner_returns_context_manager():
    ui, _ = _make_ui()
    spinner = ui.spinner("Working...")
    with spinner:
        pass


def test_make_download_hook():
    ui, _ = _make_ui()
    with ui.download_progress() as progress:
        task = progress.add_task("download", total=None)
        hook = ui.make_download_hook(progress, task)
        # Simulate download: first block sets total
        hook(0, 8192, 100000)
        hook(1, 8192, 100000)
        hook(12, 8192, 100000)


def test_build_summary_dir_breakdown_verbose(tmp_path: Path):
    """Verbose build summary shows component breakdown for bundled builds."""
    ui, buf = _make_ui(verbose=True)
    app_dir = tmp_path / "MyApp"
    internal = app_dir / "_internal"
    (internal / "app").mkdir(parents=True)
    (internal / "lib").mkdir()

    # Simulate runtime files
    (internal / "python313.zip").write_bytes(b"\x00" * 10000)
    (internal / "site.pyd").write_bytes(b"\x00" * 5000)

    # Simulate app code
    (internal / "app" / "main.pyc").write_bytes(b"\x00" * 1000)

    # Simulate deps
    (internal / "lib" / "requests.pyc").write_bytes(b"\x00" * 3000)

    # Root-level exe
    (app_dir / "MyApp.exe").write_bytes(b"\x00" * 2000)

    ui.build_summary([app_dir])
    output = buf.getvalue()
    assert "runtime:" in output
    assert "app:" in output
    assert "deps:" in output


def test_build_summary_dir_no_breakdown_non_verbose(tmp_path: Path):
    """Non-verbose build summary does NOT show breakdown."""
    ui, buf = _make_ui(verbose=False)
    app_dir = tmp_path / "MyApp"
    internal = app_dir / "_internal"
    (internal / "app").mkdir(parents=True)
    (internal / "app" / "main.pyc").write_bytes(b"\x00" * 1000)
    (app_dir / "MyApp.exe").write_bytes(b"\x00" * 2000)

    ui.build_summary([app_dir])
    output = buf.getvalue()
    assert "Finished" in output
    assert "runtime:" not in output

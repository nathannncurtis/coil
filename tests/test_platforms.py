"""Tests for platform handlers."""

import pytest
from pathlib import Path

from coil.platforms import get_handler
from coil.platforms.windows import (
    WindowsHandler,
    _generate_launcher_script,
    _generate_bat_launcher,
    _generate_vbs_launcher,
    set_pe_subsystem,
)
from coil.platforms.macos import MacOSHandler
from coil.platforms.linux import LinuxHandler


def test_get_handler_windows():
    handler = get_handler("windows")
    assert isinstance(handler, WindowsHandler)


def test_get_handler_macos():
    handler = get_handler("macos")
    assert isinstance(handler, MacOSHandler)


def test_get_handler_linux():
    handler = get_handler("linux")
    assert isinstance(handler, LinuxHandler)


def test_get_handler_invalid():
    with pytest.raises(ValueError, match="Unsupported"):
        get_handler("freebsd")


def test_windows_executable_extension():
    handler = WindowsHandler()
    assert handler.get_executable_extension() == ".exe"


def test_windows_runtime_arch():
    handler = WindowsHandler()
    arch = handler.get_runtime_arch()
    assert arch in ("amd64", "win32")


def test_macos_not_implemented():
    handler = MacOSHandler()
    with pytest.raises(NotImplementedError, match="macOS"):
        handler.create_launcher(Path("."), "main.pyc", "test")
    with pytest.raises(NotImplementedError, match="macOS"):
        handler.get_runtime_arch()
    with pytest.raises(NotImplementedError, match="macOS"):
        handler.get_executable_extension()


def test_linux_not_implemented():
    handler = LinuxHandler()
    with pytest.raises(NotImplementedError, match="Linux"):
        handler.create_launcher(Path("."), "main.pyc", "test")
    with pytest.raises(NotImplementedError, match="Linux"):
        handler.get_runtime_arch()
    with pytest.raises(NotImplementedError, match="Linux"):
        handler.get_executable_extension()


def test_generate_launcher_script():
    script = _generate_launcher_script("main.pyc", gui=False)
    assert "main.pyc" in script
    assert "sys.path" in script
    assert "__main__" in script


def test_generate_bat_launcher_console():
    bat = _generate_bat_launcher("python.exe", "_launcher.py", gui=False)
    assert "python.exe" in bat
    assert "_launcher.py" in bat
    assert "start" not in bat


def test_generate_bat_launcher_gui():
    bat = _generate_bat_launcher("python.exe", "_launcher.py", gui=True)
    assert "start" in bat
    assert "/B" in bat


def test_generate_vbs_launcher():
    vbs = _generate_vbs_launcher("app.bat")
    assert "app.bat" in vbs
    assert "WScript.Shell" in vbs
    assert ", 0," in vbs


def test_create_launcher(tmp_path: Path):
    handler = WindowsHandler()
    # Create a fake python.exe so the launcher can find it
    (tmp_path / "python.exe").write_text("fake")

    result = handler.create_launcher(
        output_dir=tmp_path,
        entry_point="main.pyc",
        name="TestApp",
        gui=False,
    )
    assert result.name == "TestApp.bat"
    assert result.is_file()
    assert (tmp_path / "_launcher_TestApp.py").is_file()


def test_create_launcher_gui(tmp_path: Path):
    handler = WindowsHandler()
    (tmp_path / "python.exe").write_text("fake")

    result = handler.create_launcher(
        output_dir=tmp_path,
        entry_point="main.pyc",
        name="GuiApp",
        gui=True,
    )
    assert result.is_file()
    assert (tmp_path / "GuiApp.vbs").is_file()

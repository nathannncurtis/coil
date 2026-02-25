"""Tests for platform handlers."""

import struct
import sys
import pytest
from pathlib import Path
from unittest.mock import patch, MagicMock

from coil.platforms import get_handler
from coil.platforms.windows import (
    WindowsHandler,
    _generate_launcher_script,
    _generate_bat_launcher,
    _generate_vbs_launcher,
    set_pe_subsystem,
    set_exe_icon,
)
from coil.platforms.macos import MacOSHandler
from coil.platforms.linux import LinuxHandler


def _make_ico(path: Path, count: int = 1) -> Path:
    """Create a minimal valid .ico file for testing."""
    # BMP info header for a 1x1 32-bit icon
    bmp_header = struct.pack(
        "<IiiHHIIiiII",
        40, 1, 2, 1, 32, 0, 0, 0, 0, 0, 0,
    )
    pixel_data = b"\xff\x00\x00\xff"  # 1 BGRA pixel
    and_mask = b"\x00\x00\x00\x00"
    image_data = bmp_header + pixel_data + and_mask

    # ICONDIR header
    ico = struct.pack("<HHH", 0, 1, count)

    # ICONDIRENTRY for each image (all point to same data for simplicity)
    data_offset = 6 + 16 * count
    for _ in range(count):
        ico += struct.pack(
            "<BBBBHHII",
            1, 1, 0, 0, 1, 32, len(image_data), data_offset,
        )

    # Image data
    for _ in range(count):
        ico += image_data

    path.write_bytes(ico)
    return path


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


def test_set_exe_icon_missing_file(tmp_path: Path):
    exe = tmp_path / "test.exe"
    exe.write_bytes(b"MZ" + b"\x00" * 200)
    with pytest.raises(FileNotFoundError):
        set_exe_icon(exe, tmp_path / "nonexistent.ico")


def test_set_exe_icon_invalid_ico(tmp_path: Path):
    exe = tmp_path / "test.exe"
    exe.write_bytes(b"MZ" + b"\x00" * 200)
    bad_ico = tmp_path / "bad.ico"
    bad_ico.write_bytes(b"\x00\x00\x05\x00\x01\x00")  # type=5, not 1
    with pytest.raises(ValueError, match="Not a valid .ico"):
        set_exe_icon(exe, bad_ico)


def test_set_exe_icon_truncated_ico(tmp_path: Path):
    exe = tmp_path / "test.exe"
    exe.write_bytes(b"MZ" + b"\x00" * 200)
    tiny_ico = tmp_path / "tiny.ico"
    tiny_ico.write_bytes(b"\x00\x00")
    with pytest.raises(ValueError, match="too small"):
        set_exe_icon(exe, tiny_ico)


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only API")
def test_set_exe_icon_calls_windows_api(tmp_path: Path):
    """Test that set_exe_icon calls the Windows UpdateResource API."""
    import ctypes

    ico = _make_ico(tmp_path / "app.ico")
    exe = tmp_path / "test.exe"
    exe.write_bytes(b"MZ" + b"\x00" * 200)

    mock_kernel = MagicMock()
    mock_kernel.BeginUpdateResourceW.return_value = 12345
    mock_kernel.UpdateResourceW.return_value = True
    mock_kernel.EndUpdateResourceW.return_value = True

    with patch.object(ctypes.windll, "kernel32", mock_kernel):
        set_exe_icon(exe, ico)

    mock_kernel.BeginUpdateResourceW.assert_called_once()
    # 1 RT_ICON call + 1 RT_GROUP_ICON call
    assert mock_kernel.UpdateResourceW.call_count == 2
    mock_kernel.EndUpdateResourceW.assert_called_once_with(12345, False)


@pytest.mark.skipif(sys.platform != "win32", reason="Windows-only API")
def test_set_exe_icon_multi_image(tmp_path: Path):
    """Test icon embedding with multiple images in one .ico."""
    import ctypes

    ico = _make_ico(tmp_path / "multi.ico", count=3)
    exe = tmp_path / "test.exe"
    exe.write_bytes(b"MZ" + b"\x00" * 200)

    mock_kernel = MagicMock()
    mock_kernel.BeginUpdateResourceW.return_value = 99
    mock_kernel.UpdateResourceW.return_value = True
    mock_kernel.EndUpdateResourceW.return_value = True

    with patch.object(ctypes.windll, "kernel32", mock_kernel):
        set_exe_icon(exe, ico)

    # 3 RT_ICON calls + 1 RT_GROUP_ICON call
    assert mock_kernel.UpdateResourceW.call_count == 4

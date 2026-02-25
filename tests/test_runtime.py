"""Tests for the embedded Python runtime management."""

import zipfile
from pathlib import Path
from unittest.mock import patch

from coil.runtime import (
    configure_pth,
    extract_runtime,
    get_cache_path,
    get_cached_zip_path,
    get_embed_url,
)


def test_get_embed_url():
    url = get_embed_url("3.12.1")
    assert url == "https://www.python.org/ftp/python/3.12.1/python-3.12.1-embed-amd64.zip"


def test_get_embed_url_win32():
    url = get_embed_url("3.11.5", arch="win32")
    assert url == "https://www.python.org/ftp/python/3.11.5/python-3.11.5-embed-win32.zip"


def test_get_cache_path():
    path = get_cache_path("3.12.1")
    assert "python-3.12.1-amd64" in str(path)


def test_get_cached_zip_path():
    path = get_cached_zip_path("3.12.1")
    assert path.name == "python-3.12.1-embed-amd64.zip"


def test_extract_runtime(tmp_path: Path):
    # Create a fake embeddable zip
    zip_path = tmp_path / "python-3.12.1-embed-amd64.zip"
    dest = tmp_path / "runtime"

    with zipfile.ZipFile(zip_path, "w") as zf:
        zf.writestr("python.exe", "fake exe")
        zf.writestr("python312.dll", "fake dll")
        zf.writestr("python312._pth", "python312.zip\n.\n#import site\n")

    result = extract_runtime(zip_path, dest)
    assert result == dest
    assert (dest / "python.exe").is_file()
    assert (dest / "python312.dll").is_file()
    assert (dest / "python312._pth").is_file()


def test_configure_pth(tmp_path: Path):
    pth = tmp_path / "python312._pth"
    pth.write_text("python312.zip\n.\n#import site\n")

    configure_pth(tmp_path, extra_paths=["app", "lib"])

    content = pth.read_text()
    assert "import site" in content
    assert "#import site" not in content
    assert "app" in content
    assert "lib" in content


def test_configure_pth_no_extras(tmp_path: Path):
    pth = tmp_path / "python312._pth"
    pth.write_text("python312.zip\n.\n#import site\n")

    configure_pth(tmp_path)

    content = pth.read_text()
    assert "import site" in content
    assert "#import site" not in content


def test_extract_runtime_overwrites_existing(tmp_path: Path):
    zip_path = tmp_path / "test.zip"
    dest = tmp_path / "runtime"
    dest.mkdir()
    (dest / "old_file.txt").write_text("old")

    with zipfile.ZipFile(zip_path, "w") as zf:
        zf.writestr("python312._pth", ".\n")
        zf.writestr("new_file.txt", "new")

    extract_runtime(zip_path, dest)
    assert not (dest / "old_file.txt").exists()
    assert (dest / "new_file.txt").is_file()

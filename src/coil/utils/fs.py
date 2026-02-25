"""File system utilities."""

import shutil
from pathlib import Path


def ensure_dir(path: Path) -> Path:
    """Create a directory if it doesn't exist."""
    path.mkdir(parents=True, exist_ok=True)
    return path


def clean_dir(path: Path) -> Path:
    """Remove and recreate a directory."""
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    return path


def copy_tree(src: Path, dst: Path) -> Path:
    """Copy a directory tree, overwriting the destination."""
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(src, dst)
    return dst


def dir_size(path: Path) -> int:
    """Get the total size of a directory in bytes."""
    total = 0
    for f in path.rglob("*"):
        if f.is_file():
            total += f.stat().st_size
    return total


def format_size(size_bytes: int) -> str:
    """Format a byte count into a human-readable string."""
    if size_bytes < 1024:
        return f"{size_bytes} B"
    elif size_bytes < 1024 * 1024:
        return f"{size_bytes / 1024:.1f} KB"
    elif size_bytes < 1024 * 1024 * 1024:
        return f"{size_bytes / (1024 * 1024):.1f} MB"
    else:
        return f"{size_bytes / (1024 * 1024 * 1024):.1f} GB"

"""Embedded Python runtime management.

Handles downloading, caching, and configuring the Windows embeddable
Python distribution for use in Coil-built executables.
"""

import os
import shutil
import urllib.request
import zipfile
from pathlib import Path


CACHE_DIR = Path.home() / ".coil" / "cache" / "runtimes"

EMBED_URL_TEMPLATE = (
    "https://www.python.org/ftp/python/{version}/python-{version}-embed-{arch}.zip"
)


def get_embed_url(python_version: str, arch: str = "amd64") -> str:
    """Get the download URL for the Windows embeddable Python distribution.

    Args:
        python_version: Full version like "3.12.1" or short like "3.12".
        arch: Architecture, either "amd64" or "win32".

    Returns:
        Download URL string.
    """
    return EMBED_URL_TEMPLATE.format(version=python_version, arch=arch)


def get_cache_path(python_version: str, arch: str = "amd64") -> Path:
    """Get the cache directory path for a specific Python version."""
    return CACHE_DIR / f"python-{python_version}-{arch}"


def get_cached_zip_path(python_version: str, arch: str = "amd64") -> Path:
    """Get the path where the downloaded zip is cached."""
    return CACHE_DIR / f"python-{python_version}-embed-{arch}.zip"


def download_runtime(
    python_version: str,
    arch: str = "amd64",
    verbose: bool = False,
) -> Path:
    """Download the Windows embeddable Python distribution.

    Downloads to the cache directory if not already present.

    Args:
        python_version: Full Python version (e.g. "3.12.1").
        arch: Architecture, "amd64" or "win32".
        verbose: Print download progress.

    Returns:
        Path to the downloaded zip file.
    """
    zip_path = get_cached_zip_path(python_version, arch)

    if zip_path.is_file():
        if verbose:
            print(f"Using cached runtime: {zip_path}")
        return zip_path

    url = get_embed_url(python_version, arch)
    CACHE_DIR.mkdir(parents=True, exist_ok=True)

    if verbose:
        print(f"Downloading Python {python_version} embeddable from {url}...")

    try:
        urllib.request.urlretrieve(url, str(zip_path))
    except Exception as e:
        if zip_path.exists():
            zip_path.unlink()
        raise RuntimeError(
            f"Failed to download Python {python_version} embeddable distribution: {e}"
        ) from e

    if verbose:
        print(f"Downloaded to {zip_path}")

    return zip_path


def extract_runtime(
    zip_path: Path,
    dest_dir: Path,
    verbose: bool = False,
) -> Path:
    """Extract the embeddable Python distribution to a destination directory.

    Args:
        zip_path: Path to the downloaded zip file.
        dest_dir: Directory to extract into.
        verbose: Print extraction progress.

    Returns:
        Path to the extracted runtime directory.
    """
    if dest_dir.exists():
        shutil.rmtree(dest_dir)

    dest_dir.mkdir(parents=True, exist_ok=True)

    if verbose:
        print(f"Extracting runtime to {dest_dir}...")

    with zipfile.ZipFile(zip_path, "r") as zf:
        zf.extractall(dest_dir)

    if verbose:
        print("Runtime extracted.")

    return dest_dir


def configure_pth(runtime_dir: Path, extra_paths: list[str] | None = None) -> None:
    """Configure the ._pth file to set up sys.path for the embedded runtime.

    The ._pth file controls what directories are on sys.path when using the
    embeddable distribution. We need to add paths for the application code
    and bundled dependencies.

    Args:
        runtime_dir: Path to the extracted runtime directory.
        extra_paths: Additional paths to add to sys.path.
    """
    # Find the existing ._pth file (e.g. python312._pth)
    pth_files = list(runtime_dir.glob("python*._pth"))
    if not pth_files:
        raise FileNotFoundError(
            f"No ._pth file found in {runtime_dir}. "
            "Is this a valid embeddable Python distribution?"
        )

    pth_file = pth_files[0]
    lines = pth_file.read_text(encoding="utf-8").splitlines()

    # Uncomment "import site" if it's commented out
    new_lines = []
    for line in lines:
        if line.strip() == "#import site":
            new_lines.append("import site")
        else:
            new_lines.append(line)

    # Add extra paths
    if extra_paths:
        for path in extra_paths:
            if path not in new_lines:
                new_lines.append(path)

    pth_file.write_text("\n".join(new_lines) + "\n", encoding="utf-8")


def resolve_full_version(short_version: str, verbose: bool = False) -> str:
    """Resolve a short Python version (e.g. '3.12') to a full version (e.g. '3.12.1').

    Tries common patch versions starting from the highest.

    Args:
        short_version: Version like "3.12" or already full like "3.12.1".
        verbose: Print resolution progress.

    Returns:
        Full version string.
    """
    parts = short_version.split(".")
    if len(parts) >= 3:
        return short_version

    major, minor = int(parts[0]), int(parts[1])

    # Try patch versions from high to low
    for patch in range(20, -1, -1):
        full = f"{major}.{minor}.{patch}"
        url = get_embed_url(full)
        try:
            req = urllib.request.Request(url, method="HEAD")
            resp = urllib.request.urlopen(req, timeout=10)
            if resp.status == 200:
                if verbose:
                    print(f"Resolved Python {short_version} -> {full}")
                return full
        except Exception:
            continue

    raise RuntimeError(
        f"Could not find an embeddable Python distribution for version {short_version}. "
        "Check that the version exists at python.org/ftp/python/."
    )


def prepare_runtime(
    python_version: str,
    dest_dir: Path,
    app_paths: list[str] | None = None,
    arch: str = "amd64",
    verbose: bool = False,
) -> Path:
    """Full pipeline: resolve version, download, extract, and configure runtime.

    Args:
        python_version: Python version (short or full).
        dest_dir: Where to extract the runtime.
        app_paths: Extra paths to add to ._pth for application code.
        arch: Architecture.
        verbose: Verbose output.

    Returns:
        Path to the configured runtime directory.
    """
    full_version = resolve_full_version(python_version, verbose=verbose)
    zip_path = download_runtime(full_version, arch=arch, verbose=verbose)
    runtime_dir = extract_runtime(zip_path, dest_dir, verbose=verbose)
    configure_pth(runtime_dir, extra_paths=app_paths)
    return runtime_dir

"""Shared pytest fixtures for the Coil test suite."""

from __future__ import annotations

import sys
import zipfile
from pathlib import Path

import pytest


def _find_cached_embed_zip() -> Path | None:
    """Locate a cached Windows embeddable Python zip.

    Prefers a zip matching the host Python minor version (so the bundled
    runtime's ABI matches anything the host's interpreter produces —
    important for tests that execute produced exes or generated .pyc files).
    Falls back to any cached embed zip if no host-version match exists.
    """
    cache = Path.home() / ".coil" / "cache" / "runtimes"
    if not cache.is_dir():
        return None
    host_short = f"{sys.version_info.major}.{sys.version_info.minor}"
    preferred = sorted(cache.glob(f"python-{host_short}.*-embed-amd64.zip"))
    if preferred:
        return preferred[-1]
    fallback = sorted(cache.glob("python-*-embed-amd64.zip"))
    return fallback[-1] if fallback else None


@pytest.fixture(scope="session")
def real_runtime(tmp_path_factory) -> Path:
    """A Windows embeddable Python runtime extracted to a session-tmp dir.

    Skips when not on Windows or when no cached embed zip is available.
    Run `coil build` once on any project to populate the cache.

    Session-scoped: extracted once and shared across tests. The Coil
    packager only reads from runtime_dir (copies out, never mutates),
    so sharing is safe.
    """
    if sys.platform != "win32":
        pytest.skip("real_runtime fixture is Windows-only")

    zip_path = _find_cached_embed_zip()
    if zip_path is None:
        pytest.skip(
            "No cached embeddable Python runtime at ~/.coil/cache/runtimes. "
            "Run any `coil build` to populate it."
        )

    runtime = tmp_path_factory.mktemp("runtime")
    with zipfile.ZipFile(zip_path, "r") as zf:
        zf.extractall(runtime)
    return runtime

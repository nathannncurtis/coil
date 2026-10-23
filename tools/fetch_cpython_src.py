"""Fetch the pinned CPython source tree the front-end tests compile against.

Downloads the CPython 3.12.10 source tarball from python.org, verifies it
against the SHA256 recorded in tools/build_launcher.py, and extracts Lib/,
Grammar/ and Parser/Python.asdl into ~/.coil/cache/stdlib-src/3.12.10/
(--dest overrides). Idempotent: an existing extraction that carries a matching
verification stamp is reused without touching the network.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_launcher import ARCHIVE_SHA256  # noqa: E402

VERSION = "3.12.10"
ARCHIVE_NAME = f"Python-{VERSION}.tar.xz"
URL = f"https://www.python.org/ftp/python/{VERSION}/{ARCHIVE_NAME}"
SHA256 = ARCHIVE_SHA256[VERSION]
DEFAULT_DEST = Path.home() / ".coil" / "cache" / "stdlib-src" / VERSION
STAMP_NAME = ".verified"
# Everything below the leading "Python-3.12.10/" that the front end and
# tools/gen_unicode_data.py (the generated character-type tables) need.
EXTRACT_RULES = ("Lib/", "Grammar/", "Parser/Python.asdl", "Objects/unicodetype_db.h",
                 "Objects/unicodectype.c",
                 "Tools/unicode/makeunicodedata.py")


def _wanted(name: str) -> bool:
    return any(name == rule or name.startswith(rule) for rule in EXTRACT_RULES)


def is_verified(dest: Path) -> bool:
    stamp = dest / STAMP_NAME
    try:
        if stamp.read_text(encoding="ascii").strip() != SHA256:
            return False
        return (dest / "Lib").is_dir() and (dest / "Grammar").is_dir() and (dest / "Parser" / "Python.asdl").is_file() and (dest / "Objects" / "unicodetype_db.h").is_file() and (dest / "Objects" / "unicodectype.c").is_file() and (dest / "Tools" / "unicode" / "makeunicodedata.py").is_file()
    except OSError:
        return False


def download_archive(cache_dir: Path) -> Path:
    cache_dir.mkdir(parents=True, exist_ok=True)
    archive = cache_dir / ARCHIVE_NAME
    if archive.is_file() and hashlib.sha256(archive.read_bytes()).hexdigest() == SHA256:
        return archive
    print(f"Downloading {URL}", file=sys.stderr, flush=True)
    with tempfile.NamedTemporaryFile(prefix=".download-", suffix=".tar.xz", dir=cache_dir, delete=False) as stream:
        download_path = Path(stream.name)
    try:
        urllib.request.urlretrieve(URL, download_path)
        if hashlib.sha256(download_path.read_bytes()).hexdigest() != SHA256:
            raise ValueError(f"CPython source archive SHA256 mismatch: {download_path}")
        os.replace(download_path, archive)
    finally:
        download_path.unlink(missing_ok=True)
    return archive


def extract(archive: Path, dest: Path) -> None:
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    prefix = f"Python-{VERSION}/"
    root = dest.resolve()
    extracted = 0
    with tarfile.open(archive, "r:xz") as source:
        for member in source:
            if not member.name.startswith(prefix):
                continue
            name = member.name[len(prefix):]
            if not _wanted(name):
                continue
            if member.isdir():
                (dest / name).mkdir(parents=True, exist_ok=True)
                continue
            if not member.isfile():
                raise ValueError(f"Unsupported archive member: {member.name}")
            target = (dest / name).resolve()
            target.relative_to(root)
            target.parent.mkdir(parents=True, exist_ok=True)
            stream = source.extractfile(member)
            if stream is None:
                raise ValueError(f"Cannot read archive member: {member.name}")
            target.write_bytes(stream.read())
            extracted += 1
    (dest / STAMP_NAME).write_text(SHA256 + "\n", encoding="ascii")
    print(f"Extracted {extracted} files to {dest}", file=sys.stderr, flush=True)


def ensure_sources(dest: Path) -> Path:
    if is_verified(dest):
        print(f"Reusing verified extraction at {dest}", file=sys.stderr, flush=True)
        return dest
    # The tarball always lives in the canonical cache, so --dest runs do not
    # re-download what the default extraction already fetched.
    extract(download_archive(DEFAULT_DEST.parent), dest)
    return dest


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dest", type=Path, default=DEFAULT_DEST,
                        help=f"extraction root (default: {DEFAULT_DEST})")
    parser.add_argument("--print-root", action="store_true",
                        help="print the extraction root to stdout")
    args = parser.parse_args(argv)
    root = ensure_sources(args.dest.expanduser())
    if args.print_root:
        print(root)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

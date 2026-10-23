"""Measure the C API surface the v1 extension set imports (spec section 10.1).

Downloads the embeddable 3.12.10 runtime and the pinned wheels of the 15.3
extension set into ~/.coil/cache (--cache overrides), SHA256-verifies every
file, reads the import tables of every .pyd and .dll they contain, and prints
the python312.dll and python3.dll symbols each package imports, per package
and as a union.

native/manifest/capi_targets.txt is that union with the packages that need
each symbol: --write-manifest regenerates it, --check-manifest fails on
drift. --check LIST --runtime DLL fails while any symbol in LIST is still a
stub in the given python312.dll, through the runtime's coil_rt_stub_count
and coil_rt_is_stub exports.
"""

from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from gen_capi import CPYTHON_VERSION, EMBED_ZIP_SHA256  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = ROOT / "native/manifest/capi_targets.txt"
EMBED_ZIP_NAME = f"python-{CPYTHON_VERSION}-embed-amd64.zip"
RUNTIME_ZIP_URL = f"https://www.python.org/ftp/python/{CPYTHON_VERSION}/{EMBED_ZIP_NAME}"
PYPI_JSON = "https://pypi.org/pypi/{project}/{version}/json"

# python3.dll is a forwarder, so a name reaches the same export either way;
# the union counts names, per package the two DLLs are reported separately.
RUNTIME_DLLS = ("python312.dll", "python3.dll")

# The v1 extension set of spec section 15.3: every member that ships Windows
# binaries. The pure-Python members (pdfplumber, pdfminer.six, pyautogui and
# its dependencies) have no PE files to read. Each row is the latest patch
# release of the minor the spec pins, except numpy, which section 10.1 pins
# at 2.5.1; per-package symbol counts match the ones the spec quotes.
# (package, PyPI project, version, wheel filename, SHA256)
WHEELS = (
    ("numpy", "numpy", "2.5.1", "numpy-2.5.1-cp312-cp312-win_amd64.whl",
     "f7d60026c0bdb1380e83bfa7a0419c4577ee4b9a08880afcb6dadeb74c649fa2"),
    ("Pillow", "pillow", "13.0.0", "pillow-13.0.0-cp312-cp312-win_amd64.whl",
     "ff3fe9eb621fbb9db4c4be33ab946f6bac5f98ab50ca8d3260503653656c961b"),
    ("pywin32", "pywin32", "312", "pywin32-312-cp312-cp312-win_amd64.whl",
     "b457f6d628a47e8a7346ce22acb7e1a46a4a78b52e1d17e1af56871bd19a93bc"),
    ("PyQt6", "pyqt6", "6.11.0", "pyqt6-6.11.0-cp310-abi3-win_amd64.whl",
     "bd11b459c54dca068e988a42cf838303334f0d441b9d16d92ae6719fcb5ac6ba"),
    ("PyQt6-sip", "pyqt6-sip", "13.13.0", "pyqt6_sip-13.13.0-cp312-cp312-win_amd64.whl",
     "c6b67d301ef64e13d313c728020616315c952473abb27029b2e5857b44a7b3e6"),
    ("PyQt5", "pyqt5", "5.15.11", "PyQt5-5.15.11-cp38-abi3-win_amd64.whl",
     "bdde598a3bb95022131a5c9ea62e0a96bd6fb28932cc1619fd7ba211531b7517"),
    ("PyQt5-sip", "pyqt5-sip", "12.20.0", "pyqt5_sip-12.20.0-cp312-cp312-win_amd64.whl",
     "6bcf4b28693b8bf22ff038ad60cb1aa7bb461bb469b39f0f8d4d85aea34506bc"),
    ("cryptography", "cryptography", "50.0.2", "cryptography-50.0.2-cp311-abi3-win_amd64.whl",
     "7afa5a6602a9f29af1f3a2965f831bae7c9d5d597b7cbb716d41ab3b7d89879c"),
    ("cffi", "cffi", "2.1.1", "cffi-2.1.1-cp312-cp312-win_amd64.whl",
     "f53e442b08449d42821fa4a4fba000095af9f62742a500f978a9f557ec44339a"),
    ("pydantic-core", "pydantic-core", "2.50.1", "pydantic_core-2.50.1-cp312-cp312-win_amd64.whl",
     "132529c83901437ff642f585216831bf5fd7a91df66829907e155192ead62498"),
    ("orjson", "orjson", "3.13.0", "orjson-3.13.0-cp312-cp312-win_amd64.whl",
     "b571236d8393edcd3236e07423f762bfcf571f852aad667a3bce9e7b755e0790"),
    ("lxml", "lxml", "6.1.3", "lxml-6.1.3-cp312-cp312-win_amd64.whl",
     "3e9a00d1c2c30936f7add097c41afc5da6556c580909104aafd382cac92a855c"),
    ("psutil", "psutil", "7.2.2", "psutil-7.2.2-cp37-abi3-win_amd64.whl",
     "eb7e81434c8d223ec4a219b5fc1c47d0417b12be7ea866e24fb5ad6e84b3d988"),
    ("regex", "regex", "2026.9.29", "regex-2026.9.29-cp312-cp312-win_amd64.whl",
     "fdd88ed5e20b1bcdd234421e454962c971aa44b653bdb7f1ea9ef683e90fb649"),
    ("markupsafe", "markupsafe", "3.0.4", "markupsafe-3.0.4-cp312-cp312-win_amd64.whl",
     "11935df9bf455ed0c04eb87bcd720f02b1fe5e02128a9430f23aed6f93336fc7"),
    ("charset-normalizer", "charset-normalizer", "3.5.2",
     "charset_normalizer-3.5.2-cp312-cp312-win_amd64.whl",
     "780fbe7cab297b81dad9fb8dc5eb003c0468ffb0d9e5f65068c53a34661a96bc"),
    ("pypdfium2", "pypdfium2", "5.14.0", "pypdfium2-5.14.0-py3-none-win_amd64.whl",
     "149fd5c6397b8df8bf7911a93506eff0be874f877afe7ac936cf5d37d21a6a06"),
)
# The C API work's first milestone, quoted with its expected count in 10.1.
MILESTONE = ("embeddable", "numpy", "Pillow")

# Per-package symbol counts the spec quotes for this set (sections 10.1
# and 15.3); check_quoted_counts fails if a pin stops matching its figure,
# so re-pinning cannot silently change documented coverage. The spec's
# union totals (564 for the milestone, 610 for the whole set) are another
# matter: none of these pins postdates the spec's 2026-10-21 PyPI
# snapshot, the milestone's inputs are byte-identical to the spec's, and
# no variant measured (pyd-only, with the support DLLs, with the
# embeddable's executables, delay imports merged in) reproduces them.
# The cause of that difference is unidentified; the manifest documents
# what these pins measure.
SPEC_COUNTS = {
    "numpy": 396,
    "Pillow": 113,
    "PyQt5": 151,
    "PyQt5-sip": 150,
    "cryptography": 150,
    "cffi": 178,
    "pydantic-core": 163,
    "orjson": 63,
    "charset-normalizer": 189,
}


def verified(path: Path, sha256: str) -> bool:
    return path.is_file() and hashlib.sha256(path.read_bytes()).hexdigest() == sha256


def fetch(url: str, path: Path, sha256: str) -> Path:
    """Download url to path unless the cached copy already hashes to sha256."""
    if verified(path, sha256):
        return path
    print(f"Downloading {url}", flush=True)
    path.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(prefix=".download-", dir=path.parent, delete=False) as stream:
        partial = Path(stream.name)
    try:
        urllib.request.urlretrieve(url, partial)
        if hashlib.sha256(partial.read_bytes()).hexdigest() != sha256:
            raise ValueError(f"SHA256 mismatch: {partial}")
        os.replace(partial, path)
    finally:
        partial.unlink(missing_ok=True)
    return path


def fetch_runtime_zip(cache: Path) -> Path:
    return fetch(RUNTIME_ZIP_URL, cache / "runtimes" / EMBED_ZIP_NAME, EMBED_ZIP_SHA256)


def fetch_wheel(cache: Path, pin: tuple[str, str, str, str, str]) -> Path:
    _, project, version, filename, sha256 = pin
    path = cache / "wheels" / filename
    if verified(path, sha256):
        return path
    with urllib.request.urlopen(PYPI_JSON.format(project=project, version=version), timeout=60) as response:
        files = json.load(response)["urls"]
    url = next((f["url"] for f in files if f["filename"] == filename), None)
    if url is None:
        raise ValueError(f"{project} {version} publishes no wheel named {filename}")
    return fetch(url, path, sha256)


def imported_symbols(data: bytes) -> dict[str, set[str]]:
    """Map the lowercased name of every imported DLL to the symbols imported."""
    import pefile

    pe = pefile.PE(data=data, fast_load=True)
    pe.parse_data_directories(
        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_IMPORT"]]
    )
    table: dict[str, set[str]] = {}
    for entry in getattr(pe, "DIRECTORY_ENTRY_IMPORT", []):
        dll = entry.dll.decode("ascii").lower()
        table[dll] = {imp.name.decode("ascii") for imp in entry.imports if imp.name}
    return table


def record(
    usage: dict[str, dict[str, set[str]]],
    package: str,
    archive: zipfile.ZipFile,
    members: list[str],
) -> None:
    for member in members:
        for dll, symbols in imported_symbols(archive.read(member)).items():
            if dll in RUNTIME_DLLS:
                usage.setdefault(package, {}).setdefault(dll, set()).update(symbols)


def measure(cache: Path) -> dict[str, dict[str, set[str]]]:
    """Package -> runtime DLL -> the symbols its binaries import."""
    usage: dict[str, dict[str, set[str]]] = {}
    with zipfile.ZipFile(fetch_runtime_zip(cache)) as embedded:
        members = [n for n in embedded.namelist() if n.lower().endswith((".pyd", ".dll"))]
        record(usage, "embeddable", embedded, members)
    for pin in WHEELS:
        with zipfile.ZipFile(fetch_wheel(cache, pin)) as wheel:
            members = [n for n in wheel.namelist() if n.lower().endswith((".pyd", ".dll"))]
            record(usage, pin[0], wheel, members)
    return usage


def needed_by(usage: dict[str, dict[str, set[str]]]) -> dict[str, set[str]]:
    """Symbol -> the packages whose import tables name it."""
    names: dict[str, set[str]] = {}
    for package, per_dll in usage.items():
        for symbols in per_dll.values():
            for symbol in symbols:
                names.setdefault(symbol, set()).add(package)
    return names


def render(usage: dict[str, dict[str, set[str]]]) -> str:
    needed = needed_by(usage)
    direct = len({name for per_dll in usage.values() for name in per_dll.get(RUNTIME_DLLS[0], ())})
    forwarded = len({name for per_dll in usage.values() for name in per_dll.get(RUNTIME_DLLS[1], ())})
    header = f"""\
# Generated by tools/capi_usage.py from the import tables of the embeddable
# {CPYTHON_VERSION} runtime and the v1 extension set (native-compiler-spec.md
# sections 10.1 and 15.3): every python312.dll symbol an import table needs,
# python3.dll forwarders included, with the packages that need it. Verify
# with `python tools/capi_usage.py --check-manifest`; regenerate with
# --write-manifest.
#
# Measured union of these pins: {len(needed)} distinct symbols over
# {len(usage)} packages, {direct} imported directly from python312.dll and
# {forwarded} through python3.dll forwarders. The spec's totals for the
# same set (564 for embeddable + numpy + Pillow, 610 for the whole set)
# are not reproduced by these pins; see SPEC_COUNTS in tools/capi_usage.py.
"""
    lines = [f"{symbol} {' '.join(sorted(needed[symbol]))}" for symbol in sorted(needed)]
    return header + "\n".join(lines) + "\n"


def check_quoted_counts(usage: dict[str, dict[str, set[str]]]) -> None:
    """Fail when a pinned package no longer matches the spec's quoted count."""
    for package, quoted in SPEC_COUNTS.items():
        symbols: set[str] = set()
        for per_dll in usage[package].values():
            symbols |= per_dll
        if len(symbols) != quoted:
            raise SystemExit(
                f"{package} imports {len(symbols)} symbols, but the spec quotes "
                f"{quoted} for this pin; change the two together"
            )


def report(usage: dict[str, dict[str, set[str]]]) -> None:
    for package, per_dll in usage.items():
        counts = ", ".join(f"{dll} {len(per_dll.get(dll, ()))}" for dll in RUNTIME_DLLS)
        print(f"{package}: {counts}")
    for dll in RUNTIME_DLLS:
        union: set[str] = set()
        for per_dll in usage.values():
            union |= per_dll.get(dll, set())
        print(f"union: {dll} {len(union)}")
    print(f"union: {len(needed_by(usage))} distinct symbols over {len(usage)} packages")
    milestone: set[str] = set()
    for package in MILESTONE:
        for symbols in usage[package].values():
            milestone |= symbols
    print(f"{' + '.join(MILESTONE)} (section 10.1 milestone): {len(milestone)}")


def read_list(path: Path) -> list[str]:
    """Symbols from a manifest written by --write-manifest or a plain list."""
    symbols = []
    for line in path.read_text(encoding="utf-8").splitlines():
        tokens = line.split("#", 1)[0].split()
        if not tokens:
            continue
        if tokens[0].lower() in RUNTIME_DLLS:
            if len(tokens) == 1:
                continue
            symbols.append(tokens[1])
        else:
            symbols.append(tokens[0])
    return symbols


def check_stubs(runtime_dll: Path, symbols: list[str]) -> None:
    """Fail while any listed symbol is still a stub in the given runtime."""
    try:
        runtime = ctypes.WinDLL(str(runtime_dll))
    except (AttributeError, OSError) as exc:
        raise SystemExit(f"cannot load {runtime_dll}: {exc}; --check runs on Windows")
    try:
        stub_count = runtime.coil_rt_stub_count
        is_stub = runtime.coil_rt_is_stub
    except AttributeError as exc:
        raise SystemExit(f"{runtime_dll.name} does not export {exc.name}; build the current coil-rt")
    stub_count.restype = ctypes.c_size_t
    is_stub.argtypes = [ctypes.c_char_p]
    is_stub.restype = ctypes.c_int
    still_stubs = []
    unknown = []
    for symbol in symbols:
        verdict = is_stub(symbol.encode("ascii"))
        if verdict == 1:
            still_stubs.append(symbol)
        elif verdict < 0:
            unknown.append(symbol)
    print(f"{runtime_dll.name}: coil_rt_stub_count reports {stub_count()} C API stubs")
    if unknown:
        for symbol in unknown:
            print(f"not an export of {runtime_dll.name}: {symbol}")
        raise SystemExit(f"{len(unknown)} listed symbols are not exports of {runtime_dll.name}")
    if still_stubs:
        for symbol in still_stubs:
            print(f"still a stub: {symbol}")
        raise SystemExit(
            f"{len(still_stubs)} of {len(symbols)} listed symbols are still stubs"
            f" in {runtime_dll.name}"
        )
    print(f"all {len(symbols)} listed symbols are implemented")


def check_manifest(usage: dict[str, dict[str, set[str]]]) -> None:
    expected = render(usage)
    actual = MANIFEST_PATH.read_text(encoding="utf-8")
    if actual == expected:
        print(f"{MANIFEST_PATH.relative_to(ROOT)}: {expected.count(chr(10))} entries, current")
        return
    expected_lines = expected.splitlines()
    actual_lines = actual.splitlines()
    print(
        f"{MANIFEST_PATH.relative_to(ROOT)}: out of date "
        f"(generated {len(expected_lines)} entries, checked in {len(actual_lines)})"
    )
    for number, (wanted, found) in enumerate(zip(expected_lines, actual_lines), 1):
        if wanted != found:
            print(f"  first difference at line {number}: {found!r} != {wanted!r}")
            break
    raise SystemExit(
        "Run python tools/capi_usage.py --write-manifest and commit the updated "
        + MANIFEST_PATH.name
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--cache", type=Path, default=Path.home() / ".coil" / "cache",
        help="download and cache directory (default: ~/.coil/cache)",
    )
    parser.add_argument(
        "--write-manifest", action="store_true",
        help="write native/manifest/capi_targets.txt",
    )
    parser.add_argument(
        "--check-manifest", action="store_true",
        help="fail if native/manifest/capi_targets.txt is not current",
    )
    parser.add_argument(
        "--check", type=Path, metavar="LIST",
        help="fail if any symbol in LIST is still a stub in --runtime",
    )
    parser.add_argument(
        "--runtime", type=Path, metavar="DLL",
        help="the python312.dll to inspect with --check",
    )
    args = parser.parse_args()
    try:
        import pefile  # noqa: F401
    except ImportError:
        raise SystemExit("pefile is required: pip install pefile")
    if args.check is not None:
        if args.runtime is None:
            parser.error("--check needs --runtime PATH to a built python312.dll")
        check_stubs(args.runtime, read_list(args.check))
        return
    usage = measure(args.cache.resolve())
    check_quoted_counts(usage)
    report(usage)
    if args.write_manifest:
        MANIFEST_PATH.parent.mkdir(parents=True, exist_ok=True)
        MANIFEST_PATH.write_text(render(usage), encoding="utf-8", newline="\n")
        print(f"Wrote {MANIFEST_PATH.relative_to(ROOT)}")
    if args.check_manifest:
        check_manifest(usage)


if __name__ == "__main__":
    main()

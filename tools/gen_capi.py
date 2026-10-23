"""Checked-in C API export lists from the pinned CPython 3.12.10 runtime.

Regenerates native/crates/coil-capi-sys/exports.txt (python312.dll) and
native/crates/coil-capi-sys/python3-exports.txt (the python3.dll stable-ABI
forwarder) from the real DLLs in the official embeddable distribution, which
are authoritative. With --check the lists are regenerated in memory and any
difference from the checked-in files is a failure.
"""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import re
import urllib.request
import zipfile

import build_launcher

ROOT = Path(__file__).resolve().parents[1]
CPYTHON_VERSION = "3.12.10"
EMBED_ZIP_SHA256 = "4acbed6dd1c744b0376e3b1cf57ce906f9dc9e95e68824584c8099a63025a3c3"
RUNTIME_ZIP_URL = (
    "https://www.python.org/ftp/python/"
    f"{CPYTHON_VERSION}/python-{CPYTHON_VERSION}-embed-amd64.zip"
)
CRATE = ROOT / "native/crates/coil-capi-sys"
# The name python3.dll's forwarder strings point at.
FORWARDER_TARGET = "python312"
# python312.dll is authoritative for its own export kinds; python3.dll only
# forwards, so its entries resolve through python312.dll and, for the three
# exports this build drops (PyOS_CheckStack, _Py_NegativeRefcount,
# _Py_RefTotal), through their declarations in the pinned headers.
PYTHON312_EXPORTS = CRATE / "exports.txt"
PYTHON3_EXPORTS = CRATE / "python3-exports.txt"
# Section characteristic: exports in executable sections are functions.
IMAGE_SCN_MEM_EXECUTE = 0x20000000


def fetch_runtime_zip(build_dir: Path) -> Path:
    """Download and hash-verify the embeddable distribution holding the DLLs."""
    archive = build_dir / f"python-{CPYTHON_VERSION}-embed-amd64.zip"
    if not archive.exists():
        print(f"Downloading {RUNTIME_ZIP_URL}", flush=True)
        urllib.request.urlretrieve(RUNTIME_ZIP_URL, archive)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != EMBED_ZIP_SHA256:
        raise ValueError(f"Embeddable distribution SHA256 mismatch: {archive}")
    return archive


def declared_kinds(headers: Path) -> tuple[set[str], set[str]]:
    """Names declared PyAPI_DATA / PyAPI_FUNC in the pinned headers."""
    data: set[str] = set()
    func: set[str] = set()
    data_pattern = re.compile(r"\bPyAPI_DATA\s*\([^;]*?\)\s*\b([A-Za-z_]\w*)\s*[;\[=]")
    func_pattern = re.compile(r"\bPyAPI_FUNC\s*\([^;]*?\)\s*\b([A-Za-z_]\w*)\s*\(")
    for header in sorted(headers.rglob("*.h")):
        text = header.read_text(errors="replace")
        data.update(data_pattern.findall(text))
        func.update(func_pattern.findall(text))
    return data, func


def export_kinds(
    dll: Path, headers: Path, resolved: dict[str, str] | None = None
) -> dict[str, str]:
    """Map every export name of the DLL to "func" or "data".

    An export is a function when its address lands in an executable section.
    Forwarded exports have a string address instead, so they resolve through
    the target DLL's table (python312.dll) and then through the declarations
    in the pinned headers.
    """
    import pefile

    pe = pefile.PE(str(dll), fast_load=True)
    pe.parse_data_directories(
        directories=[pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_EXPORT"]]
    )
    exports = getattr(pe, "DIRECTORY_ENTRY_EXPORT", None)
    if exports is None:
        raise ValueError(f"{dll.name} has no export table")
    sections = [
        (
            section.VirtualAddress,
            section.VirtualAddress
            + max(section.Misc_VirtualSize, section.SizeOfRawData),
            section.Characteristics,
        )
        for section in pe.sections
    ]
    data_names, func_names = (set(), set()) if resolved is None else declared_kinds(headers)
    kinds: dict[str, str] = {}
    for symbol in exports.symbols:
        if symbol.name is None:
            raise ValueError(f"{dll.name} exports no name at ordinal {symbol.ordinal}")
        name = symbol.name.decode("ascii")
        if symbol.forwarder:
            target_dll, _, target = symbol.forwarder.decode("ascii").partition(".")
            if resolved is None:
                raise ValueError(f"{dll.name}: unexpected forwarded export {name}")
            if target_dll != FORWARDER_TARGET:
                raise ValueError(
                    f"{dll.name}: {name} forwards to {target_dll!r}, "
                    f"only {FORWARDER_TARGET} forwarders are handled"
                )
            if target in resolved:
                kinds[name] = resolved[target]
            elif target in data_names and target not in func_names:
                kinds[name] = "data"
            elif target in func_names and target not in data_names:
                kinds[name] = "func"
            else:
                raise ValueError(f"{dll.name}: {name} forwards to unclassified {target}")
        else:
            for start, end, characteristics in sections:
                if start <= symbol.address < end:
                    kinds[name] = (
                        "func" if characteristics & IMAGE_SCN_MEM_EXECUTE else "data"
                    )
                    break
            else:
                raise ValueError(f"{dll.name}: {name} points outside every section")
    return kinds


def render(kinds: dict[str, str]) -> str:
    return "".join(f"{name} {kinds[name]}\n" for name in sorted(kinds))


def generate(build_dir: Path) -> dict[Path, str]:
    headers = build_launcher.prepare_headers(CPYTHON_VERSION, build_dir)
    archive = fetch_runtime_zip(build_dir)
    with zipfile.ZipFile(archive) as embedded:
        for name in ("python312.dll", "python3.dll"):
            embedded.extract(name, build_dir)
    python312 = export_kinds(build_dir / "python312.dll", headers)
    python3 = export_kinds(build_dir / "python3.dll", headers, resolved=python312)
    return {PYTHON312_EXPORTS: render(python312), PYTHON3_EXPORTS: render(python3)}


def check(generated: dict[Path, str]) -> None:
    failures = []
    for path, expected in generated.items():
        actual = path.read_text(encoding="utf-8")
        if actual == expected:
            print(f"{path.relative_to(ROOT)}: {expected.count(chr(10))} entries, current")
            continue
        failures.append(path)
        expected_lines = expected.splitlines()
        actual_lines = actual.splitlines()
        print(
            f"{path.relative_to(ROOT)}: out of date "
            f"(generated {len(expected_lines)} entries, checked in {len(actual_lines)})"
        )
        for number, (wanted, found) in enumerate(zip(expected_lines, actual_lines), 1):
            if wanted != found:
                print(f"  first difference at line {number}: {found!r} != {wanted!r}")
                break
    if failures:
        raise SystemExit(
            "Run python tools/gen_capi.py and commit the updated "
            + ", ".join(path.name for path in failures)
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check", action="store_true", help="fail if the export lists are not current"
    )
    parser.add_argument(
        "--build-dir",
        type=Path,
        default=ROOT / "build/capi",
        help="download and cache directory",
    )
    args = parser.parse_args()
    try:
        import pefile  # noqa: F401
    except ImportError:
        raise SystemExit("pefile is required: pip install pefile")
    build_dir = args.build_dir.resolve()
    build_dir.mkdir(parents=True, exist_ok=True)
    generated = generate(build_dir)
    if args.check:
        check(generated)
    else:
        for path, content in generated.items():
            path.write_text(content, encoding="utf-8", newline="\n")
            print(f"Wrote {path.relative_to(ROOT)}: {content.count(chr(10))} entries")


if __name__ == "__main__":
    main()

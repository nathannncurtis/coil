"""Build CPython 3.12.10's _testcapi extension against coil's python312 library.

Run from an x64 Native Tools prompt, or pass --vcvars to vcvars64.bat.
Extracts Modules/_testcapimodule.c and Modules/_testcapi/* from the pinned
CPython source archive and builds _testcapi.pyd with MSVC, linked against
coil's python312.lib import library. Downloads and intermediate files stay
below --build-dir (default: build/testcapi).
"""

from __future__ import annotations

import argparse
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_launcher import ROOT, compiler_environment, prepare_headers


VERSION = "3.12.10"
PREFIX = f"Python-{VERSION}/"
# The module entry point and the generated feature-macro table it includes.
MODULE_FILES = ("Modules/_testcapimodule.c", "Modules/_testcapi_feature_macros.inc")


def prepare_module_sources(archive: Path, build_dir: Path) -> Path:
    """Extract the _testcapi sources, preserving the include layout."""
    modules = build_dir / "Modules"
    modules.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive) as source:
        for member in source:
            if not member.isfile() or not member.name.startswith(PREFIX):
                continue
            name = member.name[len(PREFIX):]
            if name not in MODULE_FILES and not name.startswith("Modules/_testcapi/"):
                continue
            target = (modules / name[len("Modules/"):]).resolve()
            target.relative_to(modules.resolve())
            target.parent.mkdir(parents=True, exist_ok=True)
            stream = source.extractfile(member)
            assert stream is not None
            target.write_bytes(stream.read())
    return modules


def find_python_lib(explicit: Path | None) -> Path:
    if explicit is not None:
        lib = explicit.resolve()
    else:
        # Cargo links the cdylib through deps/python312.dll.lib; only the dll
        # itself is copied up into the profile directory. CI passes
        # --python-lib because the shared build cache can restore other
        # profiles' import libraries into the target directory, and linking
        # against one of those would resolve against a stale export table.
        lib = None
        for profile in ("release", "debug"):
            candidate = ROOT / "native" / "target" / profile / "deps" / "python312.dll.lib"
            if candidate.is_file():
                lib = candidate
                break
        if lib is None:
            raise SystemExit(
                "python312.dll.lib not found; run cargo build -p coil-rt or pass --python-lib"
            )
    if not lib.is_file():
        raise SystemExit(f"python312 import library not found: {lib}")
    return lib


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-dir", type=Path, default=ROOT / "build/testcapi")
    parser.add_argument("--python-lib", type=Path)
    parser.add_argument("--vcvars", type=Path)
    args = parser.parse_args()
    build_dir = args.build_dir.resolve()
    build_dir.mkdir(parents=True, exist_ok=True)
    environment = compiler_environment(args.vcvars)
    compiler = shutil.which(
        "cl.exe",
        path=next(value for key, value in environment.items() if key.lower() == "path"),
    )
    if not compiler:
        raise SystemExit("MSVC cl.exe was not found in the compiler environment")
    headers = prepare_headers(VERSION, build_dir)
    archive = build_dir / f"Python-{VERSION}.tar.xz"
    modules = prepare_module_sources(archive, build_dir)
    python_lib = find_python_lib(args.python_lib)
    # pyconfig.h nominates python312.lib via #pragma comment(lib, ...), and
    # python3.lib for objects built with Py_LIMITED_API; stage the cargo
    # import library under both names on the lib path so the pragmas resolve
    # to the same archive we pass explicitly.
    for name in ("python312.lib", "python3.lib"):
        staged = build_dir / name
        if python_lib != staged.resolve():
            shutil.copyfile(python_lib, staged)
    python_lib = build_dir / "python312.lib"
    include_flags = [
        f"/I{headers}",
        f"/I{headers / 'internal'}",
        f"/I{modules}",
        f"/I{modules / '_testcapi'}",
    ]
    flags = [
        "/nologo", "/O1", "/MT", "/W4", "/std:c11", "/Brepro",
        "/DUNICODE", "/D_UNICODE", *include_flags,
    ]
    sources = sorted(modules.glob("*.c")) + sorted((modules / "_testcapi").glob("*.c"))
    if not sources:
        raise SystemExit(f"no _testcapi sources found under {modules}")
    objects = []
    for source in sources:
        rel = source.relative_to(modules).with_suffix("")
        obj = build_dir / "obj" / (str(rel).replace("\\", "_").replace("/", "_") + ".obj")
        obj.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            [compiler, *flags, "/c", str(source), f"/Fo{obj}"],
            cwd=build_dir, env=environment, check=True,
        )
        objects.append(obj)
    output = build_dir / "_testcapi.pyd"
    subprocess.run(
        [compiler, *flags, "/DLL", *map(str, objects), str(python_lib),
         f"/Fe{output}", "/link", "/SUBSYSTEM:WINDOWS", "/MACHINE:X64",
         "/DYNAMICBASE", "/NXCOMPAT", "/HIGHENTROPYVA", "/Brepro",
         f"/LIBPATH:{build_dir}"],
        cwd=build_dir, env=environment, check=True,
    )
    print(f"Wrote {output}", flush=True)


if __name__ == "__main__":
    main()

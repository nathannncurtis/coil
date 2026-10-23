"""Layout oracle for coil-rt-abi (spec section 8.2).

Compiles tools/layout_oracle.c against the pinned CPython 3.12.10 headers
with MSVC -- the same header preparation tools/build_launcher.py uses -- and
runs it. The program prints one line per constant in
native/crates/coil-rt-abi/src/cp312.rs:

  <struct>.<field> offset <bytes>
  <struct> sizeof <bytes>

where the struct and field names are the Rust module and constant names,
lowercased. Without --check the lines are printed verbatim; with --check they
are compared against the Rust constants instead, and any mismatch, any
constant without an oracle line, and any oracle line without a constant fail
the run. Run it where MSVC reproduces the target layout: Windows x64.
"""

from __future__ import annotations

import argparse
import re
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from build_launcher import compiler_environment, prepare_headers  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
# The same pinned source tools/build_launcher.py verifies and extracts.
VERSION = "3.12.10"
ORACLE_C = ROOT / "tools" / "layout_oracle.c"
RUST_CONSTANTS = ROOT / "native" / "crates" / "coil-rt-abi" / "src" / "cp312.rs"
ORACLE_LINE = re.compile(r"(\S+) (offset|sizeof) (\d+)$")
RUST_MODULE = re.compile(r"pub mod (\w+) \{$")
RUST_CONST = re.compile(r"pub const ([A-Za-z0-9_]+): usize = (\d+);$")


def parse_rust_constants(path: Path) -> dict[str, int]:
    """Map "module.const" to the value in cp312.rs; SIZEOF becomes module.sizeof."""
    constants: dict[str, int] = {}
    module = None
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        head = RUST_MODULE.fullmatch(line)
        if head:
            module = head.group(1)
        elif line == "}":
            module = None
        elif module is not None:
            const = RUST_CONST.fullmatch(line)
            if const:
                name = const.group(1).lower()
                constants[f"{module}.{name}"] = int(const.group(2))
    if not constants:
        raise ValueError(f"No pub const declarations found: {path}")
    return constants


def oracle_constants(build_dir: Path, vcvars: Path | None) -> dict[str, int]:
    """Compile and run the C oracle, mapping its lines the same way."""
    headers = prepare_headers(VERSION, build_dir)
    environment = compiler_environment(vcvars)
    path = next(value for key, value in environment.items() if key.lower() == "path")
    compiler = shutil.which("cl.exe", path=path)
    if not compiler:
        raise SystemExit("MSVC cl.exe was not found in the compiler environment")
    obj = build_dir / "layout_oracle.obj"
    exe = build_dir / "layout_oracle.exe"
    command = [compiler, "/nologo", "/std:c11", f"/I{headers}", str(ORACLE_C),
               f"/Fo{obj}", f"/Fe{exe}", "/link", "/SUBSYSTEM:CONSOLE"]
    subprocess.run(command, cwd=build_dir, env=environment, check=True)
    completed = subprocess.run([str(exe)], check=True, stdout=subprocess.PIPE, text=True)
    constants: dict[str, int] = {}
    for line in completed.stdout.splitlines():
        match = ORACLE_LINE.fullmatch(line)
        if not match:
            raise ValueError(f"Unrecognised oracle line: {line!r}")
        name, kind, value = match.group(1), match.group(2), int(match.group(3))
        if kind == "sizeof":
            name = f"{name}.sizeof"
        if name in constants:
            raise ValueError(f"Duplicate oracle line for: {name}")
        constants[name] = value
    return constants


def compare(rust: dict[str, int], oracle: dict[str, int]) -> list[str]:
    problems = []
    for name in sorted(set(rust) - set(oracle)):
        problems.append(f"no oracle line for {name}")
    for name in sorted(set(oracle) - set(rust)):
        problems.append(f"oracle line without a cp312.rs constant: {name}")
    for name in sorted(set(rust) & set(oracle)):
        if rust[name] != oracle[name]:
            problems.append(f"{name}: cp312.rs has {rust[name]}, headers say {oracle[name]}")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-dir", type=Path, default=ROOT / "build" / "layout",
                        help=f"download and build directory (default: {ROOT / 'build' / 'layout'})")
    parser.add_argument("--vcvars", type=Path, help="path to vcvars64.bat for MSVC")
    parser.add_argument("--check", action="store_true",
                        help="compare the oracle against cp312.rs instead of printing it")
    args = parser.parse_args(argv)
    build_dir = args.build_dir.resolve()
    build_dir.mkdir(parents=True, exist_ok=True)
    oracle = oracle_constants(build_dir, args.vcvars)
    if not args.check:
        for name, value in oracle.items():
            if name.endswith(".sizeof"):
                print(f"{name[:-len('.sizeof')]} sizeof {value}")
            else:
                print(f"{name} offset {value}")
        return 0
    problems = compare(parse_rust_constants(RUST_CONSTANTS), oracle)
    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print(f"OK: {len(oracle)} constants match the CPython {VERSION} headers")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

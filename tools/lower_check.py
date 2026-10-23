"""Lowering acceptance check (spec sections 4, 5, 12.4).

Runs `coil-front-dump --ir` over the given .py files and reports one line per
file: `ok`, `unsupported: <kinds>` (the t11/t12 constructs the file uses) or
`error: <message>`. Files CPython 3.12 itself cannot parse are skipped: they
are outside the accepted set. Exit status is 0 only when no file reports an
error.

Must run on CPython 3.12 (the pinned target); standard library only.
"""

from __future__ import annotations

import argparse
import io
import json
import subprocess
import sys
import symtable
import tokenize
from pathlib import Path
from typing import Iterable

DEFAULT_DUMP = Path(__file__).resolve().parents[1] / "native" / "target" / "debug" / "coil-front-dump"


def require_python312() -> None:
    if sys.version_info[:2] != (3, 12):
        raise SystemExit(
            f"lower_check must run on CPython 3.12 (the pinned target); "
            f"this interpreter is {sys.version.split()[0]}"
        )


def collect_paths(inputs: Iterable[str]) -> list[Path]:
    paths: list[Path] = []
    for item in inputs:
        path = Path(item)
        if path.is_dir():
            paths.extend(child for child in path.rglob("*.py") if child.is_file())
        else:
            paths.append(path)
    return sorted(dict.fromkeys(paths), key=str)


def run_dump(dump: Path, paths: list[Path], optimize: int) -> dict[str, dict[str, object]]:
    records: dict[str, dict[str, object]] = {}
    chunk: list[Path] = []
    for path in list(paths) + [None]:  # sentinel flushes the last chunk
        if path is not None:
            chunk.append(path)
        if path is None or len(chunk) == 256:
            if chunk:
                proc = subprocess.run(
                    [str(dump), "--ir", "--optimize", str(optimize), *(str(p) for p in chunk)],
                    capture_output=True,
                    text=True,
                )
                if proc.returncode != 0:
                    sys.stderr.write(proc.stderr)
                    raise SystemExit(f"coil-front-dump failed with status {proc.returncode}")
                for line in proc.stdout.splitlines():
                    record = json.loads(line)
                    records[str(record["path"])] = record
                chunk = []
    return records


def decode_like_cpython(raw: bytes) -> str:
    """Decode source exactly as CPython's compile() does: the coding cookie
    (or BOM) selects the codec, unknown codecs and undecodable bytes are
    errors, and the decoded text is universal-newline-normalised."""
    encoding, _ = tokenize.detect_encoding(io.BytesIO(raw).readline)
    return raw.decode(encoding).replace("\r\n", "\n").replace("\r", "\n")


def main(argv: list[str] | None = None) -> int:
    require_python312()
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("inputs", nargs="+", help=".py files or directories")
    parser.add_argument("--dump", type=Path, default=DEFAULT_DUMP, help="coil-front-dump binary")
    parser.add_argument("--optimize", type=int, default=0, choices=(0, 1, 2),
                        help="optimisation level passed to the lowerer")
    args = parser.parse_args(argv)

    paths = collect_paths(args.inputs)
    accepted: list[Path] = []
    skipped = 0
    for path in paths:
        # CPython's front end up to the stage lowering consumes: parsing
        # plus the symtable (which runs the future-feature checks). Files
        # rejected here, such as Lib/test badsyntax_future fixtures, are
        # outside the accepted set.
        try:
            symtable.symtable(decode_like_cpython(path.read_bytes()), str(path), "exec")
        except (SyntaxError, ValueError, UnicodeDecodeError, LookupError):
            skipped += 1
            continue
        accepted.append(path)

    records = run_dump(args.dump, accepted, args.optimize)

    ok = 0
    unsupported: dict[str, int] = {}
    errors = 0
    for path in accepted:
        record = records.get(str(path))
        if record is None:
            print(f"{path}: error: no dump record")
            errors += 1
            continue
        if not record.get("ok"):
            error = record.get("error", {})
            print(f"{path}: error: {error.get('type')}: {error.get('msg')}")
            errors += 1
        elif record.get("unsupported"):
            kinds = list(record["unsupported"])
            for kind in kinds:
                unsupported[kind] = unsupported.get(kind, 0) + 1
            print(f"{path}: unsupported: {', '.join(kinds)}")
        else:
            ok += 1
            print(f"{path}: ok")

    summary = [
        f"{len(paths)} files, {skipped} skipped (CPython front end rejects)",
        f"{ok} ok, {sum(unsupported.values())} unsupported, {errors} errors",
    ]
    if unsupported:
        summary.append(
            "unsupported kinds: " + ", ".join(f"{k} x{v}" for k, v in sorted(unsupported.items()))
        )
    print("; ".join(summary))
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())

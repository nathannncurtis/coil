"""Differential harness: run the CPython oracle and the Rust dumper side by side.

Invokes tools/front_oracle.py in-process and a dumper command as a subprocess
in the same mode over the same file list, compares the JSON Lines records per
file and reports the first differing location as a JSON pointer (RFC 6901)
into the file's object. The dumper receives the same mode flag and a batch of
the expanded file list (``<dumper> --tokens|--ast|--symtable FILE...``) and
must write the same JSON Lines to stdout. Files go in batches small enough for
Windows' command-line limit, and each batch is compared and dropped before the
next runs, so memory stays bounded by a batch rather than the whole tree
(--ast over all of Lib/ is about 400 MB of JSON per side).

Error objects are compared field by field: only ``type`` and ``lineno`` in
--tokens mode; ``type``, ``lineno`` and ``offset`` in --ast mode; all fields
in --symtable mode. Two records whose compared fields agree count as
``both_error``, not as a mismatch.

Prints ``files``, ``match``, ``mismatch`` and ``both_error`` counts and one
``mismatch <path> <pointer>`` line per differing file; exits 1 on any
mismatch, 2 when the dumper itself fails.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import front_oracle  # noqa: E402

DEFAULT_DUMPER = "cargo run --release -q -p coil-front --bin coil-front-dump --"
# CreateProcess caps a command line at 32,767 characters; leave room for the dumper itself.
BATCH_CHARS = 24_000
BATCH_FILES = 64
ERROR_FIELDS = {"tokens": ("type", "lineno"), "ast": ("type", "lineno", "offset")}


def split_command(command: str) -> list[str]:
    if os.name == "nt":
        # POSIX splitting eats backslashes, which Windows paths are full of.
        tokens = shlex.split(command, posix=False)
        stripped = []
        for token in tokens:
            if len(token) > 1 and token[0] == token[-1] and token[0] in ("'", '"'):
                token = token[1:-1]
            stripped.append(token)
        return stripped
    return shlex.split(command)


def run_dumper(command: str, mode: str, paths: list[Path]) -> list[dict]:
    tokens = split_command(command) + [f"--{mode}"] + [str(path) for path in paths]
    try:
        completed = subprocess.run(tokens, capture_output=True, text=True)
    except OSError as exc:
        print(f"cannot run the dumper: {exc}", file=sys.stderr)
        raise SystemExit(2) from exc
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr)
        print(f"the dumper exited with code {completed.returncode}", file=sys.stderr)
        raise SystemExit(2)
    records = []
    for number, line in enumerate(completed.stdout.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as exc:
            print(f"dumper output line {number} is not JSON: {exc}", file=sys.stderr)
            raise SystemExit(2) from exc
        if not isinstance(record, dict) or not isinstance(record.get("path"), str):
            print(f"dumper output line {number} is not a record object", file=sys.stderr)
            raise SystemExit(2)
        records.append(record)
    return records


def escape_pointer_token(key: str) -> str:
    return key.replace("~", "~0").replace("/", "~1")


def first_difference(left: object, right: object, pointer: str = "") -> str | None:
    if type(left) is not type(right):
        return pointer
    if isinstance(left, dict):
        if list(left) != list(right):
            return pointer
        for key in left:
            child = first_difference(left[key], right[key], pointer + "/" + escape_pointer_token(key))
            if child is not None:
                return child
        return None
    if isinstance(left, list):
        if len(left) != len(right):
            return pointer
        for index, (left_item, right_item) in enumerate(zip(left, right)):
            child = first_difference(left_item, right_item, f"{pointer}/{index}")
            if child is not None:
                return child
        return None
    return None if left == right else pointer


def compared_part(record: dict, mode: str) -> dict:
    if record.get("ok", False):
        return record
    error = record.get("error")
    if mode in ERROR_FIELDS:
        error = {field: error.get(field) for field in ERROR_FIELDS[mode]}
    return {"path": record.get("path"), "ok": False, "error": error}


def batches(paths: list[Path], max_chars: int = BATCH_CHARS, max_files: int = BATCH_FILES) -> list[list[Path]]:
    out: list[list[Path]] = []
    current: list[Path] = []
    size = 0
    for path in paths:
        length = len(str(path)) + 3
        if current and (size + length > max_chars or len(current) >= max_files):
            out.append(current)
            current, size = [], 0
        current.append(path)
        size += length
    if current:
        out.append(current)
    return out


def diff(mode: str, oracle_records: list[dict], dumper_records: list[dict]) -> dict:
    by_path = {record["path"]: record for record in dumper_records}
    seen = set()
    counts = {"files": 0, "match": 0, "mismatch": 0, "both_error": 0}
    mismatches: list[tuple[str, str]] = []
    for oracle in oracle_records:
        path = oracle["path"]
        seen.add(path)
        counts["files"] += 1
        dumper = by_path.get(path)
        if dumper is None:
            counts["mismatch"] += 1
            mismatches.append((path, "missing from dumper output"))
            continue
        pointer = first_difference(compared_part(oracle, mode), compared_part(dumper, mode))
        if pointer is None:
            counts["both_error" if not oracle.get("ok", False) else "match"] += 1
        else:
            counts["mismatch"] += 1
            mismatches.append((path, pointer))
    for record in dumper_records:
        if record["path"] not in seen:
            counts["files"] += 1
            counts["mismatch"] += 1
            mismatches.append((record["path"], "not produced by the oracle"))
    return {"counts": counts, "mismatches": mismatches}


def diff_batched(mode: str, dumper: str, paths: list[Path], **limits: int) -> dict:
    counts = {"files": 0, "match": 0, "mismatch": 0, "both_error": 0}
    mismatches: list[tuple[str, str]] = []
    for batch in batches(paths, **limits):
        result = diff(mode, front_oracle.records(mode, batch), run_dumper(dumper, mode, batch))
        for name in counts:
            counts[name] += result["counts"][name]
        mismatches.extend(result["mismatches"])
    return {"counts": counts, "mismatches": mismatches}


def main(argv: list[str] | None = None) -> int:
    front_oracle.require_python312()
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--tokens", action="store_const", const="tokens", dest="mode")
    modes.add_argument("--ast", action="store_const", const="ast", dest="mode")
    modes.add_argument("--symtable", action="store_const", const="symtable", dest="mode")
    parser.add_argument("inputs", nargs="+", metavar="FILE_OR_DIR")
    parser.add_argument("--dumper", default=DEFAULT_DUMPER,
                        help=f"command producing the Rust side's JSON Lines (default: {DEFAULT_DUMPER})")
    args = parser.parse_args(argv)
    result = diff_batched(args.mode, args.dumper, front_oracle.collect_paths(args.inputs))
    for path, detail in result["mismatches"]:
        print(f"mismatch {path} {detail or '(whole object)'}")
    for name in ("files", "match", "mismatch", "both_error"):
        print(f"{name} {result['counts'][name]}")
    return 1 if result["counts"]["mismatch"] else 0


if __name__ == "__main__":
    raise SystemExit(main())

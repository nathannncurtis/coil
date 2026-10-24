"""Generate the ABI constants shared by the Rust runtime and the compiler.

Reads native/abi.toml — the contract between coil-rt and compiled code
(spec section 2) — and writes native/crates/coil-rt/src/abi_generated.rs
(the format version, the SHA-256 of abi.toml, and the contract tables)
and src/coil/native/abi_constants.py (the same values for the Python
side). Both outputs are checked in; ``--check`` regenerates in memory
and exits non-zero when a checked-in copy is stale.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import tomllib
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
ABI_TOML = REPO_ROOT / "native" / "abi.toml"
RUST_OUTPUT = REPO_ROOT / "native" / "crates" / "coil-rt" / "src" / "abi_generated.rs"
PYTHON_OUTPUT = REPO_ROOT / "src" / "coil" / "native" / "abi_constants.py"


def load_contract() -> tuple[dict, bytes]:
    # Hash the contract's content, not the checkout: a Windows clone may
    # hold the file with CRLF, and the runtime and a cross-compiled
    # executable must agree on the hash regardless of line endings
    # (spec sections 16 and 17).
    data = ABI_TOML.read_bytes().replace(b"\r\n", b"\n")
    contract = tomllib.loads(data.decode("utf-8"))
    version = contract.get("meta", {}).get("version")
    if not isinstance(version, int) or isinstance(version, bool) or version < 1:
        raise SystemExit(f"{ABI_TOML}: [meta] needs a positive integer version")
    check_functions(contract.get("functions", {}))
    check_offsets(contract.get("offsets", {}))
    check_formats(contract.get("formats", {}))
    return contract, hashlib.sha256(data).digest()


def check_functions(table: dict) -> None:
    """[functions]: name -> {ret: str, args: [str]}, C-spelled."""
    for name, sig in table.items():
        if not name.startswith("coil_rt_"):
            raise SystemExit(f"{ABI_TOML}: [functions] entry {name!r} is not a coil_rt_* name")
        if not isinstance(sig, dict) or set(sig) != {"ret", "args"}:
            raise SystemExit(
                f"{ABI_TOML}: [functions].{name} needs 'ret' (str) and 'args' ([str])"
            )
        if not isinstance(sig["ret"], str):
            raise SystemExit(
                f"{ABI_TOML}: [functions].{name} needs 'ret' (str) and 'args' ([str])"
            )
        if not isinstance(sig["args"], list) or not all(
            isinstance(arg, str) for arg in sig["args"]
        ):
            raise SystemExit(
                f"{ABI_TOML}: [functions].{name} needs 'ret' (str) and 'args' ([str])"
            )


def check_offsets(table: dict) -> None:
    """[offsets]: struct group -> {field: int bytes, may be negative}."""
    for group, fields in table.items():
        if not isinstance(fields, dict) or not fields:
            raise SystemExit(f"{ABI_TOML}: [offsets.{group}] needs named integer fields")
        for field, value in fields.items():
            if not isinstance(value, int) or isinstance(value, bool):
                raise SystemExit(
                    f"{ABI_TOML}: [offsets.{group}].{field} must be an integer byte count"
                )


def check_formats(table: dict) -> None:
    """[formats]: format name -> ordered {field: description}; a field may
    be a group, its value a dict of subfield -> description."""
    for name, fields in table.items():
        if not isinstance(fields, dict) or not fields:
            raise SystemExit(f"{ABI_TOML}: [formats.{name}] needs named fields")
        for field, desc in fields.items():
            if isinstance(desc, dict):
                if not desc or not all(isinstance(sub, str) for sub in desc.values()):
                    raise SystemExit(
                        f"{ABI_TOML}: [formats.{name}].{field} group needs string subfields"
                    )
            elif not isinstance(desc, str):
                raise SystemExit(f"{ABI_TOML}: [formats.{name}].{field} must be a string")


def format_rows(fields: dict) -> list[tuple[str, str]]:
    """Flatten one format's fields into ordered (name, description) rows,
    a group's subfields as `group.field`."""
    rows: list[tuple[str, str]] = []
    for field, desc in fields.items():
        if isinstance(desc, dict):
            for sub, subdesc in desc.items():
                rows.append((f"{field}.{sub}", subdesc))
        else:
            rows.append((field, desc))
    return rows


def rust_function_table(table: dict) -> list[str]:
    lines = [
        "/// The `coil_rt_*` entry points compiled code may call, with their",
        "/// C-spelled signatures (abi.toml [functions]).",
        "#[rustfmt::skip]",
        "pub static ABI_FUNCTIONS: &[(&str, &str, &[&str])] = &[",
    ]
    for name, sig in table.items():
        args = ", ".join(f'"{arg}"' for arg in sig["args"])
        lines.append(f'    ("{name}", "{sig["ret"]}", &[{args}]),')
    lines.append("];")
    return lines


def rust_offset_table(table: dict) -> list[str]:
    lines = [
        "/// Struct offsets compiled code touches directly, flattened as",
        "/// `group.field` (abi.toml [offsets]).",
        "#[rustfmt::skip]",
        "pub static ABI_OFFSETS: &[(&str, isize)] = &[",
    ]
    for group, fields in table.items():
        for field, value in fields.items():
            lines.append(f'    ("{group}.{field}", {value}),')
    lines.append("];")
    return lines


def rust_format_table(table: dict) -> list[str]:
    lines = [
        "/// The binary table formats the compiler emits and the runtime",
        "/// loads, each field described (abi.toml [formats]).",
        "#[rustfmt::skip]",
        "pub static ABI_FORMATS: &[(&str, &[(&str, &str)])] = &[",
    ]
    for name, fields in table.items():
        rows = ", ".join(f'("{field}", "{desc}")' for field, desc in format_rows(fields))
        lines.append(f'    ("{name}", &[{rows}]),')
    lines.append("];")
    return lines


def rust_source(contract: dict, digest: bytes) -> str:
    lines = [
        "// Generated by tools/gen_abi.py from native/abi.toml. Do not edit:",
        "// rerun the generator instead.",
        "",
        "/// Format version of the runtime/compiled-code contract (abi.toml [meta]).",
        f"pub const ABI_FORMAT_VERSION: u32 = {contract['meta']['version']};",
        "",
        "/// SHA-256 of native/abi.toml's content (LF line endings) when this",
        "/// file was generated. Compiled code embeds the same value;",
        "/// `coil_rt_init` refuses a mismatch (section 2).",
        "#[rustfmt::skip]",
        "pub static ABI_SHA256: [u8; 32] = [",
    ]
    for row in range(4):
        chunk = digest[row * 8 : (row + 1) * 8]
        lines.append("    " + " ".join(f"0x{byte:02x}," for byte in chunk))
    lines.append("];")
    for section in (
        rust_function_table(contract.get("functions", {})),
        rust_offset_table(contract.get("offsets", {})),
        rust_format_table(contract.get("formats", {})),
    ):
        lines.append("")
        lines.extend(section)
    return "\n".join(lines) + "\n"


def python_source(contract: dict, digest: bytes) -> str:
    functions = contract.get("functions", {})
    offsets = contract.get("offsets", {})
    formats = contract.get("formats", {})
    lines = [
        '"""Generated constants for the runtime/compiled-code contract.',
        "",
        "Generated by tools/gen_abi.py from native/abi.toml. Do not edit:",
        "rerun the generator instead.",
        '"""',
        "",
        f"ABI_FORMAT_VERSION: int = {contract['meta']['version']}",
        "",
        "# SHA-256 of native/abi.toml's content (LF line endings) when this",
        "# file was generated. The driver embeds it in compiled executables",
        "# and the runtime refuses a mismatch (section 2).",
        f'ABI_SHA256: bytes = bytes.fromhex("{digest.hex()}")',
        "",
        "# The coil_rt_* entry points compiled code may call: name to",
        "# (return type, argument types), C-spelled (abi.toml [functions]).",
        "ABI_FUNCTIONS: dict[str, tuple[str, list[str]]] = {",
    ]
    for name, sig in functions.items():
        args = ", ".join(f'"{arg}"' for arg in sig["args"])
        lines.append(f'    "{name}": ("{sig["ret"]}", [{args}]),')
    lines.append("}")
    lines += [
        "",
        "# Struct offsets compiled code touches directly: `group.field` to",
        "# byte count (abi.toml [offsets]).",
        "ABI_OFFSETS: dict[str, int] = {",
    ]
    for group, fields in offsets.items():
        for field, value in fields.items():
            lines.append(f'    "{group}.{field}": {value},')
    lines.append("}")
    lines += [
        "",
        "# The binary table formats the compiler emits and the runtime",
        "# loads: format name to ordered field descriptions (abi.toml [formats]).",
        "ABI_FORMATS: dict[str, dict[str, str]] = {",
    ]
    for name, fields in formats.items():
        lines.append(f'    "{name}": {{')
        for field, desc in format_rows(fields):
            lines.append(f'        "{field}": "{desc}",')
        lines.append("    },")
    lines.append("}")
    return "\n".join(lines) + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true",
                        help="fail if a checked-in output is stale")
    args = parser.parse_args(argv)
    contract, digest = load_contract()
    outputs = (
        (RUST_OUTPUT, rust_source(contract, digest)),
        (PYTHON_OUTPUT, python_source(contract, digest)),
    )
    if args.check:
        stale = False
        for path, source in outputs:
            current = path.read_text(encoding="utf-8") if path.is_file() else ""
            if current != source:
                print(f"{path} is stale; run tools/gen_abi.py to regenerate it", file=sys.stderr)
                stale = True
        return 1 if stale else 0
    for path, source in outputs:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8", newline="\n")
        print(f"Wrote {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

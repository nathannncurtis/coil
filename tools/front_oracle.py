"""CPython 3.12 front-end oracle for the differential tests (spec sections 4, 11.1).

Runs the interpreter's own tokenizer, parser and symtable over Python source
files and emits one canonical JSON object per file (JSON Lines on stdout,
sorted by path). The Rust front end must reproduce this output exactly; this
module therefore refuses to run on anything but CPython 3.12, the pinned
target whose grammar the parser implements.

Modes (one required), each taking files or directories, recursing into *.py:

  --tokens     tokenize.tokenize over the file's bytes
  --ast        ast.parse(source_bytes, filename, mode="exec")
  --symtable   symtable.symtable(source, filename, "exec")

Canonical AST encoding (the Rust side emits exactly this):

* A node is ``{"_type": <class name>}`` followed by each field in ``_fields``
  order and each attribute in ``_attributes`` order. Lists become arrays,
  ``None`` becomes ``null``, identifiers stay strings, numbers stay numbers.
  Nodes without fields - the enum-like ``Load``, ``Store``, ``Add``, ``Eq``,
  ... - fall out of the general rule as ``{"_type": "Load"}``.
* ``Constant.value`` and ``MatchSingleton.value`` become a tagged object,
  because JSON cannot carry every Python constant directly:
    str       {"const": "str", "v": [<code points as ints>]}
    bytes     {"const": "bytes", "v": "<lowercase hex>"}
    int       {"const": "int", "v": "<decimal string>"}
    float     {"const": "float", "v": "<repr>"}
    complex   {"const": "complex", "real": "<repr>", "imag": "<repr>"}
    bool      {"const": "bool", "v": true|false}
    None      {"const": "None"}
    Ellipsis  {"const": "Ellipsis"}
* ``type_ignores`` is encoded like any other field.

Symbol tables record ``name``, ``type``, ``lineno``, ``is_optimized``,
``is_nested``, ``symbols`` (each name mapped to the sorted list of the true
predicates among those probed) and ``children`` recursively; symbols are
listed in sorted name order, children in ``get_children()`` order.

Objects are serialised with json.dumps(..., ensure_ascii=True,
separators=(",", ":"), sort_keys=False).
"""

from __future__ import annotations

import argparse
import ast
import io
import json
import sys
import tokenize
from pathlib import Path
from typing import Iterable

import symtable


def require_python312() -> None:
    if sys.version_info[:2] != (3, 12):
        raise SystemExit(
            f"front_oracle must run on CPython 3.12 (the pinned target); "
            f"this interpreter is {sys.version.split()[0]}"
        )


# The predicates probed per symbol; those that hold are reported sorted by name.
SYMBOL_PREDICATES = (
    "is_referenced",
    "is_imported",
    "is_parameter",
    "is_global",
    "is_declared_global",
    "is_local",
    "is_annotated",
    "is_free",
    "is_assigned",
    "is_namespace",
    "is_nonlocal",
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


def dumps(obj: object) -> str:
    return json.dumps(obj, ensure_ascii=True, separators=(",", ":"), sort_keys=False)


# --- AST encoding ---------------------------------------------------------


def encode_constant(value: object) -> dict[str, object]:
    if value is None:
        return {"const": "None"}
    if value is Ellipsis:
        return {"const": "Ellipsis"}
    if isinstance(value, bool):
        return {"const": "bool", "v": value}
    if isinstance(value, str):
        return {"const": "str", "v": [ord(char) for char in value]}
    if isinstance(value, bytes):
        return {"const": "bytes", "v": value.hex()}
    if isinstance(value, int):
        return {"const": "int", "v": str(value)}
    if isinstance(value, float):
        return {"const": "float", "v": repr(value)}
    if isinstance(value, complex):
        return {"const": "complex", "real": repr(value.real), "imag": repr(value.imag)}
    raise ValueError(f"unsupported constant value of type {type(value).__name__}")


def encode_node(node: ast.AST) -> dict[str, object]:
    result: dict[str, object] = {"_type": type(node).__name__}
    is_constant = isinstance(node, (ast.Constant, ast.MatchSingleton))
    for field in node._fields:
        value = getattr(node, field, None)
        result[field] = encode_constant(value) if is_constant and field == "value" else encode_value(value)
    for attribute in node._attributes:
        result[attribute] = encode_value(getattr(node, attribute, None))
    return result


def encode_value(value: object) -> object:
    if isinstance(value, ast.AST):
        return encode_node(value)
    if isinstance(value, (list, tuple)):
        return [encode_value(item) for item in value]
    if value is None or isinstance(value, (str, int, bool)):
        return value
    raise ValueError(f"unsupported AST field value of type {type(value).__name__}")


# --- per-file records ------------------------------------------------------


def _error_fields(exc: BaseException) -> dict[str, object]:
    fields: dict[str, object] = {"type": type(exc).__name__}
    if isinstance(exc, SyntaxError):
        fields.update(
            msg=str(exc.msg),
            lineno=exc.lineno,
            offset=exc.offset,
            end_lineno=exc.end_lineno,
            end_offset=exc.end_offset,
        )
    else:
        fields.update(msg=str(exc), lineno=None, offset=None, end_lineno=None, end_offset=None)
    return fields


def _token_lineno(exc: BaseException) -> int | None:
    lineno = getattr(exc, "lineno", None)
    if lineno is None and exc.args and isinstance(exc.args[-1], tuple) and exc.args[-1]:
        lineno = exc.args[-1][0]
    return lineno if isinstance(lineno, int) else None


def tokens_record(path: Path) -> dict[str, object]:
    try:
        data = path.read_bytes()
        tokens = []
        for token in tokenize.tokenize(io.BytesIO(data).readline):
            tokens.append([
                tokenize.tok_name[token.exact_type],
                token.string,
                [token.start[0], token.start[1]],
                [token.end[0], token.end[1]],
            ])
    except Exception as exc:
        return {"path": str(path), "ok": False,
                "error": {"type": type(exc).__name__, "lineno": _token_lineno(exc)}}
    return {"path": str(path), "ok": True, "tokens": tokens}


def ast_record(path: Path) -> dict[str, object]:
    try:
        tree = ast.parse(path.read_bytes(), str(path), mode="exec")
    except Exception as exc:
        return {"path": str(path), "ok": False, "error": _error_fields(exc)}
    return {"path": str(path), "ok": True, "ast": encode_node(tree)}


def encode_table(table: symtable.SymbolTable) -> dict[str, object]:
    symbols = {}
    for symbol in sorted(table.get_symbols(), key=lambda item: item.get_name()):
        symbols[symbol.get_name()] = sorted(
            name for name in SYMBOL_PREDICATES if getattr(symbol, name)()
        )
    return {
        "name": table.get_name(),
        "type": table.get_type(),
        "lineno": table.get_lineno(),
        "is_optimized": table.is_optimized(),
        "is_nested": table.is_nested(),
        "symbols": symbols,
        "children": [encode_table(child) for child in table.get_children()],
    }


def symtable_record(path: Path) -> dict[str, object]:
    try:
        table = symtable.symtable(path.read_bytes(), str(path), "exec")
    except Exception as exc:
        return {"path": str(path), "ok": False, "error": _error_fields(exc)}
    return {"path": str(path), "ok": True, "symtable": encode_table(table)}


RECORDERS = {"tokens": tokens_record, "ast": ast_record, "symtable": symtable_record}


def records(mode: str, paths: Iterable[Path | str]) -> list[dict[str, object]]:
    recorder = RECORDERS[mode]
    return [recorder(Path(path)) for path in sorted(paths, key=str)]


def main(argv: list[str] | None = None) -> int:
    require_python312()
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--tokens", action="store_const", const="tokens", dest="mode")
    modes.add_argument("--ast", action="store_const", const="ast", dest="mode")
    modes.add_argument("--symtable", action="store_const", const="symtable", dest="mode")
    parser.add_argument("inputs", nargs="+", metavar="FILE_OR_DIR",
                        help="Python files, or directories recursed into *.py")
    args = parser.parse_args(argv)
    for path in collect_paths(args.inputs):
        print(dumps(RECORDERS[args.mode](path)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

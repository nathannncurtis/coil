"""Generate the raising-stub C API export layer (spec section 10.1).

python312.dll must export every CPython 3.12.10 C API symbol: the 1706-name
table in native/crates/coil-capi-sys/exports.txt is the real DLL's export
table. Entries coil-rt does not implement yet are exported as stubs:
functions raise SystemError("coil: <name> is not implemented") and data
entries are zero-initialized placeholders.

The tool scans coil-rt for the symbols it implements, resolves each missing
symbol's C type from the pinned CPython headers (the same archive
build_launcher.py uses), and writes native/crates/coil-rt/src/capi_stubs.rs.
--check regenerates in memory and diffs against the checked-in file.
"""

from __future__ import annotations

import argparse
import difflib
import re
import sys
from collections import defaultdict
from pathlib import Path

from build_launcher import prepare_headers

ROOT = Path(__file__).resolve().parents[1]
RT_SRC = ROOT / "native/crates/coil-rt/src"
EXPORTS = ROOT / "native/crates/coil-capi-sys/exports.txt"
GENERATED = RT_SRC / "capi_stubs.rs"
CPYTHON_VERSION = "3.12.10"

# ---------------------------------------------------------------- coil-rt scan

NO_MANGLE = re.compile(r"#\[unsafe\(no_mangle\)\]\s*\n")
MACRO_DEF = re.compile(r"macro_rules!\s*\w+\s*\{")
ITEM = re.compile(
    r"\s*(?:\s*#\[[^\]]*\]\s*)*"
    r"(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?(?:extern\s+\"C\"\s+)?(fn|static(?:\s+mut)?|const)\s+([A-Za-z_]\w*)"
)
IDENTS = re.compile(r"[A-Za-z_]\w*")
EXC_ENTRY = re.compile(r'\s*"(\w+)"\s*,\s*(\w+)\s*,\s*(\w+)\s*(?:,\s*(\w+))?\s*=')


def macro_definition_spans(text: str) -> list[tuple[int, int]]:
    """Spans of macro_rules! bodies that contain no_mangle (their items are
    templates, not exports)."""
    spans = []
    for match in MACRO_DEF.finditer(text):
        depth, i = 1, match.end()
        while i < len(text) and depth:
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
            i += 1
        if "no_mangle" in text[match.start():i]:
            spans.append((match.start(), i))
    return spans


def invocation_body(text: str, start: int) -> str:
    """The balanced ( ... ) or { ... } body of a macro invocation."""
    i = start
    while i < len(text) and text[i] not in "({":
        i += 1
    open_ch, close_ch = text[i], {"(": ")", "{": "}"}[text[i]]
    depth, j = 0, i
    while j < len(text):
        if text[j] == open_ch:
            depth += 1
        elif text[j] == close_ch:
            depth -= 1
            if not depth:
                return text[i + 1:j]
        j += 1
    raise SystemExit(f"unbalanced macro invocation at offset {start}")


def mask_comments(text: str) -> str:
    """Replace comment characters with spaces, preserving offsets and
    newlines, so regex scans never match inside a comment."""
    out = list(text)
    i, n = 0, len(text)
    in_str = False
    while i < n:
        ch = text[i]
        if in_str:
            if ch == "\\":
                i += 2
                continue
            if ch == '"':
                in_str = False
            i += 1
            continue
        if ch == '"':
            in_str = True
            i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                out[i] = " "
                i += 1
            continue
        if ch == "/" and i + 1 < n and text[i + 1] == "*":
            depth = 1
            out[i] = out[i + 1] = " "
            i += 2
            while i < n and depth:
                if text[i] == "/" and i + 1 < n and text[i + 1] == "*":
                    depth += 1
                    out[i] = out[i + 1] = " "
                    i += 2
                elif text[i] == "*" and i + 1 < n and text[i + 1] == "/":
                    depth -= 1
                    out[i] = out[i + 1] = " "
                    i += 2
                else:
                    if text[i] != "\n":
                        out[i] = " "
                    i += 1
            continue
        i += 1
    return "".join(out)


def strip_comments(body: str) -> str:
    return "\n".join(
        line for line in body.splitlines() if not line.strip().startswith("//")
    )


def char_predicate_names(body: str) -> set[str]:
    return {IDENTS.search(strip_comments(body)).group(0)}


def unicode_error_accessor_names(body: str) -> set[str]:
    text = re.sub(r'"[^"]*"', " ", strip_comments(body))
    return set(IDENTS.findall(text)[:8])


def view_type_names(body: str) -> set[str]:
    return {IDENTS.search(strip_comments(body)).group(0)}


def exception_type_names(body: str) -> set[str]:
    names = set()
    for entry in strip_comments(body).split(";"):
        if not entry.strip():
            continue
        match = EXC_ENTRY.match(entry)
        if match is None:
            raise SystemExit(f"unparseable exception_types! entry: {entry[:60]!r}")
        # `ExceptionGroup` has no PyExc slot in 3.12.10's export table.
        if match.group(4) is not None:
            names.add(match.group(4))
    return names


MACRO_EXTRACTORS = {
    "char_predicate": char_predicate_names,
    "unicode_error_accessors": unicode_error_accessor_names,
    "view_type": view_type_names,
    "iter_type": view_type_names,
    "exception_types": exception_type_names,
}
# The macro definitions attach #[unsafe(no_mangle)] to the items they expand
# to, so the invocations themselves carry no attribute; find them by name.
MACRO_INVOCATION_ANY = re.compile(r"\b(" + "|".join(MACRO_EXTRACTORS) + r")\s*!")


def implemented_names() -> set[str]:
    names: set[str] = set()
    for path in sorted(RT_SRC.rglob("*.rs")):
        if path.name == GENERATED.name:
            continue
        text = mask_comments(path.read_text())
        spans = macro_definition_spans(text)
        for match in NO_MANGLE.finditer(text):
            if any(a <= match.start() < b for a, b in spans):
                continue
            item = ITEM.match(text[match.end():])
            if item is None:
                raise SystemExit(
                    f"unparseable no_mangle item in {path.relative_to(ROOT)}: "
                    f"{text[match.end():match.end() + 60]!r}"
                )
            names.add(item.group(2))
        for match in MACRO_INVOCATION_ANY.finditer(text):
            if any(a <= match.start() < b for a, b in spans):
                continue
            names |= MACRO_EXTRACTORS[match.group(1)](
                invocation_body(text, match.end())
            )
    return names


# ------------------------------------------------------- header type resolution

FUNC_RE = re.compile(r"PyAPI_FUNC\s*\(([^)]*)\)\s*(?:_Py_NO_RETURN\s+)?([A-Za-z_]\w*)\s*\(")
FNPTR_RE = re.compile(
    r"PyAPI_DATA\s*\(([^)]*)\)\s*(\*)?\s*\(\s*\*\s*([A-Za-z_]\w*)\s*\)\s*\(([^)]*)\)"
)
DATAFUNC_RE = re.compile(r"PyAPI_DATA\s*\(([^)]*)\)\s*([A-Za-z_]\w*)\s*\(")
DATA_RE = re.compile(r"PyAPI_DATA\s*\(([^)]*)\)\s*([A-Za-z_]\w*)\s*(\[\s*(\d*)\s*\])?\s*[;=]")
OBJDEF_RE = re.compile(
    r"^\s*#\s*define\s+([A-Za-z_]\w*)\s+([A-Za-z_]\w*)\s*(?://.*)?$", re.M
)
FNDEF_RE = re.compile(r"^\s*#\s*define\s+([A-Za-z_]\w*)\s*\(([^)]*)\)\s*(\S.*)$", re.M)


def load_declarations(headers: Path) -> dict:
    """name -> list of (kind, type, extra) from the pinned headers, plus the
    object-like alias and function-like macro maps used to resolve names the
    headers declare under a macro spelling."""
    decls: dict[str, list[tuple[str, str, str | None]]] = defaultdict(list)
    value_to_names: dict[str, list[str]] = defaultdict(list)
    fn_macros: dict[str, str] = {}
    for path in sorted(headers.rglob("*.h")):
        text = mask_comments(path.read_text())
        for match in FNPTR_RE.finditer(text):
            ret = match.group(1) + (" *" if match.group(2) else "")
            decls[match.group(3)].append(("fnptr", ret, match.group(4)))
        for match in DATAFUNC_RE.finditer(text):
            decls[match.group(2)].append(("datafunc", match.group(1), None))
        for match in FUNC_RE.finditer(text):
            decls[match.group(2)].append(("func", match.group(1), None))
        for match in DATA_RE.finditer(text):
            decls[match.group(2)].append(("data", match.group(1), match.group(4)))
        for match in OBJDEF_RE.finditer(text):
            value_to_names[match.group(2)].append(match.group(1))
        for match in FNDEF_RE.finditer(text):
            fn_macros.setdefault(match.group(1), match.group(3))
    return {"decls": decls, "value_to_names": value_to_names, "fn_macros": fn_macros}


def resolve(name: str, table: dict) -> list[tuple[str, str, str | None]]:
    """The declarations of `name`, following object-like aliases (the header
    may declare the symbol under its macro spelling) and function-like macros
    (the symbol is the macro's first-identifier target)."""
    current = name
    for _ in range(5):
        found = table["decls"].get(current)
        if found:
            return found
        aliases = table["value_to_names"].get(current, [])
        for alias in aliases:
            found = table["decls"].get(alias)
            if found:
                return found
        if not aliases and current in table["fn_macros"]:
            first = re.match(r"[A-Za-z_]\w*", table["fn_macros"][current])
            if first is None:
                break
            current = first.group(0)
            continue
        return []
    return []


def func_class(ctype: str) -> str:
    ctype = re.sub(r"_Py_NO_RETURN", "", ctype).strip()
    if ctype == "void":
        return "void"
    if ctype.endswith("*"):
        return "ptr"
    if ctype == "PyStatus":
        return "status"
    if ctype == "double":
        return "f64"
    if ctype == "float":
        return "f32"
    return "int"


# ------------------------------------------------------------- Rust emission

FUNC_RETURNS = {
    "void": ("()", None),
    "ptr": ("*mut c_void", "core::ptr::null_mut()"),
    "status": (
        "PyStatusStub",
        "PyStatusStub {\n"
        "        _type: 1,\n"
        "        func: core::ptr::null(),\n"
        "        err_msg: core::ptr::null(),\n"
        "        exitcode: 0,\n"
        "    }",
    ),
    "f64": ("f64", "-1.0"),
    "f32": ("f32", "-1.0"),
    "int": ("isize", "-1"),
}

# C scalar types to the Rust type with the same size and signedness; c_long
# and friends track the platform's C ABI (LLP64 on Windows, LP64 on Unix).
SCALAR_RUST = {
    "int": "i32",
    "unsigned int": "u32",
    "long": "c_long",
    "unsigned long": "c_ulong",
    "const unsigned long": "c_ulong",
    "short": "i16",
    "unsigned short": "u16",
    "char": "c_char",
    "unsigned char": "u8",
    "const unsigned char": "u8",
    "const unsigned int": "u32",
    "wchar_t": "wchar_t",
    "Py_ssize_t": "isize",
    "size_t": "usize",
    "Py_hash_t": "usize",
    "Py_uhash_t": "usize",
    "int64_t": "i64",
    "uint64_t": "u64",
    "int32_t": "i32",
    "uint32_t": "u32",
    "float": "f32",
    "double": "f64",
}

# Data types that are pointers in C.
POINTER_RUST = {
    "PyObject *": "*mut c_void",
    "PyThreadState*": "*mut c_void",
    "const char *": "*const c_char",
    "const char * const": "*const c_char",
    "const struct _frozen *": "*const c_void",
    "struct _inittab *": "*const c_void",
}

# Opaque struct types with no public definition or layout coil-rt can mirror;
# nothing coil-rt runs dereferences them.
OPAQUE_RUST = {"_PyRuntimeState": "[u8; 0]", "_Py_HashSecret_t": "[u8; 0]"}

FNPTR_ARG_RUST = {"FILE *": "*mut c_void", "const char *": "*const c_char"}
FNPTR_RET_RUST = {"int": "i32", "char *": "*mut c_char"}


def func_stub(name: str, rtype: str) -> str:
    ret, value = FUNC_RETURNS[func_class(rtype)]
    body = f'    raise("{name}");\n'
    if value is not None:
        body += f"    {value}\n"
    signature = f'pub extern "C" fn {name}()'
    if ret != "()":
        signature += f" -> {ret}"
    return (
        f"#[unsafe(no_mangle)]\n"
        f"{signature} {{\n"
        f"{body}}}\n"
    )


def data_stub(name: str, ctype: str, arr: str | None) -> str:
    if arr is not None:
        size = int(arr) if arr else 0
        element = SCALAR_RUST.get(ctype)
        if element is not None:
            if size == 0:
                return (
                    f"#[unsafe(no_mangle)]\n"
                    f"pub static mut {name}: [{element}; 0] = [];\n"
                )
            return (
                f"#[unsafe(no_mangle)]\n"
                f"pub static mut {name}: [{element}; {size}] = [0{element}; {size}];\n"
            )
        element = POINTER_RUST.get(ctype)
        if element is not None:
            if size != 0:
                raise SystemExit(f"non-empty pointer array for {name}: {ctype!r}")
            return (
                f"#[unsafe(no_mangle)]\n"
                f"pub static mut {name}: [{element}; 0] = [];\n"
            )
        raise SystemExit(f"no array element type for {name}: {ctype!r}")
    if ctype == "PyTypeObject":
        return (
            f"#[unsafe(no_mangle)]\n"
            f"pub static mut {name}: PyTypeObject = {{\n"
            f"    // SAFETY: zero is a valid bit pattern for PyTypeObject.\n"
            f"    unsafe {{ core::mem::zeroed() }}\n"
            f"}};\n"
        )
    if ctype in POINTER_RUST:
        rust = POINTER_RUST[ctype]
        null = "core::ptr::null_mut()" if rust.startswith("*mut") else "core::ptr::null()"
        return (
            f"#[unsafe(no_mangle)]\n"
            f"pub static mut {name}: {rust} = {null};\n"
        )
    if ctype in OPAQUE_RUST:
        return (
            f"#[unsafe(no_mangle)]\n"
            f"pub static mut {name}: [u8; 0] = [];\n"
        )
    if ctype in SCALAR_RUST:
        return (
            f"#[unsafe(no_mangle)]\n"
            f"pub static mut {name}: {SCALAR_RUST[ctype]} = 0;\n"
        )
    raise SystemExit(f"no data type mapping for {name}: {ctype!r}")


def fnptr_stub(name: str, rtype: str, args: str) -> str:
    ret = FNPTR_RET_RUST.get(rtype.strip())
    if ret is None:
        raise SystemExit(f"no fnptr return type for {name}: {rtype!r}")
    arg_types = []
    for arg in args.split(","):
        arg = arg.strip()
        if not arg or arg == "void":
            continue
        mapped = FNPTR_ARG_RUST.get(arg)
        if mapped is None:
            raise SystemExit(f"no fnptr argument type for {name}: {arg!r}")
        arg_types.append(mapped)
    sig = ", ".join(arg_types)
    line = f"pub static mut {name}: Option<unsafe extern \"C\" fn({sig}) -> {ret}> = None;"
    if len(line) <= 100:
        return f"#[unsafe(no_mangle)]\n{line}\n"
    return (
        f"#[unsafe(no_mangle)]\n"
        f"pub static mut {name}: Option<\n"
        f"    unsafe extern \"C\" fn({sig}) -> {ret},\n"
        f"> = None;\n"
    )


HEADER = """\
//! Raising stubs for the C API entries coil-rt does not implement yet
//! (spec section 10.1): the DLL exports every CPython 3.12.10 symbol, and
//! each unimplemented function raises `SystemError("coil: <name> is not
//! implemented")`, naming the importing module when one is being imported.
//! Data entries are zero-initialized placeholders.
//!
//! Generated by `tools/gen_capi_stubs.py` from the pinned CPython headers;
//! do not edit by hand.

#![allow(non_snake_case)]

use coil_capi_sys::PyTypeObject;

/// `PyStatus`'s 3.12 layout; `_type == 1` is `_PyStatus_TYPE_ERROR`.
#[repr(C)]
pub struct PyStatusStub {
    _type: i32,
    func: *const c_void,
    err_msg: *const c_void,
    exitcode: i32,
}

fn raise(name: &str) {
    let mut message = format!("coil: {name} is not implemented");
    if let Some(module) = crate::tstate::calling_module() {
        message.push_str(" (calling module: ");
        message.push_str(&module);
        message.push(')');
    }
    crate::tstate::set_string(crate::types::exc::system_error(), message.as_bytes());
}
"""


def render(funcs: list[tuple[str, str]], data: list[str]) -> str:
    body = ["// --- functions ---\n"]
    for name, rtype in sorted(funcs):
        body.append(func_stub(name, rtype))
    body.append("\n// --- data ---\n")
    for stub in sorted(data):
        body.append(stub)
    text = "".join(body)
    aliases = [
        a
        for a in ("c_char", "c_long", "c_ulong", "c_void", "wchar_t")
        if re.search(rf"\b{a}\b", text)
    ]
    if len(aliases) == 1:
        use_line = f"use core::ffi::{aliases[0]};\n"
    else:
        use_line = f"use core::ffi::{{{', '.join(aliases)}}};\n"
    return HEADER + use_line + text


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--build-dir", type=Path, default=ROOT / "build/capi_stubs")
    parser.add_argument("--check", action="store_true",
                        help="diff against the checked-in file instead of writing")
    args = parser.parse_args()

    exports: dict[str, str] = {}
    for line in EXPORTS.read_text().splitlines():
        name, kind = line.split()
        exports[name] = kind

    implemented = implemented_names()
    missing = {n: k for n, k in exports.items() if n not in implemented}

    build_dir = args.build_dir.resolve()
    build_dir.mkdir(parents=True, exist_ok=True)
    headers = prepare_headers(CPYTHON_VERSION, build_dir)
    table = load_declarations(headers)

    funcs: list[tuple[str, str]] = []
    data: list[str] = []
    for name, kind in sorted(missing.items()):
        decls = resolve(name, table)
        if not decls:
            raise SystemExit(f"no declaration for {name} in the pinned headers")
        kinds = {d[0] for d in decls}
        if "datafunc" in kinds:
            kinds = {"func"}
        if "fnptr" in kinds:
            kinds = {"data"}
        expected = {"func"} if kind == "func" else {"data"}
        if kinds != expected:
            raise SystemExit(f"kind mismatch for {name}: exports.txt {kind!r}, headers {kinds}")
        if kind == "func":
            rtypes = {d[1] for d in decls if d[0] in ("func", "datafunc")}
            if len(rtypes) != 1:
                raise SystemExit(f"conflicting return types for {name}: {rtypes}")
            funcs.append((name, next(iter(rtypes))))
        elif "fnptr" in {d[0] for d in decls}:
            rtype, fargs = next(d[1:] for d in decls if d[0] == "fnptr")
            data.append(fnptr_stub(name, rtype, fargs))
        else:
            types = {(d[1], d[2]) for d in decls if d[0] == "data"}
            if len(types) != 1:
                raise SystemExit(f"conflicting data types for {name}: {types}")
            ctype, arr = next(iter(types))
            data.append(data_stub(name, ctype, arr))

    content = render(funcs, data)
    if args.check:
        current = GENERATED.read_text()
        if current != content:
            sys.stdout.writelines(
                difflib.unified_diff(
                    current.splitlines(keepends=True),
                    content.splitlines(keepends=True),
                    fromfile=f"a/{GENERATED.relative_to(ROOT)}",
                    tofile=f"b/{GENERATED.relative_to(ROOT)}",
                )
            )
            raise SystemExit(f"{GENERATED.relative_to(ROOT)} is out of date; rerun without --check")
        print(f"ok: {GENERATED.relative_to(ROOT)} matches ({len(funcs)} functions, {len(data)} data)")
        return
    GENERATED.write_text(content)
    print(f"wrote {GENERATED.relative_to(ROOT)} ({len(funcs)} functions, {len(data)} data)")


if __name__ == "__main__":
    main()

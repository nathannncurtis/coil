"""Generate the coil-front AST types from the pinned Parser/Python.asdl.

Parses the pinned CPython source tree's Parser/Python.asdl (obtained via
tools/fetch_cpython_src.py --print-root, which downloads and verifies the
tarball into ~/.coil/cache on first use) with a small hand-written ASDL
parser and writes native/crates/coil-front/src/ast_generated.rs: one Rust
type per ASDL type, sum types as enums, product types as structs, ``*`` as
Vec, ``?`` as Option, attributes appended to every node that has them. The
generated file is checked in; ``--check`` regenerates in memory and exits
non-zero when the checked-in copy is stale.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

TOOLS = Path(__file__).resolve().parent
sys.path.insert(0, str(TOOLS))
from fetch_cpython_src import VERSION  # noqa: E402

REPO_ROOT = TOOLS.parent
OUTPUT = REPO_ROOT / "native" / "crates" / "coil-front" / "src" / "ast_generated.rs"

# ASDL's four builtin types mapped to their Rust representation: identifiers
# and strings are interned Syms (both may carry any code point sequence),
# int fits every ASDL use, constant is the Constant enum in ast.rs.
BUILTIN_TYPES = {"identifier": "Sym", "string": "Sym", "int": "i32", "constant": "Constant"}

# CPython's ASDL declares these slots expr*, but a missing element is stored
# as null: a ** unpacking in a dict display (ast.Dict(keys=[None], ...)) and a
# keyword-only argument without a default (ast.arguments(kw_defaults=[None, ...])).
# Keys are (owner, field): the constructor name for sum types, the type name
# for product types.
FIELD_TYPE_OVERRIDES = {
    ("Dict", "keys"): "Vec<Option<Expr>>",
    ("arguments", "kw_defaults"): "Vec<Option<Expr>>",
}

# Rust keywords that appear as ASDL field names; emitted as raw identifiers.
RUST_KEYWORDS = frozenset({"type", "ref", "in", "for", "if", "box"})


def rust_type_name(asdl_name: str) -> str:
    return "".join(part.capitalize() for part in asdl_name.split("_"))


@dataclass
class Field:
    name: str
    type: str
    seq: bool = False
    opt: bool = False


@dataclass
class Constructor:
    name: str
    fields: list[Field]


@dataclass
class TypeDef:
    name: str
    constructors: list[Constructor]
    is_product: bool
    attributes: list[Field] = field(default_factory=list)


TOKEN = re.compile(r"[A-Za-z_][A-Za-z0-9_]*|[(){}=|,*?]")


def tokenize(text: str) -> list[str]:
    tokens: list[str] = []
    for line in text.splitlines():
        tokens.extend(TOKEN.findall(line.split("--", 1)[0]))
    return tokens


class Asdl:
    """Parser for the ASDL grammar Parser/Python.asdl is written in."""

    def __init__(self, tokens: list[str]) -> None:
        self.tokens = tokens
        self.pos = 0

    def peek(self) -> str:
        return self.tokens[self.pos]

    def take(self) -> str:
        token = self.tokens[self.pos]
        self.pos += 1
        return token

    def expect(self, token: str) -> None:
        if self.take() != token:
            context = self.tokens[max(0, self.pos - 3):self.pos + 3]
            raise ValueError(f"expected {token!r} near {context}")

    def parse(self) -> list[TypeDef]:
        self.expect("module")
        self.take()
        self.expect("{")
        types: list[TypeDef] = []
        while self.peek() != "}":
            types.append(self.type_def())
            if self.peek() == "attributes":
                self.take()
                types[-1].attributes = self.field_list()
        self.expect("}")
        if self.pos != len(self.tokens):
            raise ValueError(f"trailing tokens: {self.tokens[self.pos:]}")
        return types

    def type_def(self) -> TypeDef:
        name = self.take()
        self.expect("=")
        if self.peek() == "(":
            constructor = Constructor(name, self.field_list())
            return TypeDef(name=name, constructors=[constructor], is_product=True)
        constructors = [self.constructor()]
        while self.peek() == "|":
            self.take()
            constructors.append(self.constructor())
        return TypeDef(name=name, constructors=constructors, is_product=False)

    def field_list(self) -> list[Field]:
        self.expect("(")
        fields: list[Field] = []
        while self.peek() != ")":
            fields.append(self.field())
            if self.peek() == ",":
                self.take()
        self.expect(")")
        return fields

    def field(self) -> Field:
        # field ::= TYPE [ '*' | '?' ] NAME, as in `stmt* body` or `expr? returns`.
        ftype = self.take()
        seq = opt = False
        if self.peek() == "*":
            self.take()
            seq = True
        elif self.peek() == "?":
            self.take()
            opt = True
        return Field(name=self.take(), type=ftype, seq=seq, opt=opt)

    def constructor(self) -> Constructor:
        name = self.take()
        fields = self.field_list() if self.peek() == "(" else []
        return Constructor(name=name, fields=fields)


def field_type(owner: str, field: Field, node_types: set[str]) -> str:
    override = FIELD_TYPE_OVERRIDES.get((owner, field.name))
    if override is not None:
        return override
    if field.type in node_types:
        # A singular field of an AST node type is boxed to keep node sizes down.
        rust_name = rust_type_name(field.type)
        return f"Vec<{rust_name}>" if field.seq else (
            f"Option<Box<{rust_name}>>" if field.opt else f"Box<{rust_name}>"
        )
    base = BUILTIN_TYPES[field.type]
    return f"Vec<{base}>" if field.seq else (f"Option<{base}>" if field.opt else base)


def field_name(field: Field) -> str:
    return f"r#{field.name}" if field.name in RUST_KEYWORDS else field.name


def generate(types: list[TypeDef]) -> str:
    node_types = {typedef.name for typedef in types}
    for typedef in types:
        fields = typedef.attributes + [f for c in typedef.constructors for f in c.fields]
        for member in fields:
            if member.type not in BUILTIN_TYPES and member.type not in node_types:
                raise ValueError(f"unknown field type {member.type} in {typedef.name}.{member.name}")
    header = [
        f"// Generated by tools/gen_ast.py from Parser/Python.asdl (CPython {VERSION}). Do not edit:",
        "// rerun the generator instead.",
        "",
        "use crate::ast::Constant;",
        "use crate::intern::Sym;",
    ]
    lines: list[str] = list(header)
    for typedef in types:
        lines.append("")
        # rustfmt::skip on each item: the generator owns this file's layout.
        lines.append("#[rustfmt::skip]")
        lines.append("#[derive(Clone, Debug)]")
        if typedef.is_product:
            lines.append(f"pub struct {rust_type_name(typedef.name)} {{")
            for member in typedef.constructors[0].fields + typedef.attributes:
                lines.append(f"    pub {field_name(member)}: {field_type(typedef.name, member, node_types)},")
            lines.append("}")
        else:
            lines.append(f"pub enum {rust_type_name(typedef.name)} {{")
            for constructor in typedef.constructors:
                members = constructor.fields + typedef.attributes
                if not members:
                    lines.append(f"    {constructor.name},")
                    continue
                lines.append(f"    {constructor.name} {{")
                for member in members:
                    lines.append(f"        {field_name(member)}: {field_type(constructor.name, member, node_types)},")
                lines.append("    },")
            lines.append("}")
    return "\n".join(lines) + "\n"


def generated_source() -> str:
    result = subprocess.run(
        [sys.executable, str(TOOLS / "fetch_cpython_src.py"), "--print-root"],
        capture_output=True,
        text=True,
        check=True,
    )
    asdl = (Path(result.stdout.strip()) / "Parser" / "Python.asdl").read_text(encoding="utf-8")
    return generate(Asdl(tokenize(asdl)).parse())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true",
                        help="fail if the checked-in output is stale")
    args = parser.parse_args(argv)
    source = generated_source()
    if args.check:
        current = OUTPUT.read_text(encoding="utf-8") if OUTPUT.is_file() else ""
        if current != source:
            print(f"{OUTPUT} is stale; run tools/gen_ast.py to regenerate it", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(source, encoding="utf-8", newline="\n")
    print(f"Wrote {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

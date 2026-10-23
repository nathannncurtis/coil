"""Extract the parser-level SyntaxError cases CPython's own tests assert.

Walks the pinned CPython test modules and pulls out every snippet whose
SyntaxError the tests assert, in three forms:

* doctest examples whose expected output ends in a SyntaxError-family line
  (all docstrings, plus module-level strings, which is how the __test__
  doctests are published);
* calls to the tests' error-checking helpers, with local aliases resolved
  (``check = self.check_syntax_error``) and loops expanded over constant
  iterables so parameterized snippets come out one per value;
* ``assertRaises(SyntaxError, eval, "...")`` calls and
  ``with self.assertRaises[Regex](SyntaxError, ...):`` blocks holding a
  literal compile/eval/exec call, including through local helper functions
  whose parameters the callers bind to constants.

Each case is then run through ast.parse -- the parser boundary: the errors
of the tokenizer and the PEG parser, before symtable.c and compile.c -- and
kept when that raises SyntaxError, IndentationError or TabError.  Cases
asserted only under ``warnings.simplefilter('error', SyntaxWarning)`` do not
raise at the parser boundary and are counted, not kept.  The assertions from
the tests (exact messages, regexes, positions, subclass) are verified
against the interpreter's actual error, so an interpreter whose messages
drifted from the pinned 3.12.10 test files fails loudly here.

The output is the JSON Lines corpus of tools/front_oracle.py --snippets: one
object per case with id, src, mode, encoding and the oracle's expected error
record.  Only snippets that raise at the parser boundary are written.
"""

from __future__ import annotations

import argparse
import ast
import dataclasses
import doctest
import inspect
import json
import re
import subprocess
import sys
import warnings
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import front_oracle  # noqa: E402

ERROR_TYPES = ("SyntaxError", "IndentationError", "TabError")
ERROR_LINE = re.compile(r"^(SyntaxError|IndentationError|TabError):\s?(.*)$")
COMPILE_FUNCS = ("compile", "eval", "exec")

# The test modules the task names, relative to Lib/test.
TEST_FILES = [
    "test_syntax.py",
    "test_exceptions.py",
    "test_fstring.py",
    "test_grammar.py",
    "test_patma.py",
    "test_type_params.py",
    "test_positional_only_arg.py",
    "test_unpack_ex.py",
    "test_named_expressions.py",
    "test_genexps.py",
    "test_string_literals.py",
    "test_eof.py",
    "test_unicode_identifiers.py",
]

# The test modules' error-checking helpers, by name, each with the extractor
# that reads a call's snippet and assertions.  Reached as a method call
# (``self._check_error(...)``) or through a local alias
# (``check = self.check``).


@dataclasses.dataclass
class Case:
    id: str
    src: str
    mode: str = "exec"
    encoding: str | None = None
    exact: str | None = None
    regex: str | None = None
    lineno: int | None = None
    offset: int | None = None
    end_lineno: int | None = None
    end_offset: int | None = None
    subclass: str | None = None

    def key(self) -> tuple:
        return (self.src, self.mode, self.encoding)


def const(node: ast.AST) -> object | None:
    """A constant value, folding + and * of constants (the tests build
    snippets like '"' + '(' * 500 + '"')."""
    if isinstance(node, ast.BinOp) and isinstance(node.op, (ast.Add, ast.Mult)):
        left, right = const(node.left), const(node.right)
        if left is not None and right is not None:
            try:
                if isinstance(node.op, ast.Add):
                    return left + right  # type: ignore[operator]
                return left * right  # type: ignore[operator]
            except Exception:
                return None
    if isinstance(node, ast.Constant) and isinstance(node.value, (str, bytes, int)):
        return node.value
    return None


def const_str(node: ast.AST) -> str | None:
    value = const(node)
    return value if isinstance(value, str) else None


def cleandoc_str(node: ast.AST) -> str | None:
    text = const_str(node)
    return None if text is None else inspect.cleandoc(text)


def compile_mode(func_name: str, call: ast.Call | None) -> str:
    if func_name == "eval":
        return "eval"
    if func_name == "exec":
        return "exec"
    if call is not None and len(call.args) >= 3:
        mode = const(call.args[2])
        if isinstance(mode, str):
            return mode
    return "exec"


# -- the visitor -----------------------------------------------------------------


class Visitor(ast.NodeVisitor):
    """Collects cases from one test module."""

    def __init__(self, filename: str):
        self.filename = filename
        self.cases: list[Case] = []
        self.module_consts: dict[str, object] = {}
        self.scopes: list[dict[str, object]] = [{}]
        self._ids: dict[str, int] = {}

    # -- bookkeeping ------------------------------------------------------------

    def case_id(self, node: ast.AST) -> str:
        base = f"{self.filename}:{getattr(node, 'lineno', 0)}"
        seen = self._ids.get(base, 0)
        self._ids[base] = seen + 1
        return base if seen == 0 else f"{base}#{seen}"

    def bind(self, name: str, value: object) -> None:
        self.scopes[-1][name] = value

    def lookup(self, name: str) -> object | None:
        for scope in reversed(self.scopes):
            if name in scope:
                return scope[name]
        return None

    # -- scope entries ------------------------------------------------------------

    def visit_Module(self, node: ast.Module) -> None:
        for stmt in node.body:
            if isinstance(stmt, (ast.Assign, ast.AnnAssign)):
                target = stmt.targets[0] if isinstance(stmt, ast.Assign) else stmt.target
                if isinstance(target, ast.Name) and isinstance(stmt.value, ast.Constant):
                    self.module_consts[target.id] = stmt.value.value
                elif isinstance(target, ast.Name) \
                        and isinstance(stmt.value, (ast.List, ast.Tuple)):
                    values = [const(item) for item in stmt.value.elts]
                    if values and all(value is not None for value in values):
                        self.module_consts[target.id] = values
        self.generic_visit(node)

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        if node.module == "test.support":
            for alias in node.names:
                if alias.name == "check_syntax_error":
                    self.bind(alias.asname or alias.name, ("helper", "check_syntax_error"))
        elif node.module == "codecs":
            for alias in node.names:
                if alias.name == "BOM_UTF8":
                    self.bind(alias.asname or alias.name, ("bom",))
        self.generic_visit(node)

    def visit_Assign(self, node: ast.Assign) -> None:
        if isinstance(node.targets[0], ast.Name):
            self.bind_assignment(node.targets[0].id, node.value)
        self.generic_visit(node)

    def visit_AnnAssign(self, node: ast.AnnAssign) -> None:
        if isinstance(node.target, ast.Name) and node.value is not None:
            self.bind_assignment(node.target.id, node.value)
        self.generic_visit(node)

    def bind_assignment(self, name: str, value: ast.AST) -> None:
        # Constants are tracked for snippets built in a variable first
        # (``code = "x := 0"`` ... ``exec(code)``); helper references win.
        resolved = self.resolve_helper_value(value)
        if resolved is not None:
            self.bind(name, resolved)
        elif isinstance(value, ast.Constant) and isinstance(value.value, (str, bytes)):
            self.bind(name, ("const", value.value))

    def resolve_helper_value(self, value: ast.AST) -> object | None:
        if isinstance(value, ast.Attribute):
            if value.attr in HELPER_EXTRACTORS:
                # self.<helper> is bound: no testcase argument in front.
                return ("bound_helper", value.attr)
        if isinstance(value, ast.Name) and (found := self.lookup(value.id)):
            return found
        return None

    def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
        # Bind the def's name in the enclosing scope first: Python makes it
        # visible to the whole scope, and the callers here may nest the calls
        # before the def line.  A def whose name is a known helper whose
        # extractor already knows its signature (test_exceptions defines its
        # ckmsg locally) dispatches to that extractor, not to the generic
        # local-def walk.
        if node.name in LOCAL_DEF_HELPERS:
            self.bind(node.name, ("helper", node.name))
        else:
            self.bind(node.name, ("local_def", node))
        self.scopes.append({})
        self.generic_visit(node)
        self.scopes.pop()

    visit_AsyncFunctionDef = visit_FunctionDef

    def visit_Lambda(self, node: ast.Lambda) -> None:
        self.scopes.append({})
        self.generic_visit(node)
        self.scopes.pop()

    # -- loops ---------------------------------------------------------------------

    def visit_For(self, node: ast.For) -> None:
        values = self.loop_values(node)
        if values is None:
            self.generic_visit(node)
            return
        for value in values:
            self.scopes.append({node.target.id: ("const", value)})
            for stmt in node.body:
                self.visit(stmt)
            self.scopes.pop()
        for stmt in node.orelse:
            self.visit(stmt)

    def loop_values(self, node: ast.For) -> list | None:
        if not isinstance(node.target, ast.Name):
            return None
        if isinstance(node.iter, (ast.Tuple, ast.List)):
            values = [const(item) for item in node.iter.elts]
        elif isinstance(node.iter, ast.Constant) and isinstance(node.iter.value, str):
            # `for paren in "([{"` iterates the characters.
            values = list(node.iter.value)
        elif isinstance(node.iter, ast.Name) and isinstance(
            self.module_consts.get(node.iter.id), (list, tuple)
        ):
            values = list(self.module_consts[node.iter.id])
        else:
            return None
        if any(value is None or isinstance(value, bytes) for value in values):
            return None
        return values

    # -- calls and with blocks -------------------------------------------------------

    def visit_Call(self, node: ast.Call) -> None:
        if isinstance(node.func, ast.Name) \
                and isinstance(resolved := self.lookup(node.func.id), tuple):
            if resolved[0] == "helper":
                self.add(HELPER_EXTRACTORS[resolved[1]](self, node))
                return
            if resolved[0] == "bound_helper":
                # An alias of a bound method: its arguments start at the
                # statement, without the testcase parameter.
                extractor = BOUND_HELPER_EXTRACTORS.get(
                    resolved[1], HELPER_EXTRACTORS[resolved[1]]
                )
                self.add(extractor(self, node))
                return
            if resolved[0] == "local_def":
                self.add(helper_local_def(self, node, resolved[1]))
                return
        if isinstance(node.func, ast.Attribute):
            if node.func.attr in BOUND_HELPER_EXTRACTORS:
                # self.<helper>(...) is the bound form.
                self.add(BOUND_HELPER_EXTRACTORS[node.func.attr](self, node))
                return
            if node.func.attr in HELPER_EXTRACTORS:
                self.add(HELPER_EXTRACTORS[node.func.attr](self, node))
                return
            if node.func.attr in ("assertRaises", "assertRaisesRegex"):
                self.add(helper_direct_assert_raises(self, node))
                return
        self.generic_visit(node)

    def visit_With(self, node: ast.With) -> None:
        for item in node.items:
            call = item.context_expr
            if not (isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)):
                continue
            if call.func.attr not in ("assertRaises", "assertRaisesRegex"):
                continue
            if not (call.args and isinstance(call.args[0], ast.Name)
                    and call.args[0].id in ERROR_TYPES):
                continue
            regex = None
            if call.func.attr == "assertRaisesRegex" and len(call.args) >= 2:
                regex = const_str(call.args[1])
            self.add(with_block_cases(self, node, regex))
        self.generic_visit(node)

    def add(self, cases: list[Case]) -> None:
        self.cases.extend(cases)


# -- helper extractions ---------------------------------------------------------


def helper_check_syntax_error(v: Visitor, call: ast.Call, first_arg: int = 1) -> list[Case]:
    # test.support.check_syntax_error(testcase, statement[, errtext],
    #                                 *, lineno, offset).  The bound
    # self.check_syntax_error call has no testcase in front.
    if len(call.args) <= first_arg:
        return []
    if (resolved := source_of(v, call.args[first_arg])) is None:
        return []
    case = Case(v.case_id(call), resolved[0], encoding=resolved[1])
    if len(call.args) > first_arg + 1:
        case.regex = const_str(call.args[first_arg + 1])
    for kw in call.keywords:
        if kw.arg in ("lineno", "offset") and isinstance(kw.value, ast.Constant):
            setattr(case, kw.arg, kw.value.value)
    return [case]


def helper_check_syntax_error_bound(v: Visitor, call: ast.Call) -> list[Case]:
    return helper_check_syntax_error(v, call, first_arg=0)


def helper_check_error_stmt(v: Visitor, call: ast.Call) -> list[Case]:
    # test_syntax.SyntaxTestCase._check_error(code, errtext, filename=,
    #     mode=, subclass=, lineno=, offset=, end_lineno=, end_offset=)
    if not call.args:
        return []
    if (resolved := source_of(v, call.args[0])) is None:
        return []
    case = Case(v.case_id(call), resolved[0], encoding=resolved[1])
    if len(call.args) >= 2 and (text := source_of(v, call.args[1])) is not None:
        case.regex = text[0]
    for kw in call.keywords:
        if not isinstance(kw.value, ast.Constant):
            continue
        if kw.arg == "mode":
            case.mode = kw.value.value
        elif kw.arg in ("lineno", "offset", "end_lineno", "end_offset"):
            setattr(case, kw.arg, kw.value.value)
    for kw in call.keywords:
        if kw.arg == "subclass" and isinstance(kw.value, ast.Name):
            case.subclass = kw.value.id
    return [case]


def helper_check_positions(v: Visitor, call: ast.Call) -> list[Case]:
    # test_exceptions.SyntaxTestCase.check(src, lineno, offset,
    #     end_lineno=None, end_offset=None, encoding='utf-8')
    if len(call.args) < 3:
        return []
    encoding = None
    for kw in call.keywords:
        if kw.arg == "encoding" and isinstance(kw.value, ast.Constant):
            encoding = kw.value.value
    if isinstance(call.args[0], ast.Constant) and isinstance(call.args[0].value, bytes):
        try:
            src = call.args[0].value.decode(encoding or "utf-8")
        except UnicodeDecodeError:
            return []
    elif (src := const_str(call.args[0])) is not None:
        pass
    else:
        return []
    case = Case(v.case_id(call), src, encoding=encoding)
    values = [const(arg) for arg in call.args[1:]]
    for name, value in zip(("lineno", "offset", "end_lineno", "end_offset"), values):
        if isinstance(value, int) and not isinstance(value, bool):
            setattr(case, name, value)
    return [case]


def helper_assert_syntax_error(v: Visitor, call: ast.Call) -> list[Case]:
    # test_patma.TestSyntaxErrors.assert_syntax_error(code): the method call's
    # arguments exclude self, and the code goes through inspect.cleandoc.
    if not call.args:
        return []
    src = cleandoc_str(call.args[0])
    if src is None:
        return []
    return [Case(v.case_id(call), src)]


def helper_ckmsg(v: Visitor, call: ast.Call) -> list[Case]:
    # test_exceptions' local ckmsg(src, msg[, exception=])
    if len(call.args) < 2:
        return []
    if (resolved := source_of(v, call.args[0])) is None:
        return []
    case = Case(v.case_id(call), resolved[0], encoding=resolved[1],
                exact=const_str(call.args[1]))
    for kw in call.keywords:
        if kw.arg == "exception" and isinstance(kw.value, ast.Name):
            if kw.value.id not in ERROR_TYPES:
                return []
            case.subclass = kw.value.id
    return [case]


def helper_assert_all_raise(v: Visitor, call: ast.Call) -> list[Case]:
    # test_fstring.TestCase.assertAllRaise(SyntaxError, regex, [strings])
    if len(call.args) < 3:
        return []
    if not (isinstance(call.args[0], ast.Name) and call.args[0].id in ERROR_TYPES):
        return []
    if not isinstance(call.args[2], (ast.List, ast.Tuple)):
        return []
    regex = const_str(call.args[1])
    cases = []
    for item in call.args[2].elts:
        if (src := const_str(item)) is not None:
            case = Case(v.case_id(call), src, mode="eval")
            case.regex = regex
            cases.append(case)
    return cases


def helper_direct_assert_raises(v: Visitor, call: ast.Call) -> list[Case]:
    # self.assertRaises(SyntaxError, eval, "src")
    if len(call.args) < 3:
        return []
    if not (isinstance(call.args[0], ast.Name) and call.args[0].id in ERROR_TYPES):
        return []
    if not (isinstance(call.args[1], ast.Name) and call.args[1].id in COMPILE_FUNCS):
        return []
    case = snippet_case(v, call.args[1].id, call.args[2], call, None)
    return [case] if case else []


def helper_local_def(v: Visitor, call: ast.Call, definition: ast.FunctionDef) -> list[Case]:
    """A call to a local helper def: bind its parameters to the caller's
    constants and extract what its body asserts with them."""
    bindings: dict[str, object] = {}
    params = [arg.arg for arg in definition.args.args]
    for param, arg in zip(params, call.args):
        if (value := const(arg)) is not None or (value := _source_const(v, arg)) is not None:
            bindings[param] = value
    for kw in call.keywords:
        if kw.arg and ((value := const(kw.value)) is not None
                       or (value := _source_const(v, kw.value)) is not None):
            bindings[kw.arg] = value
    sub = Visitor(v.filename)
    sub.scopes = [dict(scope) for scope in v.scopes]
    # A def that calls itself (the test sources have some) must not expand
    # into itself.
    sub.scopes.append({definition.name: None})
    sub.module_consts = v.module_consts
    sub._ids = v._ids
    _current_bindings.clear()
    _current_bindings.update({name: value for scope in v.scopes
                              for name, value in scope.items()})
    try:
        for stmt in substitute(definition, bindings):
            sub.visit(stmt)
    finally:
        _current_bindings.clear()
    return sub.cases


# The visible constant bindings while a helper def's body is substituted;
# a module global so the recursive _copy can see them.
_current_bindings: dict[str, object] = {}


HELPER_EXTRACTORS = {
    "check_syntax_error": helper_check_syntax_error,
    "_check_error": helper_check_error_stmt,
    "check": helper_check_positions,
    "assert_syntax_error": helper_assert_syntax_error,
    "ckmsg": helper_ckmsg,
    "assertAllRaise": helper_assert_all_raise,
}

# The helpers whose bound form shifts the arguments: self.check_syntax_error
# drops the leading testcase parameter.  Every other helper takes the source
# as its first argument in both forms.
BOUND_HELPER_EXTRACTORS = {
    "check_syntax_error": helper_check_syntax_error_bound,
}

# Helper names that the test modules also define locally, where the generic
# local-def walk cannot see the assertion shape (ckmsg's compile call sits in
# a bare try block, not under assertRaises).
LOCAL_DEF_HELPERS = {"ckmsg"}


def with_block_cases(v: Visitor, node: ast.With, regex: str | None) -> list[Case]:
    """Snippets from the compile/eval/exec calls inside an assertRaises with
    block."""
    cases = []
    for inner in ast.walk(ast.Module(body=node.body, type_ignores=[])):
        if not (isinstance(inner, ast.Call) and isinstance(inner.func, ast.Name)
                and inner.func.id in COMPILE_FUNCS and inner.args):
            continue
        if case := snippet_case(v, inner.func.id, inner.args[0], inner, regex):
            cases.append(case)
    return cases


def snippet_case(
    v: Visitor, func_name: str, src_node: ast.AST, at: ast.AST, regex: str | None,
) -> Case | None:
    """A case from a compile/eval/exec call's source argument: a text or
    bytes constant, possibly built up, .encode()d or BOM-prefixed."""
    if (resolved := source_of(v, src_node)) is None:
        return None
    src, encoding = resolved
    case = Case(v.case_id(at), src, mode=compile_mode(func_name, None))
    case.encoding = encoding
    case.regex = regex
    return case


def source_of(v: Visitor, node: ast.AST) -> tuple[str, str | None] | None:
    """A snippet source as (text, explicit encoding).  Handles the shapes the
    tests build: constants, constant folding, f-strings over bound loop
    variables, .encode() calls and a leading BOM_UTF8."""
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        left = source_of(v, node.left)
        right = source_of(v, node.right)
        if left is None or right is None:
            return None
        (lsrc, lenc), (rsrc, renc) = left, right
        if lenc is not None and renc is not None and lenc != renc:
            return None
        return (lsrc + rsrc, lenc or renc)
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) \
            and node.func.attr == "encode" \
            and (text := const_str(node.func.value)) is not None:
        encoding = "utf-8"
        if node.args and (name := const_str(node.args[0])):
            encoding = name
        return (text, encoding)
    if isinstance(node, ast.Name) and v.lookup(node.id) == ("bom",):
        return ("\ufeff", None)
    if isinstance(node, ast.JoinedStr):
        parts = []
        for value in node.values:
            if isinstance(value, ast.Constant):
                parts.append(value.value)
            elif isinstance(value, ast.FormattedValue) and not value.format_spec \
                    and value.conversion == -1 \
                    and (inner := source_of(v, value.value)) is not None:
                parts.append(inner[0])
            else:
                return None
        return ("".join(parts), None)
    if isinstance(node, ast.Name) \
            and isinstance(found := v.lookup(node.id), tuple) \
            and found[0] == "const":
        if isinstance(found[1], str):
            return (found[1], None)
        if isinstance(found[1], bytes):
            # Not every bytes source is UTF-8: test_syntax builds a latin-1
            # snippet with an explicit `# coding=latin` line.  latin-1 maps
            # every byte, so the round trip through the corpus is exact.
            try:
                return (found[1].decode("utf-8"), "utf-8")
            except UnicodeDecodeError:
                return (found[1].decode("latin-1"), "latin-1")
    if (text := const_str(node)) is not None:
        return (text, None)
    return None


def _source_const(v: Visitor, node: ast.AST) -> object | None:
    """A snippet-shaped argument resolved to its text (f-strings over bound
    constants, encoded or BOM-prefixed builds)."""
    if isinstance(node, ast.Constant) and isinstance(node.value, bytes):
        return node.value
    if (resolved := source_of(v, node)) is not None:
        return resolved[0]
    return None


def substitute(definition: ast.FunctionDef, bindings: dict[str, object]) -> list[ast.stmt]:
    """The def's body with bound parameter names replaced by constants, so
    generic extraction sees literal snippets."""
    copied = ast.Module(
        body=[_copy(stmt, bindings) for stmt in definition.body], type_ignores=[]
    )
    ast.fix_missing_locations(copied)
    return copied.body


def _copy(node, bindings: dict[str, object]):
    if isinstance(node, ast.Name):
        if node.id in bindings:
            return ast.Constant(value=bindings[node.id])
        if isinstance(bound := _current_bindings.get(node.id), tuple) \
                and bound[0] == "const":
            return ast.Constant(value=bound[1])
    if isinstance(node, ast.JoinedStr):
        parts = []
        for value in node.values:
            if isinstance(value, ast.FormattedValue) \
                    and isinstance(value.value, ast.Name) \
                    and value.value.id in bindings and not value.format_spec \
                    and value.conversion == -1:
                parts.append(ast.Constant(value=str(bindings[value.value.id])))
            else:
                parts.append(_copy(value, bindings))
        return ast.JoinedStr(values=parts)
    if isinstance(node, ast.AST):
        changes = {}
        for field, value in ast.iter_fields(node):
            if isinstance(value, ast.AST):
                changes[field] = _copy(value, bindings)
            elif isinstance(value, list):
                changes[field] = [
                    _copy(item, bindings) if isinstance(item, ast.AST) else item
                    for item in value
                ]
        return type(node)(**{**node.__dict__, **changes})
    return node


# -- doctests ---------------------------------------------------------------------


def doctest_cases(v: Visitor, text: str, at: ast.AST) -> list[Case]:
    """The doctest examples whose expected output ends in a SyntaxError."""
    cases = []
    try:
        examples = doctest.DocTestParser().get_examples(text)
    except Exception:
        return []
    for example in examples:
        lines = [line for line in example.want.splitlines() if line.strip()]
        if not lines or not (match := ERROR_LINE.match(lines[-1])):
            continue
        case = Case(v.case_id(at), example.source.rstrip("\n"))
        case.exact = match.group(2)
        cases.append(case)
    return cases


def doctest_entry_points(tree: ast.AST) -> list[ast.AST]:
    """Every docstring, plus every module-level string constant (the __test__
    doctests live in plain module strings)."""
    nodes = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.ClassDef, ast.FunctionDef,
                             ast.AsyncFunctionDef)):
            doc = ast.get_docstring(node, clean=False)
            if doc is not None:
                nodes.append(node)
        elif isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant) \
                and isinstance(node.value.value, str) \
                and isinstance(node.scopes_target if False else _assign_target(node), ast.Name):
            nodes.append(node)
    return nodes


def _assign_target(node: ast.Assign) -> ast.AST:
    return node.targets[0]


# -- evaluation against the oracle ---------------------------------------------------


def oracle_record(case: Case) -> dict:
    with warnings.catch_warnings():
        warnings.simplefilter("ignore", SyntaxWarning)
        return front_oracle.snippet_record({
            "id": case.id, "src": case.src, "mode": case.mode,
            "encoding": case.encoding,
        })


def verify(case: Case, error: dict) -> str | None:
    """Why the oracle's error fails the test's assertion, or None."""
    message = error["msg"]
    if case.exact is not None and message != case.exact:
        return f"exact message {case.exact!r} != {message!r}"
    if case.regex is not None and not re.search(case.regex, message):
        if not re.search(case.regex, f"{message} (<string>, line {error['lineno']})"):
            return f"regex {case.regex!r} not in {message!r}"
    for field in ("lineno", "offset", "end_lineno", "end_offset"):
        expected = getattr(case, field)
        if expected is not None and error[field] != expected:
            return f"{field} {error[field]} != {expected}"
    if case.subclass is not None and error["type"] != case.subclass:
        return f"type {error['type']} != {case.subclass}"
    return None


def evaluate(cases: list[Case]) -> tuple[list[tuple[Case, dict]], dict[str, int]]:
    """Run every case through the oracle; keep the SyntaxError-family errors
    whose assertions hold."""
    kept: list[tuple[Case, dict]] = []
    counts: dict[str, int] = {}
    for case in cases:
        record = oracle_record(case)
        if record["ok"]:
            counts["no parser error"] = counts.get("no parser error", 0) + 1
            continue
        error = record["error"]
        if error["type"] not in ERROR_TYPES:
            counts[f"other error: {error['type']}"] = \
                counts.get(f"other error: {error['type']}", 0) + 1
            continue
        if (reason := verify(case, error)) is not None:
            raise SystemExit(
                f"{case.id}: assertion does not hold: {reason}\n"
                f"  src: {case.src!r}"
            )
        counts["kept"] = counts.get("kept", 0) + 1
        kept.append((case, error))
    return kept, counts


# -- driver ---------------------------------------------------------------------------


def module_cases(test_dir: Path, filename: str) -> tuple[list[Case], dict[str, int]]:
    text = (test_dir / filename).read_text(encoding="utf-8")
    tree = ast.parse(text)
    visitor = Visitor(filename)
    for node in ast.walk(tree):
        if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef)):
            if (doc := ast.get_docstring(node, clean=False)) is not None:
                visitor.add(doctest_cases(visitor, doc, node))
        elif isinstance(node, ast.Module):
            if (doc := ast.get_docstring(node, clean=False)) is not None:
                visitor.add(doctest_cases(visitor, doc, node))
        elif isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant) \
                and isinstance(node.value.value, str) \
                and isinstance(node.targets[0], ast.Name):
            visitor.add(doctest_cases(visitor, node.value.value, node))
    visitor.visit(tree)
    if filename == "test_unicode_identifiers.py":
        visitor.add(unicode_identifier_cases(test_dir, visitor))
    kept, counts = evaluate(visitor.cases)
    return kept, counts


def unicode_identifier_cases(test_dir: Path, v: Visitor) -> list[Case]:
    # test_unicode_identifiers.test_invalid imports
    # test.tokenizedata.badsyntax_3131 and asserts its parse error.
    path = test_dir / "tokenizedata" / "badsyntax_3131.py"
    case = Case(f"{v.filename}:{path.name}", path.read_bytes().decode("utf-8"))
    case.encoding = "utf-8"
    case.exact = "invalid character '€' (U+20AC)"
    case.lineno = 2
    case.offset = 1
    return [case]


def cpython_root(explicit: str | None) -> Path:
    if explicit:
        return Path(explicit)
    root = Path(subprocess.run(
        [sys.executable, str(Path(__file__).with_name("fetch_cpython_src.py")),
         "--print-root"],
        check=True, capture_output=True, text=True,
    ).stdout.strip())
    if not root.is_dir():
        root = Path(home) / ".coil" / "cache" / "stdlib-src" / "3.12.10"
    return root


home = str(Path.home())


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cpython", metavar="DIR",
                        help="the pinned CPython source tree (default: fetched)")
    parser.add_argument("--out", metavar="FILE",
                        default="native/crates/coil-front/tests/syntax_cases.jsonl",
                        help="corpus output path")
    args = parser.parse_args(argv)
    test_dir = cpython_root(args.cpython) / "Lib" / "test"
    all_kept: list[tuple[Case, dict]] = []
    totals: dict[str, int] = {}
    for filename in TEST_FILES:
        kept, counts = module_cases(test_dir, filename)
        all_kept.extend(kept)
        dropped = sum(count for name, count in counts.items() if name != "kept")
        print(f"{filename}: {dropped} dropped, {counts.get('kept', 0)} kept: "
              + ", ".join(f"{k}={v}" for k, v in sorted(counts.items()) if k != "kept"))
        for name, count in counts.items():
            totals[name] = totals.get(name, 0) + count
    seen: set[tuple] = set()
    written: list[tuple[Case, dict]] = []
    for case, error in sorted(all_kept, key=lambda item: item[0].id):
        if case.key() in seen:
            totals["duplicate snippets"] = totals.get("duplicate snippets", 0) + 1
            continue
        seen.add(case.key())
        written.append((case, error))
    out = Path(args.out)
    with out.open("w", encoding="utf-8") as handle:
        for case, error in written:
            record = {"id": case.id, "src": case.src}
            if case.mode != "exec":
                record["mode"] = case.mode
            if case.encoding is not None:
                record["encoding"] = case.encoding
            record["expected"] = {field: error[field] for field in
                                  ("type", "msg", "lineno", "offset",
                                   "end_lineno", "end_offset")}
            handle.write(json.dumps(record, ensure_ascii=True, separators=(",", ":")) + "\n")
    dropped = sum(count for name, count in totals.items() if name != "kept")
    print(f"\ntotal: {dropped} dropped, {totals.get('kept', 0)} kept, "
          f"{len(written)} written to {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

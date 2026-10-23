//! End-to-end lowering tests: source text through parse, symtable and
//! `lower_module`. `lower_module` verifies every function it produces, so an
//! `Ok` here means the IR also passed `verify`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use coil_front::lower::{LowerKind, LowerOptions, LowerOutput, lower_module};
use coil_front::parser::parse_module;
use coil_front::symtable;
use coil_ir::ir::{Const, Function, Instr, LocalKind, Params, Terminator};
use coil_ir::types::Value;

fn lower_opt(src: &str, optimize: u8) -> Result<LowerOutput, String> {
    let (module, mut interner) =
        parse_module(src.as_bytes(), Some(Path::new("test.py"))).map_err(|e| e.message)?;
    let table = symtable::symtable(&module, &mut interner).map_err(|e| e.message)?;
    lower_module(
        "test",
        PathBuf::from("test.py"),
        &module,
        src.as_bytes(),
        &interner,
        &table,
        LowerOptions { optimize },
    )
    .map_err(|e| e.to_string())
}

fn lower(src: &str) -> LowerOutput {
    lower_opt(src, 0).expect("lowering failed")
}

fn fn_by_qualname<'a>(out: &'a LowerOutput, name: &str) -> &'a Function {
    out.module
        .functions
        .iter()
        .find(|f| out.module.consts.sym_str(f.qualname) == name)
        .unwrap_or_else(|| panic!("no function named {name}"))
}

fn instrs(f: &Function) -> impl Iterator<Item = &Instr> {
    f.blocks.iter().flat_map(|b| b.instrs.iter())
}

fn has_instr(f: &Function, pred: impl Fn(&Instr) -> bool) -> bool {
    instrs(f).any(pred)
}

fn local_kind(f: &Function, out: &LowerOutput, name: &str) -> LocalKind {
    f.locals
        .iter()
        .find(|m| out.module.consts.sym_str(m.name) == name)
        .map(|m| m.kind)
        .unwrap_or_else(|| {
            panic!(
                "no local named {name} in {}",
                out.module.consts.sym_str(f.qualname)
            )
        })
}

fn sym_of(out: &LowerOutput, name: &str) -> coil_ir::types::Sym {
    out.module
        .consts
        .strings
        .iter()
        .position(|s| s == name)
        .map(|i| coil_ir::types::Sym(i as u32))
        .unwrap_or_else(|| panic!("no string {name} in the pool"))
}

#[test]
fn module_function_and_call() {
    let out = lower(
        r#"
def add(a, b):
    return a + b
x = add(1, 2)
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let names: Vec<&str> = out
        .module
        .functions
        .iter()
        .map(|f| out.module.consts.sym_str(f.qualname))
        .collect();
    assert_eq!(names, vec!["<module>", "add"]);
    let add = fn_by_qualname(&out, "add");
    assert_eq!(add.params.arity(), 2);
    assert!(has_instr(add, |i| matches!(i, Instr::BinOp { .. })));
    assert!(has_instr(fn_by_qualname(&out, "<module>"), |i| matches!(
        i,
        Instr::Call { .. }
    )));
}

#[test]
fn parameter_kinds_fill_params() {
    let out = lower("def f(a, b=1, /, c=2, *d, e=3, **g):\n    return (a, b, c, d, e, g)\n");
    let f = fn_by_qualname(&out, "f");
    assert_eq!(
        f.params,
        Params {
            positional: vec![sym_of(&out, "a"), sym_of(&out, "b"), sym_of(&out, "c")],
            vararg: Some(sym_of(&out, "d")),
            kwonly: vec![sym_of(&out, "e")],
            kwarg: Some(sym_of(&out, "g")),
        }
    );
    assert_eq!(f.params.arity(), 6);
}

#[test]
fn lambda_parameters_and_defaults() {
    let out = lower("f = lambda a, b=1, *, c: a + b + c\n");
    let l = fn_by_qualname(&out, "<lambda>");
    assert_eq!(l.params.positional.len(), 2);
    assert_eq!(l.params.kwonly.len(), 1);
    assert!(has_instr(fn_by_qualname(&out, "<module>"), |i| matches!(
        i,
        Instr::MakeFunction { .. }
    )));
}

#[test]
fn closures_pass_cells_and_load_closure() {
    let out = lower(
        r#"
def outer():
    x = 1
    def inner():
        return x
    return inner
"#,
    );
    let outer = fn_by_qualname(&out, "outer");
    let inner = fn_by_qualname(&out, "outer.<locals>.inner");
    assert_eq!(local_kind(outer, &out, "x"), LocalKind::Cell);
    assert_eq!(local_kind(inner, &out, "x"), LocalKind::Free);
    assert!(has_instr(inner, |i| matches!(
        i,
        Instr::LoadClosure { index: 0, .. }
    )));
    assert!(has_instr(outer, |i| matches!(
        i,
        Instr::MakeFunction { cells, .. } if cells.len() == 1
    )));
}

#[test]
fn class_local_shadowing_a_cell_is_routed_through_the_class() {
    // A class-body local that shadows an enclosing function's cell and is
    // read by a method: CPython passes the name through the class body's
    // closure (DEF_FREE_CLASS), so the class body carries a Free local and
    // a LoadClosure, and the method closes over the class body in turn.
    let out = lower(
        r#"
def f():
    method_and_var = "outer"
    class C:
        method_and_var = "class_var"
        def method(self):
            return method_and_var
    return C
"#,
    );
    let f = fn_by_qualname(&out, "f");
    assert_eq!(local_kind(f, &out, "method_and_var"), LocalKind::Cell);
    let body = fn_by_qualname(&out, "f.<locals>.C");
    assert_eq!(local_kind(body, &out, "method_and_var"), LocalKind::Free);
    assert!(has_instr(body, |i| matches!(i, Instr::LoadClosure { .. })));
    let m = fn_by_qualname(&out, "f.<locals>.C.method");
    assert_eq!(local_kind(m, &out, "method_and_var"), LocalKind::Free);
    assert!(has_instr(m, |i| matches!(i, Instr::LoadClosure { .. })));
}

#[test]
fn implicit_class_cell() {
    let out = lower(
        r#"
class C:
    def m(self):
        return __class__
"#,
    );
    let body = fn_by_qualname(&out, "C");
    assert_eq!(local_kind(body, &out, "__class__"), LocalKind::Cell);
    let m = fn_by_qualname(&out, "C.m");
    assert_eq!(local_kind(m, &out, "__class__"), LocalKind::Free);
    assert!(has_instr(m, |i| matches!(i, Instr::LoadClosure { .. })));
}

#[test]
fn zero_arg_super_uses_the_class_cell() {
    let out = lower(
        r#"
class Base:
    def m(self):
        return 1
class C(Base):
    def m(self):
        return super().m()
"#,
    );
    let m = fn_by_qualname(&out, "C.m");
    let base = sym_of(&out, "super");
    assert!(has_instr(m, |i| matches!(
        i,
        Instr::LoadGlobal { name, .. } if *name == base
    )));
    assert!(has_instr(m, |i| matches!(i, Instr::Call { .. })));
}

#[test]
fn for_loop_handler_shape() {
    let out = lower(
        r#"
def f(xs):
    for x in xs:
        g(x)
    else:
        return 0
    return 1
"#,
    );
    let f = fn_by_qualname(&out, "f");
    assert!(has_instr(f, |i| matches!(i, Instr::GetIter { .. })));
    assert!(has_instr(f, |i| matches!(i, Instr::Next { .. })));
    // The StopIteration handler: CatchBegin tests, the loop exits through it.
    assert!(has_instr(f, |i| matches!(i, Instr::CatchBegin { .. })));
    assert!(has_instr(f, |i| matches!(i, Instr::CatchEnd)));
    // Every block with a may-raise instruction carries a handler (verify
    // re-checks this); the loop body must resolve its own landing block.
    assert!(f.blocks.iter().any(|b| b.handler.is_some()));
}

#[test]
fn while_else_break_continue() {
    lower(
        r#"
def f(n):
    while n > 0:
        n -= 1
        if n == 2:
            break
        if n == 1:
            continue
    else:
        return 0
    return n
"#,
    );
}

#[test]
fn boolop_and_chained_compare_form_phis() {
    let out = lower("def f(a, b, c):\n    return a < b < c and b or c\n");
    let f = fn_by_qualname(&out, "f");
    assert!(f.blocks.iter().any(|b| !b.phis.is_empty()));
}

#[test]
fn star_calls_displays_and_subscripts() {
    lower(
        r#"
def f(a, b, *rest, k=1, **kw):
    xs = [a, *rest]
    ys = {a: b, **kw}
    zs = (*rest,)
    s = {a, *rest}
    t = a[b, *rest]
    return g(*xs, **ys)
"#,
    );
}

#[test]
fn slices_in_and_out_of_subscripts() {
    lower(
        r#"
def f(xs, a, b):
    return xs[1:2:3], xs[:a], xs[a:b, ::-1], xs[..., 0]
"#,
    );
}

#[test]
fn walrus_in_nested_loop_conditions() {
    // Regression: the trivial-phi replacement used to leave a stale operand
    // when a phi's users were rewritten in a nested loop with a walrus in
    // both conditions and an attribute read in the latch; verify now
    // catches a missing definition if the aliases map is not updated.
    lower(
        r#"
def encode(input, output):
    while s := input.read(57):
        while len(s) < 57 and (ns := input.read(57-len(s))):
            s += ns
        output.write(s)
"#,
    );
}

#[test]
fn class_definition_with_decorator_and_methods() {
    let out = lower(
        r#"
def deco(c):
    return c

@deco
class Base:
    "doc"
    x = 1
    def m(self):
        return type(self)
"#,
    );
    let names: Vec<&str> = out
        .module
        .classes
        .iter()
        .map(|c| out.module.consts.sym_str(c.name))
        .collect();
    assert_eq!(names, vec!["Base"]);
    let cls = &out.module.classes[0];
    let body = &out.module.functions[cls.body.0 as usize];
    assert_eq!(out.module.consts.sym_str(body.qualname), "Base");
    let method_names: Vec<&str> = cls
        .methods
        .iter()
        .map(|&(s, _)| out.module.consts.sym_str(s))
        .collect();
    assert_eq!(method_names, vec!["m"]);
}

#[test]
fn nested_classes_get_nested_quals() {
    let out = lower(
        r#"
class A:
    class B:
        pass
"#,
    );
    fn_by_qualname(&out, "A");
    fn_by_qualname(&out, "A.B");
}

#[test]
fn generic_function_and_class() {
    let out = lower(
        r#"
def f[T](x: T) -> T:
    return x

class C[T: int](Base):
    def m(self, y: T) -> T:
        return y
"#,
    );
    fn_by_qualname(&out, "<generic parameters of f>");
    fn_by_qualname(&out, "f");
    fn_by_qualname(&out, "<generic parameters of C>");
    fn_by_qualname(&out, "C");
    fn_by_qualname(&out, "C.m");
}

#[test]
fn type_alias_statements() {
    let out = lower(
        r#"
type Alias = int
type Generic[T] = list[T]
"#,
    );
    fn_by_qualname(&out, "Alias");
    fn_by_qualname(&out, "Generic");
    fn_by_qualname(&out, "<generic parameters of Generic>");
}

#[test]
fn star_class_bases_call_build_class() {
    let out = lower(
        r#"
def f(*bases, **kw):
    class C(*bases, metaclass=type, **kw):
        pass
    return C
"#,
    );
    let f = fn_by_qualname(&out, "f");
    assert!(has_instr(f, |i| matches!(i, Instr::CallEx { .. })));
    let build = sym_of(&out, "__build_class__");
    assert!(has_instr(f, |i| matches!(
        i,
        Instr::LoadGlobal { name, .. } if *name == build
    )));
}

#[test]
fn star_annotations() {
    lower("def f[T, *Ts, **P](*args: *Ts, **kwargs: P):\n    return args\n");
}

#[test]
fn del_targets_flatten() {
    lower(
        r#"
def f(x, y, z, o, xs, d):
    del x, (y, z)
    del o.attr, xs[0], d["k"]
"#,
    );
}

#[test]
fn import_forms() {
    let out = lower(
        r#"
import os
import os.path as p
from os import sep
from os.path import join as j
x = p.sep + sep
"#,
    );
    let module = fn_by_qualname(&out, "<module>");
    // `import os.path as p` walks the attribute chain from the Import
    // result; `p.sep` adds one more read.
    let attrs = instrs(module)
        .filter(|i| matches!(i, Instr::GetAttr { .. }))
        .count();
    assert!(attrs >= 2, "expected attribute walks, got {attrs}");
}

#[test]
fn global_and_nonlocal_stores() {
    lower(
        r#"
x = 1
def outer():
    y = 2
    def inner():
        global x
        nonlocal y
        x = x + 1
        y = y + 1
    inner()
    return y
"#,
    );
}

#[test]
fn assert_follows_optimize_level() {
    let src = "def f(x):\n    assert x > 0\n    return x\n";
    let kept = lower_opt(src, 0).expect("optimize 0 lowers");
    let assert_err = sym_of(&kept, "AssertionError");
    assert!(has_instr(fn_by_qualname(&kept, "f"), |i| matches!(
        i,
        Instr::LoadGlobal { name, .. } if *name == assert_err
    )));
    let dropped = lower_opt(src, 1).expect("optimize 1 lowers");
    assert!(!has_instr(fn_by_qualname(&dropped, "f"), |i| matches!(
        i,
        Instr::LoadGlobal { name, .. } if *name == assert_err
    )));
}

#[test]
fn docstring_follows_optimize_level() {
    let src = "def f():\n    'doc'\n    return 1\n";
    let kept = lower_opt(src, 0).expect("optimize 0 lowers");
    assert!(kept.module.consts.strings.iter().any(|s| s == "__doc__"));
    let dropped = lower_opt(src, 2).expect("optimize 2 lowers");
    assert!(!dropped.module.consts.strings.iter().any(|s| s == "__doc__"));
}

#[test]
fn future_annotations_stringify() {
    let out = lower(
        "from __future__ import annotations\ndef f(x: Undefined) -> Missing:\n    return x\n",
    );
    // The annotations become string constants, not name lookups.
    let module = fn_by_qualname(&out, "<module>");
    let undef = sym_of(&out, "Undefined");
    assert!(!has_instr(module, |i| matches!(
        i,
        Instr::LoadGlobal { name, .. } if *name == undef
    )));
    assert!(out.module.consts.strings.iter().any(|s| s == "Undefined"));
}

#[test]
fn unreachable_read_after_break_resolves() {
    // The `return x` block has no predecessors; its read of `x` still
    // needs a definition that dominates the exit edge.
    lower(
        r#"
def f(xs):
    for x in xs:
        break
        return x
    return 0
"#,
    );
}

#[test]
fn raise_forms() {
    lower(
        r#"
def f(x):
    if x:
        raise ValueError(x) from None
    raise
"#,
    );
}

#[test]
fn assignment_targets() {
    lower(
        r#"
def f(a, xs):
    x = y = 0
    x, *y = xs
    xs[0] = a
    xs.attr = a
    (x, y), z = xs, 1
    x += 1
    x //= 2
"#,
    );
}

#[test]
fn fstrings_and_consts() {
    lower(
        r#"
def f(name, v):
    s = f"{name!r} {v} {name:{v}}"
    t = (1, "a", b"b", 1.5, True, None, ..., ())
    return s is t
"#,
    );
}

#[test]
fn unsupported_statements_report_kinds_and_keep_the_rest() {
    let src = r#"
def a():
    async def inner():
        pass

def e():
    yield 1

def g2(s):
    return [i for i in s]

def kept():
    return 7
"#;
    let out = lower(src);
    let kinds: HashSet<&str> = out.unsupported.iter().map(|k| k.name()).collect();
    assert_eq!(
        kinds,
        HashSet::from(["async function", "yield", "comprehension"])
    );
    // The rest of the module still lowered and verified.
    fn_by_qualname(&out, "kept");
}

#[test]
fn unsupported_expressions_report_their_kind() {
    // Unsupported constructs in droppable positions are dropped and named
    // in the output; the module and the rest of the function still verify.
    let cases: &[(&str, LowerKind)] = &[
        (
            "def f(s):\n    return (x for x in s)\n",
            LowerKind::GeneratorExp,
        ),
        (
            "def f(s):\n    return {x for x in s}\n",
            LowerKind::Comprehension,
        ),
        (
            "def f(s):\n    return {x: 1 for x in s}\n",
            LowerKind::Comprehension,
        ),
        ("def f(s):\n    return await s\n", LowerKind::Await),
        (
            "def f(s):\n    return x if s else (yield)\n",
            LowerKind::Yield,
        ),
    ];
    for &(src, kind) in cases {
        let out = lower(src);
        assert!(
            out.unsupported.contains(&kind),
            "{:?} missing from {:?} for {src}",
            kind,
            out.unsupported
        );
    }
}

#[test]
fn lambda_body_with_unsupported_expression_still_lowers_rest() {
    // A lambda body cannot be dropped without dropping the lambda, so the
    // lambda itself is dropped and the module still verifies.
    let out = lower("f = lambda s: [x for x in s]\ng = 1\n");
    assert!(
        out.unsupported
            .iter()
            .any(|k| matches!(k, LowerKind::Comprehension))
    );
    let names: Vec<&str> = out
        .module
        .functions
        .iter()
        .map(|f| out.module.consts.sym_str(f.qualname))
        .collect();
    assert_eq!(names, vec!["<module>"]);
}

#[test]
fn terminated_loops_and_returns() {
    lower(
        r#"
def f():
    while True:
        break
    return 1
"#,
    );
    lower(
        r#"
def g(x):
    if x:
        return 1
    return 2
"#,
    );
}

#[test]
fn returns_of_every_form_verify() {
    lower(
        r#"
def a():
    return
def b():
    return 1
def c(x):
    return x
"#,
    );
}

#[test]
fn conditional_expressions_and_boolops_verify() {
    lower(
        r#"
def f(a, b):
    x = a if a else b
    y = 1 if x else 2
    z = a and b or y
    w = not z
    return (x, y, z, w, ~a, +a, -a)
"#,
    );
}

#[test]
fn compare_operators_verify() {
    lower(
        r#"
def f(a, b):
    return (a is b, a is not b, a in b, a not in b, a == b, a != b,
            a < b, a <= b, a > b, a >= b)
"#,
    );
}

#[test]
fn every_terminator_reaches_a_return() {
    let out = lower(
        r#"
def f(x):
    if x:
        return 1
    while x:
        x -= 1
    return 0
"#,
    );
    let f = fn_by_qualname(&out, "f");
    assert!(
        f.blocks
            .iter()
            .all(|b| !matches!(b.term, Terminator::Unreachable))
    );
}

#[test]
fn import_as_walks_only_the_trailing_components() {
    // `import a.b as c` yields the top module `a` from the Import; the
    // asname walks only the components after the first (IMPORT_FROM b).
    let out = lower(
        r#"
import os as o
import os.path as p
"#,
    );
    let module = fn_by_qualname(&out, "<module>");
    let attrs: Vec<&coil_ir::types::Sym> = instrs(module)
        .filter_map(|i| match i {
            Instr::GetAttr { name, .. } => Some(name),
            _ => None,
        })
        .collect();
    assert_eq!(attrs.len(), 1, "one hop for os.path, none for os");
    assert_eq!(*attrs[0], sym_of(&out, "path"));
}

#[test]
fn decorator_and_default_scopes_bind_in_symtable_order() {
    // The symtable visits the defaults' scopes before the decorators',
    // while the code generator evaluates the decorators first; both a
    // scoped decorator and a scoped default must still bind their own
    // scopes.
    let out = lower(
        r#"
x = 1
y = 2
def deco(f):
    return f
@deco(lambda a: x)
def f(a=lambda: y):
    return a
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    fn_by_qualname(&out, "f");
    // Both lambdas lower in the module scope, one in the decorator and one
    // in the defaults, each bound to its own scope.
    let lambdas = out
        .module
        .functions
        .iter()
        .filter(|f| out.module.consts.sym_str(f.qualname) == "<lambda>")
        .count();
    assert_eq!(lambdas, 2);
}

#[test]
fn lambda_defaults_precede_the_lambda_scope() {
    // A default's scope precedes the lambda's own scope in symtable order.
    let out = lower(
        r#"
def g():
    y = 5
    h = lambda a=(lambda: y): a
    return h
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    fn_by_qualname(&out, "g.<locals>.<lambda>");
}

#[test]
fn dropped_generic_def_keeps_the_child_cursor() {
    // A TypeVar bound's scopes nest inside the type-parameters scope, so
    // dropping the def advances the cursor by exactly one slot.
    let out = lower(
        r#"
def f[T: (i for i in g())](x):
    pass
def h():
    return 1
"#,
    );
    assert!(
        out.unsupported
            .iter()
            .any(|k| matches!(k, LowerKind::GeneratorExp))
    );
    fn_by_qualname(&out, "h");
}

#[test]
fn zero_arg_super_needs_a_positional_parameter() {
    // CPython rewrites super() only when the enclosing method has a
    // positional first parameter; otherwise the zero-arg call stands and
    // raises RuntimeError at runtime.
    let out = lower(
        r#"
class C:
    def m(*args):
        return super().m()
    def n(self):
        return super().m()
"#,
    );
    let m = fn_by_qualname(&out, "C.m");
    let zero_arg = instrs(m).any(|i| match i {
        Instr::Call { args, .. } => args.is_empty(),
        _ => false,
    });
    assert!(zero_arg, "super() must stay a zero-arg call in C.m");
    let n = fn_by_qualname(&out, "C.n");
    let rewritten = instrs(n).any(|i| match i {
        Instr::Call { args, .. } => args.len() == 2,
        _ => false,
    });
    assert!(
        rewritten,
        "super() must become super(__class__, self) in C.n"
    );
}

#[test]
fn chained_comparison_evaluates_each_operand_once() {
    // The middle operand of a < b < c feeds both comparisons; CPython
    // copies it rather than re-evaluating it.
    let out = lower("def f():\n    return 0 < g() < 2\n");
    let f = fn_by_qualname(&out, "f");
    let calls = instrs(f)
        .filter(|i| matches!(i, Instr::Call { .. }))
        .count();
    assert_eq!(calls, 1, "g() must be called exactly once");
}

#[test]
fn class_type_params_mangle_to_the_class_name() {
    // The symtable records `__T` in a class as `_C__T`; the type-parameters
    // scope must write that cell, which the class body and methods read.
    let out = lower(
        r#"
class C[__T]:
    x: __T = 1
    def m(self):
        return __T
"#,
    );
    let tp = fn_by_qualname(&out, "<generic parameters of C>");
    assert_eq!(local_kind(tp, &out, "_C__T"), LocalKind::Cell);
    assert!(has_instr(tp, |i| matches!(i, Instr::StoreCell { .. })));
    let body = fn_by_qualname(&out, "C");
    assert_eq!(local_kind(body, &out, "_C__T"), LocalKind::Free);
    let m = fn_by_qualname(&out, "C.m");
    assert_eq!(local_kind(m, &out, "_C__T"), LocalKind::Free);
}

#[test]
fn try_except_binds_and_cleans_the_name() {
    let out = lower(
        r#"
def f(s):
    try:
        g(s)
    except (ValueError, KeyError) as e:
        h(e)
    except:
        g(2)
    else:
        g(3)
    return s
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    assert_eq!(
        instrs(f)
            .filter(|i| matches!(i, Instr::CatchBegin { .. }))
            .count(),
        2
    );
    assert!(has_instr(f, |i| matches!(i, Instr::CatchEnd)));
    assert!(has_instr(f, |i| matches!(i, Instr::PopExcInfo)));
    // The no-match tail re-raises the in-flight exception.
    assert!(
        f.blocks
            .iter()
            .any(|b| matches!(b.term, Terminator::Reraise))
    );
}

#[test]
fn try_star_splits_and_recombines() {
    let out = lower(
        r#"
def f(s):
    try:
        g(s)
    except* ValueError as e:
        h(e)
    except* (KeyError, IndexError):
        pass
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    assert_eq!(
        instrs(f)
            .filter(|i| matches!(i, Instr::CatchStarBegin { .. }))
            .count(),
        2
    );
    assert!(has_instr(f, |i| matches!(i, Instr::PrepReraiseStar { .. })));
    // Every handler body's own exceptions land in a BaseException catch
    // that appends to the outcome list.
    assert_eq!(
        instrs(f)
            .filter(|i| matches!(i, Instr::CatchBegin { .. }))
            .count(),
        2
    );
}

#[test]
fn finally_dispatches_every_reason() {
    let out = lower(
        r#"
def f(s):
    try:
        try:
            if s:
                return 1
            g(s)
        finally:
            cleanup()
    except ValueError:
        return 2
    return 3

def loop(s):
    while s:
        try:
            if s:
                break
            continue
        finally:
            cleanup()
    return 4

def raising(s):
    try:
        g(s)
    finally:
        raise KeyError(s)
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    assert!(has_instr(f, |i| matches!(i, Instr::FinallyEnter { .. })));
    assert!(has_instr(f, |i| matches!(i, Instr::FinallyExit { .. })));
    // The return value rides to the dispatch in a one-tuple.
    assert!(has_instr(f, |i| matches!(i, Instr::Unpack { n: 1, .. })));
    fn_by_qualname(&out, "loop");
    fn_by_qualname(&out, "raising");
}

#[test]
fn with_items_call_enter_and_exit() {
    let out = lower(
        r#"
def f(m, n):
    with m as a, n as b:
        g(a, b)
    return a
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    let attrs: Vec<&str> = instrs(f)
        .filter_map(|i| match i {
            Instr::GetAttr { name, .. } => Some(out.module.consts.sym_str(*name)),
            _ => None,
        })
        .collect();
    assert!(attrs.contains(&"__enter__"));
    assert!(attrs.contains(&"__exit__"));
    // Suppression tests the exit result; failure re-raises.
    assert!(
        f.blocks
            .iter()
            .any(|b| matches!(b.term, Terminator::Reraise))
    );
}

#[test]
fn match_pattern_kinds_verify() {
    let out = lower(
        r#"
def f(x, pairs):
    match x:
        case 1 | 2 if x:
            y = 0
        case [1, *rest]:
            y = 1
        case {"k": v, **others}:
            y = 2
        case C(a, k=b) as w:
            y = 3
        case str() | bytes():
            y = 4
        case _:
            y = 5
    return y, rest, others, w
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    // Sequence and mapping tests look up members of collections.abc.
    let names: Vec<&str> = instrs(f)
        .filter_map(|i| match i {
            Instr::GetAttr { name, .. } => Some(out.module.consts.sym_str(*name)),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"Sequence"));
    assert!(names.contains(&"Mapping"));
    // A class pattern's missing attribute fails the case instead of raising.
    assert!(has_instr(f, |i| matches!(i, Instr::CatchBegin { .. })));
}

#[test]
fn control_flow_through_finally_and_with() {
    // return/break/continue crossing both a with and a finally verify.
    lower(
        r#"
def f(m, s):
    while s:
        with m:
            try:
                if s:
                    return 1
                break
            finally:
                cleanup()
    return 0

def g(m):
    with m:
        raise ValueError(1)
"#,
    );
}

#[test]
fn with_calls_the_bound_protocol_methods() {
    // `method` is a bound-method lookup: the manager must not be passed to
    // `__enter__`/`__exit__` as an extra leading argument.
    let out = lower(
        r#"
def f(m):
    with m:
        g(1, 2)
    return 0
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    // The enter call has no arguments, the body call its own two, the
    // `type(exc)` lookup one, and every exit call exactly the protocol's
    // three. Block order is irrelevant, so compare as a multiset.
    let mut arg_counts: Vec<usize> = instrs(f)
        .filter_map(|i| match i {
            Instr::Call { args, .. } => Some(args.len()),
            _ => None,
        })
        .collect();
    arg_counts.sort_unstable();
    assert_eq!(arg_counts, vec![0, 1, 2, 3, 3]);
}

/// The constant a value is defined by, if any.
fn const_of<'a>(f: &'a Function, out: &'a LowerOutput, v: Value) -> Option<&'a Const> {
    instrs(f).find_map(|i| match i {
        Instr::Const { idx, into } if *into == v => Some(&out.module.consts.consts[idx.0 as usize]),
        _ => None,
    })
}

fn defined_by_phi(f: &Function, v: Value) -> bool {
    f.blocks.iter().any(|b| b.phis.iter().any(|p| p.into == v))
}

fn defined_by_pushexcinfo(f: &Function, v: Value) -> bool {
    instrs(f).any(|i| matches!(i, Instr::PushExcInfo { into } if *into == v))
}

/// The values returned from inside a handler body, i.e. the terminators of
/// blocks that end with `popexcinfo`.
fn handler_returns(f: &Function) -> Vec<Value> {
    f.blocks
        .iter()
        .filter(|b| b.instrs.iter().any(|i| matches!(i, Instr::PopExcInfo)))
        .filter_map(|b| match &b.term {
            Terminator::Return { value } => Some(*value),
            _ => None,
        })
        .collect()
}

#[test]
fn handlers_read_raise_point_locals_and_own_writes() {
    let out = lower(
        r#"
def reads(g):
    x = 1
    try:
        g()
        x = 2
        g()
    except ValueError:
        return x

def bound(g):
    try:
        g()
    except ValueError as e:
        return e

def writes(g):
    try:
        g()
    except ValueError:
        msg = "failed"
        return msg
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    // The handler sees a merge of both raise points: x is 1 at the first
    // call and 2 at the second, neither the region-entry constant nor one
    // end-of-body value.
    let f = fn_by_qualname(&out, "reads");
    let rets = handler_returns(f);
    assert_eq!(rets.len(), 1);
    let ret = rets[0];
    let mut incomings: Vec<i64> = f
        .blocks
        .iter()
        .flat_map(|b| b.phis.iter().filter(|p| p.into == ret))
        .flat_map(|p| {
            p.incomings
                .iter()
                .filter_map(|(v, _)| match const_of(f, &out, *v) {
                    Some(Const::Int(n)) => Some(*n),
                    _ => None,
                })
        })
        .collect();
    incomings.sort_unstable();
    assert_eq!(incomings, vec![1, 2]);
    assert!(defined_by_phi(f, ret));
    // The bound name reads the caught exception, and a write inside the
    // handler body survives to its own reads.
    for name in ["bound", "writes"] {
        let f = fn_by_qualname(&out, name);
        assert_eq!(handler_returns(f).len(), 1);
    }
    let bound = fn_by_qualname(&out, "bound");
    let ret = handler_returns(bound)[0];
    assert!(defined_by_pushexcinfo(bound, ret));
    let writes = fn_by_qualname(&out, "writes");
    let ret = handler_returns(writes)[0];
    assert!(
        matches!(const_of(writes, &out, ret), Some(Const::Str(s)) if out.module.consts.sym_str(*s) == "failed"),
        "handler-local write must survive to its own read, got {ret:?}"
    );
}

#[test]
fn suppressed_with_keeps_raise_point_locals() {
    let out = lower(
        r#"
def f(m):
    x = 1
    with m:
        x = 2
        raise ValueError
    return x
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    let ret = f
        .blocks
        .iter()
        .find_map(|b| match &b.term {
            Terminator::Return { value } => Some(*value),
            _ => None,
        })
        .unwrap();
    assert!(matches!(const_of(f, &out, ret), Some(Const::Int(2))));
}

#[test]
fn finally_inside_except_star_handler_compiles() {
    // CPython compiles a try/finally inside an except* handler; only control
    // flow leaving the handler itself is the error.
    let out = lower(
        r#"
def f(g, c):
    try:
        g()
    except* ValueError:
        try:
            g()
        finally:
            c()
    return 0
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let err = match lower_opt(
        r#"
def f(g):
    try:
        g()
    except* ValueError:
        return 1
"#,
        0,
    ) {
        Err(e) => e,
        Ok(_) => panic!("return in an except* block must not lower"),
    };
    assert!(
        err.to_string()
            .contains("cannot appear in an except* block"),
        "{err}"
    );
}

#[test]
fn except_star_reraise_keeps_context() {
    // CPython's tail is a bare RERAISE: no `from None`, so the prepared
    // group keeps its context and chains implicitly.
    let out = lower(
        r#"
def f(g, h):
    try:
        g()
    except* ValueError:
        h()
    return 0
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    let to_raise = instrs(f)
        .find_map(|i| match i {
            Instr::PrepReraiseStar { into, .. } => Some(*into),
            _ => None,
        })
        .expect("prepared reraise");
    let raised = f.blocks.iter().any(|b| matches!(&b.term, Terminator::Raise { exc: Some(v), cause } if *v == to_raise && cause.is_none()));
    assert!(raised, "the reraise must not set a cause");
}

#[test]
fn except_star_body_exception_keeps_the_rest() {
    // An exception in the handler body joins the outcome list, and the
    // unmatched rest still reaches the recombine: the handler body's block
    // must resolve to that landing.
    let out = lower(
        r#"
def f(g, h):
    try:
        g()
    except* ValueError:
        h()
    return 0
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    let h = Value(1);
    let body = f
        .blocks
        .iter()
        .find(|b| {
            b.instrs
                .iter()
                .any(|i| matches!(i, Instr::Call { callee, .. } if *callee == h))
        })
        .expect("handler body call");
    let hcatch = body.handler.expect("handler body is covered");
    let landing = &f.blocks[hcatch.0 as usize];
    assert!(
        landing
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::CatchBegin { .. })),
        "the landing catches the body exception"
    );
    assert!(
        landing
            .instrs
            .iter()
            .any(|i| matches!(i, Instr::Call { args, .. } if args.len() == 1)),
        "the landing appends the body exception to the outcome list"
    );
    // The rest from the split is what the final recombine appends.
    let rest = instrs(f)
        .find_map(|i| match i {
            Instr::CatchStarBegin { rest, .. } => Some(*rest),
            _ => None,
        })
        .expect("rest split");
    assert!(
        instrs(f)
            .any(|i| matches!(i, Instr::Call { args, .. } if args.len() == 1 && args[0] == rest)),
        "the unmatched rest reaches the recombine"
    );
}

#[test]
fn finalbody_abandon_restores_exc_info() {
    // `break` out of a finalbody abandons an in-flight exception; CPython's
    // FINALLY_END restores exc_info, so only the path with a pending push
    // pops it.
    let out = lower(
        r#"
def f(g):
    for i in (1, 2, 3):
        try:
            g()
        finally:
            break
    return 0
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    let guarded = f.blocks.iter().any(|b| {
        let Terminator::Branch { cond, then_to, .. } = &b.term else {
            return false;
        };
        let on_pending =
            instrs(f).any(|i| matches!(i, Instr::Truth { into, .. } if *into == *cond));
        on_pending
            && f.blocks[then_to.0 as usize]
                .instrs
                .iter()
                .any(|i| matches!(i, Instr::PopExcInfo))
    });
    assert!(guarded, "a truth-guarded PopExcInfo must restore exc_info");
}

/// True when the function raises `<exc_name>(<msg>)` where the message is a
/// `buildstr` containing a constant whose text contains `text`.
fn raises_with_message(f: &Function, out: &LowerOutput, exc_name: &str, text: &str) -> bool {
    let consts = &out.module.consts;
    let head = consts
        .consts
        .iter()
        .position(|c| matches!(c, Const::Str(s) if consts.sym_str(*s).contains(text)))
        .and_then(|i| {
            instrs(f).find_map(|ins| match ins {
                Instr::Const { idx, into } if idx.0 as usize == i => Some(*into),
                _ => None,
            })
        });
    let Some(head) = head else {
        return false;
    };
    let msg = instrs(f).find_map(|ins| match ins {
        Instr::BuildStr { parts, into } if parts.contains(&head) => Some(*into),
        _ => None,
    });
    let Some(msg) = msg else {
        return false;
    };
    let exc_of = |callee: Value| {
        instrs(f).any(|ins| match ins {
            Instr::LoadGlobal { name, into } => {
                *into == callee && consts.sym_str(*name) == exc_name
            }
            _ => false,
        })
    };
    instrs(f).any(|ins| match ins {
        Instr::Call {
            callee, args, into, ..
        } if args.first() == Some(&msg) => exc_of(*callee) && raised_is(f, *into),
        _ => false,
    })
}

/// True when `v` is the operand of a raise terminator.
fn raised_is(f: &Function, v: Value) -> bool {
    f.blocks
        .iter()
        .any(|b| matches!(&b.term, Terminator::Raise { exc: Some(e), .. } if *e == v))
}

#[test]
fn mapping_pattern_checks_duplicate_keys() {
    // Two keys that evaluate equal raise ValueError, as CPython's MATCH_KEYS
    // does; a duplicate is not a failed match.
    let out = lower(
        r#"
class K:
    a = 'x'
def f(d):
    match d:
        case {'x': p, K.a: q}:
            return p
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    let f = fn_by_qualname(&out, "f");
    assert!(
        raises_with_message(
            f,
            &out,
            "ValueError",
            "mapping pattern checks duplicate key ("
        ),
        "duplicate keys must raise ValueError with CPython's message"
    );
}

#[test]
fn class_pattern_checks_duplicate_attributes() {
    let out = lower(
        r#"
class P:
    __match_args__ = ('x', 'x')
class Q:
    __match_args__ = ('v',)
def pos_pos(o):
    match o:
        case P(a, b):
            return a
def pos_kw(o):
    match o:
        case Q(v, v=w):
            return v
"#,
    );
    assert_eq!(out.unsupported, vec![]);
    for name in ["pos_pos", "pos_kw"] {
        let f = fn_by_qualname(&out, name);
        assert!(
            raises_with_message(
                f,
                &out,
                "TypeError",
                "() got multiple sub-patterns for attribute "
            ),
            "{name} must raise TypeError for the repeated attribute"
        );
    }
}

#[test]
fn pattern_captures_bind_only_on_full_match() {
    // CPython defers the capture stores to just before the guard, so a later
    // sub-pattern that raises leaves earlier captures unbound. At module
    // scope the capture store is a real instruction, and it must come after
    // the second sub-pattern's may-raise lookup.
    let out_m = lower(
        r#"
obj = None
match [1, 2]:
    case [x, obj.v]:
        r = x
"#,
    );
    assert_eq!(out_m.unsupported, vec![]);
    let m = fn_by_qualname(&out_m, "<module>");
    let flat: Vec<&Instr> = instrs(m).collect();
    let lookup = flat
        .iter()
        .position(|i| matches!(i, Instr::GetAttr { name, .. } if out_m.module.consts.sym_str(*name) == "v"))
        .expect("obj.v lookup");
    let store_x = flat
        .iter()
        .position(|i| matches!(i, Instr::StoreName { name, .. } if out_m.module.consts.sym_str(*name) == "x"))
        .expect("capture store");
    assert!(
        lookup < store_x,
        "the capture store must follow every sub-pattern test"
    );
    let out2 = lower(
        r#"
def g(s):
    match s:
        case [x] | {"a": x}:
            return x
"#,
    );
    assert_eq!(out2.unsupported, vec![]);
    let g = fn_by_qualname(&out2, "g");
    let merged = g.blocks.iter().any(|b| match &b.term {
        Terminator::Return { value } => defined_by_phi(g, *value),
        _ => false,
    });
    assert!(merged, "the alternative captures must merge at the or-join");
    let err = match lower_opt(
        r#"
def h(s):
    match s:
        case [x] | {"a": y}:
            return x
"#,
        0,
    ) {
        Err(e) => e,
        Ok(_) => panic!("alternatives binding different names must not lower"),
    };
    assert!(err.to_string().contains("bind different names"), "{err}");
}

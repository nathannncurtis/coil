//! Lowering from the AST plus symbol table to Coil IR (spec section 4): one
//! function at a time, in CPython 3.12's codegen order so evaluation order
//! matches. Comprehensions are inlined into their enclosing scope (PEP 709)
//! with their `for` targets isolated; generator expressions become generator
//! functions whose single parameter `.0` receives the outermost iterator
//! pre-iterated. Generators, coroutines and async generators lower to the
//! resumable form of spec section 12.5: `Yield`/`YieldFrom` instructions
//! whose `state` tags the resumption points the backend switches on, with
//! locals that live across them as ordinary SSA variables.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use coil_ir::builder::{Error as BuildError, FunctionBuilder};
use coil_ir::ir::{
    BinOp, ClassDesc, CmpOp, Const, ConstPool, FnFlags, Function, Instr, LocalKind, LocalMeta,
    Module, Params, Scalar, Terminator, UnaryOp,
};
use coil_ir::types::{BlockId, ClassId, FnId, InstrId, Sym, Type, Value};
use coil_ir::verify;

use crate::ast::Constant;
use crate::ast_generated::{
    Alias, Arguments, Boolop, Cmpop, Comprehension, Excepthandler, Expr, Keyword, MatchCase,
    Operator, Pattern, Stmt, TypeParam, Unaryop, Withitem,
};
use crate::intern::{Interner, Sym as FSym};
use crate::symtable::{self, SymbolTable};

/// Optimisation level, as `--optimize`: 0 keeps `assert`, 1 drops it, 2 also
/// drops docstrings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LowerOptions {
    pub optimize: u8,
}

#[derive(Debug)]
pub enum LowerError {
    Build(BuildError),
    /// A builder error surfaced while sealing a finished function, with the
    /// function's qualname for diagnosis.
    BuildIn {
        qualname: String,
        source: BuildError,
    },
    Check {
        qualname: String,
        source: verify::Error,
    },
    /// A name the symbol table must provide (a free variable's cell) is
    /// missing: the symtable and the AST disagree.
    MissingCell {
        name: String,
        line: u32,
    },
    /// An AST shape the parser guarantees never reaches lowering.
    UnexpectedAst {
        what: &'static str,
        line: u32,
    },
    /// A compile-stage rejection, with CPython's message (e.g. `await`
    /// outside an async function).
    Rejected {
        message: String,
        line: u32,
    },
}

impl From<BuildError> for LowerError {
    fn from(e: BuildError) -> Self {
        LowerError::Build(e)
    }
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::Build(e) => write!(f, "builder: {e}"),
            LowerError::BuildIn { qualname, source } => {
                write!(f, "build in {qualname}: {source}")
            }
            LowerError::Check { qualname, source } => {
                write!(f, "verify in {qualname}: {source}")
            }
            LowerError::MissingCell { name, line } => {
                write!(f, "no cell for free variable '{name}' (line {line})")
            }
            LowerError::UnexpectedAst { what, line } => {
                write!(f, "unexpected AST shape: {what} (line {line})")
            }
            LowerError::Rejected { message, line } => {
                write!(f, "{message} (line {line})")
            }
        }
    }
}

impl std::error::Error for LowerError {}

/// Lower a module. The symbol table must be `symtable::symtable`'s result for
/// the same module; `source` is the exact bytes it was parsed from (needed
/// for `from __future__ import annotations` stringification).
pub fn lower_module(
    module_name: &str,
    file: PathBuf,
    module: &crate::ast_generated::Mod,
    source: &[u8],
    interner: &Interner,
    table: &SymbolTable,
    options: LowerOptions,
) -> Result<Module, LowerError> {
    let mut low = Lowerer {
        interner,
        names: HashMap::new(),
        consts: ConstPool::default(),
        functions: Vec::new(),
        classes: Vec::new(),
        root: table,
        optimize: options.optimize,
        future_annotations: has_future_annotations(module, interner),
        line_starts: line_starts(source),
        source,
    };
    let body_id = low.reserve_function();
    let module_sym = low.pool(module_name);
    let mut f = low.new_fn(
        table,
        FnKind::Module,
        "<module>",
        "<module>",
        ParamSpec::none(),
        None,
        None,
        (Flavor::Module, table),
    );
    let body = match module {
        crate::ast_generated::Mod::Module { body, .. } => body,
        _ => {
            return Err(LowerError::UnexpectedAst {
                what: "lowering needs a Module",
                line: 0,
            });
        }
    };
    f.lower_namespace_body(body)?;
    f.return_none()?;
    f.finish_into(body_id)?;
    let module = Module {
        name: module_sym,
        file,
        functions: low.functions,
        classes: low.classes,
        consts: low.consts,
        body: FnId(0),
    };
    // Per-function verification names the offending function in the error.
    for (i, f) in module.functions.iter().enumerate() {
        if let Err(err) = verify::verify_function(f, &module.classes) {
            return Err(LowerError::Check {
                qualname: module
                    .consts
                    .sym_str(module.functions[i].qualname)
                    .to_owned(),
                source: err,
            });
        }
    }
    verify::verify_module(&module).map_err(|source| LowerError::Check {
        qualname: "<module>".to_owned(),
        source,
    })?;
    Ok(module)
}

fn has_future_annotations(module: &crate::ast_generated::Mod, interner: &Interner) -> bool {
    fn is(interner: &Interner, s: FSym, text: &str) -> bool {
        let r = interner.resolve(s);
        r.len() == text.chars().count()
            && r.iter()
                .zip(text.chars())
                .all(|(&cp, ch)| char::from_u32(cp) == Some(ch))
    }
    match module {
        crate::ast_generated::Mod::Module { body, .. } => body.iter().any(|s| {
            matches!(
                s,
                Stmt::ImportFrom {
                    module: Some(m),
                    names,
                    ..
                } if is(interner, *m, "__future__")
                    && names.iter().any(|a| is(interner, a.name, "annotations"))
            )
        }),
        _ => false,
    }
}

fn line_starts(source: &[u8]) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in source.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    /// Children of the module scope get bare qualnames.
    Module,
    /// Children of a function-like scope are nested under `.<locals>.`.
    Function,
    /// Children of a class scope are nested under `.`.
    Class,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FnKind {
    Module,
    Function,
    Class,
    TypeParams,
    TypeAlias,
    TypeVarBound,
}

impl FnKind {
    fn function_like(self) -> bool {
        matches!(
            self,
            FnKind::Function | FnKind::TypeParams | FnKind::TypeAlias | FnKind::TypeVarBound
        )
    }

    /// Module and class bodies bind names through name operations; function
    /// scopes use SSA locals and globals.
    fn uses_name_ops(self) -> bool {
        matches!(self, FnKind::Module | FnKind::Class)
    }
}

#[derive(Clone, Copy)]
struct Pos {
    line: u32,
    col: u32,
}

fn pos_of(line: i32, col: i32) -> Pos {
    Pos {
        line: line.max(0) as u32,
        col: col.max(0) as u32,
    }
}

fn expr_pos(e: &Expr) -> Pos {
    match e {
        Expr::BoolOp {
            lineno, col_offset, ..
        }
        | Expr::NamedExpr {
            lineno, col_offset, ..
        }
        | Expr::BinOp {
            lineno, col_offset, ..
        }
        | Expr::UnaryOp {
            lineno, col_offset, ..
        }
        | Expr::Lambda {
            lineno, col_offset, ..
        }
        | Expr::IfExp {
            lineno, col_offset, ..
        }
        | Expr::Dict {
            lineno, col_offset, ..
        }
        | Expr::Set {
            lineno, col_offset, ..
        }
        | Expr::ListComp {
            lineno, col_offset, ..
        }
        | Expr::SetComp {
            lineno, col_offset, ..
        }
        | Expr::DictComp {
            lineno, col_offset, ..
        }
        | Expr::GeneratorExp {
            lineno, col_offset, ..
        }
        | Expr::Await {
            lineno, col_offset, ..
        }
        | Expr::Yield {
            lineno, col_offset, ..
        }
        | Expr::YieldFrom {
            lineno, col_offset, ..
        }
        | Expr::Compare {
            lineno, col_offset, ..
        }
        | Expr::Call {
            lineno, col_offset, ..
        }
        | Expr::FormattedValue {
            lineno, col_offset, ..
        }
        | Expr::JoinedStr {
            lineno, col_offset, ..
        }
        | Expr::Constant {
            lineno, col_offset, ..
        }
        | Expr::Attribute {
            lineno, col_offset, ..
        }
        | Expr::Subscript {
            lineno, col_offset, ..
        }
        | Expr::Starred {
            lineno, col_offset, ..
        }
        | Expr::Name {
            lineno, col_offset, ..
        }
        | Expr::List {
            lineno, col_offset, ..
        }
        | Expr::Tuple {
            lineno, col_offset, ..
        }
        | Expr::Slice {
            lineno, col_offset, ..
        } => pos_of(*lineno, *col_offset),
    }
}

fn pattern_pos(p: &Pattern) -> Pos {
    match p {
        Pattern::MatchValue {
            lineno, col_offset, ..
        }
        | Pattern::MatchSingleton {
            lineno, col_offset, ..
        }
        | Pattern::MatchSequence {
            lineno, col_offset, ..
        }
        | Pattern::MatchMapping {
            lineno, col_offset, ..
        }
        | Pattern::MatchClass {
            lineno, col_offset, ..
        }
        | Pattern::MatchStar {
            lineno, col_offset, ..
        }
        | Pattern::MatchAs {
            lineno, col_offset, ..
        }
        | Pattern::MatchOr {
            lineno, col_offset, ..
        } => pos_of(*lineno, *col_offset),
    }
}

fn stmt_pos(s: &Stmt) -> Pos {
    let (line, col) = match s {
        Stmt::FunctionDef {
            lineno, col_offset, ..
        }
        | Stmt::AsyncFunctionDef {
            lineno, col_offset, ..
        }
        | Stmt::ClassDef {
            lineno, col_offset, ..
        }
        | Stmt::Return {
            lineno, col_offset, ..
        }
        | Stmt::Delete {
            lineno, col_offset, ..
        }
        | Stmt::Assign {
            lineno, col_offset, ..
        }
        | Stmt::TypeAlias {
            lineno, col_offset, ..
        }
        | Stmt::AugAssign {
            lineno, col_offset, ..
        }
        | Stmt::AnnAssign {
            lineno, col_offset, ..
        }
        | Stmt::For {
            lineno, col_offset, ..
        }
        | Stmt::AsyncFor {
            lineno, col_offset, ..
        }
        | Stmt::While {
            lineno, col_offset, ..
        }
        | Stmt::If {
            lineno, col_offset, ..
        }
        | Stmt::With {
            lineno, col_offset, ..
        }
        | Stmt::AsyncWith {
            lineno, col_offset, ..
        }
        | Stmt::Match {
            lineno, col_offset, ..
        }
        | Stmt::Raise {
            lineno, col_offset, ..
        }
        | Stmt::Try {
            lineno, col_offset, ..
        }
        | Stmt::TryStar {
            lineno, col_offset, ..
        }
        | Stmt::Assert {
            lineno, col_offset, ..
        }
        | Stmt::Import {
            lineno, col_offset, ..
        }
        | Stmt::ImportFrom {
            lineno, col_offset, ..
        }
        | Stmt::Global {
            lineno, col_offset, ..
        }
        | Stmt::Nonlocal {
            lineno, col_offset, ..
        }
        | Stmt::Expr {
            lineno, col_offset, ..
        }
        | Stmt::Pass {
            lineno, col_offset, ..
        }
        | Stmt::Break {
            lineno, col_offset, ..
        }
        | Stmt::Continue {
            lineno, col_offset, ..
        } => (*lineno, *col_offset),
    };
    pos_of(line, col)
}

/// Module-level state shared by every lowered function.
struct Lowerer<'a> {
    interner: &'a Interner,
    names: HashMap<String, Sym>,
    consts: ConstPool,
    functions: Vec<Function>,
    classes: Vec<ClassDesc>,
    root: &'a SymbolTable,
    optimize: u8,
    future_annotations: bool,
    line_starts: Vec<usize>,
    source: &'a [u8],
}

impl<'a> Lowerer<'a> {
    /// The interned pool symbol for `s`, cached per module.
    fn pool(&mut self, s: &str) -> Sym {
        if let Some(&v) = self.names.get(s) {
            return v;
        }
        let v = self.consts.intern(s);
        self.names.insert(s.to_owned(), v);
        v
    }

    /// The source text of a name or literal, with lone surrogates replaced
    /// (the const pool holds Rust strings).
    fn text(&self, s: FSym) -> String {
        self.interner
            .resolve(s)
            .iter()
            .map(|&cp| char::from_u32(cp).unwrap_or('\u{FFFD}'))
            .collect()
    }

    fn reserve_function(&mut self) -> FnId {
        self.functions.push(Function {
            qualname: Sym(u32::MAX),
            params: Params {
                positional: Vec::new(),
                vararg: None,
                kwonly: Vec::new(),
                kwarg: None,
            },
            blocks: Vec::new(),
            entry: BlockId(0),
            locals: Vec::new(),
            flags: FnFlags(0),
            types: Vec::new(),
            lines: Vec::new(),
        });
        FnId(self.functions.len() as u32 - 1)
    }

    fn reserve_class(&mut self) -> ClassId {
        self.classes.push(ClassDesc {
            name: Sym(u32::MAX),
            bases: Box::from([]),
            body: FnId(0),
            methods: Box::from([]),
            has_ext_base: false,
        });
        ClassId(self.classes.len() as u32 - 1)
    }

    /// A symbol's flags in the module scope (the top-level shadowing check
    /// for the `super()` rewrite).
    fn root_flags(&self, name: &str) -> Option<i64> {
        for &(s, flags) in &self.root.symbols {
            let it = self.interner.resolve(s).iter();
            let eq = it.len() == name.chars().count()
                && it
                    .zip(name.chars())
                    .all(|(&cp, c)| char::from_u32(cp) == Some(c));
            if eq {
                return Some(flags);
            }
        }
        None
    }

    /// Source text between an expression's start and end positions.
    fn span_text(&self, e: &Expr) -> String {
        let (l1, c1, l2, c2) = match e {
            Expr::BoolOp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::NamedExpr {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::BinOp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::UnaryOp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Lambda {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::IfExp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Dict {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Set {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::ListComp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::SetComp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::DictComp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::GeneratorExp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Await {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Yield {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::YieldFrom {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Compare {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Call {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::FormattedValue {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::JoinedStr {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Constant {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Attribute {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Subscript {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Starred {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Name {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::List {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Tuple {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Slice {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => (*lineno, *col_offset, *end_lineno, *end_col_offset),
        };
        let start = self
            .line_starts
            .get(l1.max(1) as usize - 1)
            .copied()
            .unwrap_or(0)
            + c1.max(0) as usize;
        let end_l = l2.unwrap_or(l1).max(1) as usize;
        let end = self
            .line_starts
            .get(end_l - 1)
            .copied()
            .unwrap_or(self.source.len())
            + c2.unwrap_or(c1).max(0) as usize;
        let end = end.min(self.source.len()).max(start);
        String::from_utf8_lossy(&self.source[start..end]).into_owned()
    }

    #[allow(clippy::too_many_arguments)]
    fn new_fn(
        &mut self,
        scope: &'a SymbolTable,
        kind: FnKind,
        _name: &str,
        qualname: &str,
        params: ParamSpec,
        private: Option<String>,
        mangled: Option<HashSet<String>>,
        eff: (Flavor, &'a SymbolTable),
    ) -> FnLower<'a, '_> {
        let param_syms: Vec<Sym> = params.ordered().iter().map(|p| self.pool(p)).collect();
        let mut b = FunctionBuilder::new(
            self.pool(qualname),
            Params {
                positional: params.positional.iter().map(|p| self.pool(p)).collect(),
                vararg: params.vararg.as_ref().map(|p| self.pool(p)),
                kwonly: params.kwonly.iter().map(|p| self.pool(p)).collect(),
                kwarg: params.kwarg.as_ref().map(|p| self.pool(p)),
            },
            vec![Type::Top; param_syms.len()],
        )
        .expect("param count matches types");
        b.set_none_const(self.consts.const_idx(Const::None));
        let mut sym_map = HashMap::new();
        for &(s, flags) in &scope.symbols {
            sym_map.insert(self.text(s), flags);
        }
        let mut cells: HashMap<String, Value> = HashMap::new();
        let mut flags = FnFlags(0);
        let entry = b.entry();
        // Generator, coroutine and async-generator flags, from the scope's
        // own symbols (spec section 12.5: the state machine the backend
        // builds keys off these).
        if scope.generator {
            flags = flags.union(if scope.coroutine {
                FnFlags::ASYNC_GEN
            } else {
                FnFlags::GENERATOR
            });
        } else if scope.coroutine {
            flags = flags.union(FnFlags::COROUTINE);
        }
        // A module needs cells only when an inlined comprehension's captured
        // target turned into one (dictbytype matches CELL scope or the
        // DEF_COMP_CELL flag, which survives on names the module also binds).
        let has_cells = scope.symbols.iter().any(|&(_, sym_flags)| {
            scope_of(sym_flags) == symtable::CELL || sym_flags & symtable::DEF_COMP_CELL != 0
        });
        let cell_scoped = kind.function_like() || matches!(kind, FnKind::Class) || has_cells;
        if kind.function_like() {
            // CPython's fast locals start as NULL slots; a read of a local
            // never assigned on the path yields None here instead of raising
            // UnboundLocalError (the IR has no unbound marker).
            for &(s, sym_flags) in &scope.symbols {
                let name = self.text(s);
                if scope_of(sym_flags) == symtable::LOCAL && !param_syms.contains(&self.pool(&name))
                {
                    let into = b.fresh(Type::Top);
                    b.push(Instr::Const {
                        idx: self.consts.const_idx(Const::None),
                        into,
                    })
                    .expect("entry block is open");
                    b.write_variable(self.pool(&name), entry, into)
                        .expect("entry block exists");
                }
            }
        } else {
            // A comprehension `for` target in a name-operation scope runs as
            // an SSA local while the comprehension is active; it needs an
            // entry-time definition so the save-read at the comprehension's
            // start verifies (an entry def dominates everything, so reads on
            // any later exception edge resolve to it).
            for &s in &scope.comp_bound {
                let name = self.text(s);
                // A comp target the parent also routes through a cell
                // (DEF_COMP_CELL) is swapped via cells, not an SSA save.
                let is_local = scope.symbols.iter().any(|&(sym, sym_flags)| {
                    sym == s
                        && scope_of(sym_flags) == symtable::LOCAL
                        && sym_flags & symtable::DEF_COMP_CELL == 0
                });
                if !is_local || param_syms.contains(&self.pool(&name)) {
                    continue;
                }
                let into = b.fresh(Type::Top);
                b.push(Instr::Const {
                    idx: self.consts.const_idx(Const::None),
                    into,
                })
                .expect("entry block is open");
                b.write_variable(self.pool(&name), entry, into)
                    .expect("entry block exists");
                if !cell_scoped {
                    b.add_local(LocalMeta {
                        name: self.pool(&name),
                        kind: LocalKind::Local,
                    });
                }
            }
        }
        if cell_scoped {
            // Cell variables start as empty cells; a cell that is also a
            // parameter is initialised from the argument.
            for &(s, sym_flags) in &scope.symbols {
                let name = self.text(s);
                if scope_of(sym_flags) == symtable::CELL || sym_flags & symtable::DEF_COMP_CELL != 0
                {
                    let initial = param_syms
                        .iter()
                        .position(|&p| p == self.pool(&name))
                        .map(|i| Value(i as u32));
                    let into = b.fresh(Type::Top);
                    b.push(Instr::MakeCell { initial, into })
                        .expect("entry block is open");
                    cells.insert(name, into);
                    flags = flags.union(FnFlags::HAS_CELLS);
                }
            }
            // Free variables arrive as the function's closure, bound by the
            // parent's MakeFunction/MakeClass in Free-local order. Class
            // bodies name-op their own locals but close over free names the
            // same way.
            let mut free_index = 0u32;
            for &(s, sym_flags) in &scope.symbols {
                // DEF_FREE_CLASS marks a class-body local that a child
                // closes over; CPython still routes it through the class
                // body's closure (dictbytype with DEF_FREE_CLASS).
                if scope_of(sym_flags) == symtable::FREE
                    || sym_flags & symtable::DEF_FREE_CLASS != 0
                {
                    let into = b.fresh(Type::Top);
                    b.push(Instr::LoadClosure {
                        index: free_index,
                        into,
                    })
                    .expect("entry block is open");
                    cells.insert(self.text(s), into);
                    free_index += 1;
                    flags = flags.union(FnFlags::HAS_CELLS);
                }
            }
            for &(s, sym_flags) in &scope.symbols {
                let meta_kind = match scope_of(sym_flags) {
                    symtable::CELL => LocalKind::Cell,
                    symtable::FREE => LocalKind::Free,
                    _ if sym_flags & symtable::DEF_FREE_CLASS != 0 => LocalKind::Free,
                    _ if sym_flags & symtable::DEF_COMP_CELL != 0 => LocalKind::Cell,
                    _ => LocalKind::Local,
                };
                b.add_local(LocalMeta {
                    name: self.pool(&self.text(s)),
                    kind: meta_kind,
                });
            }
        }
        b.set_flags(flags);
        FnLower {
            low: self,
            b,
            scope,
            sym_map,
            child: 0,
            kind,
            qualname: qualname.to_owned(),
            eff_flavor: eff.0,
            eff_scope: eff.1,
            private,
            mangled,
            cells,
            handlers: Vec::new(),
            unwinds: Vec::new(),
            raised_in: None,
            class_cell: None,
            classdict_cell: None,
            methods: Vec::new(),
            join_count: 0,
            n_argcount: params.positional.len(),
            finally_count: 0,
            terminated: false,
            async_def: false,
            genexp_scope: false,
            comp_frames: Vec::new(),
            yield_state: 1,
            pending: VecDeque::new(),
        }
    }
}

/// One scope in first-seen order, with each name's flag word.
fn scope_of(flags: i64) -> i64 {
    (flags >> symtable::SCOPE_OFFSET) & symtable::SCOPE_MASK
}

/// `_Py_Mangle`: `__x` becomes `_ClassName__x` in class scopes.
fn mangle_name(private: &str, name: &str) -> String {
    let mut chars = name.chars();
    if !(matches!(chars.next(), Some('_')) && matches!(chars.next(), Some('_'))) {
        return name.to_owned();
    }
    if name.ends_with("__") || name.contains('.') {
        return name.to_owned();
    }
    let stripped = private.trim_start_matches('_');
    if stripped.is_empty() {
        return name.to_owned();
    }
    format!("_{stripped}{name}")
}

/// A region a `break`, `continue` or `return` crosses on its way out, in
/// order from innermost out (CPython's fblock stack). Each variant names the
/// action unwinding needs, mirroring `compiler_unwind_fblock`.
enum Unwind {
    Loop {
        end: BlockId,
        head: BlockId,
    },
    /// A protected body (`try` body, `except*` body region) with nothing to
    /// undo.
    Handler,
    /// An `except`/`except*` handler body: restore the `exc_info` stack and
    /// delete the bound name on the way out.
    HandlerCleanup {
        name: Option<FSym>,
    },
    /// A `with` body: call the bound `__exit__(None, None, None)` on the
    /// way out.
    With {
        exit: Value,
    },
    /// An `async with` body: await the bound `__aexit__(None, None, None)`
    /// on the way out.
    WithAsync {
        exit: Value,
    },
    /// A `try` body guarded by `finally`: store the reason in `var` and jump
    /// `entry`, abandoning the unwinding to the finally's dispatch.
    Finally {
        var: Sym,
        pending: Sym,
        entry: BlockId,
        seq: usize,
        break_sent: bool,
        continue_sent: bool,
        return_sent: bool,
    },
    /// A `finally` body: an exit through it that abandons an in-flight
    /// exception restores the exc_info stack when `pending` says the landing
    /// pushed one (CPython's FINALLY_END unwind pops it too).
    Finalbody {
        pending: Sym,
    },
    /// An `except*` handler body: `break`/`continue`/`return` cannot leave it.
    StarHandler,
}

/// The number of positional sub-patterns a class accepts, for the arity
/// error: fixed for the match-self builtins, `__match_args__`'s length
/// otherwise.
enum Arity {
    Static(usize),
    Runtime(Value),
}

/// The kind of a comprehension expression: which container the element
/// lands in, or that it is yielded (a generator expression).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompKind {
    List,
    Set,
    Dict,
    Generator,
}

/// The symbol-table swap an inlined comprehension installs while it runs
/// (CPython's `push_inlined_comprehension_state`): `overrides` replaces the
/// parent's flags for the names the comprehension isolates, `saved` and
/// `saved_cells` hold what the outer bindings to restore afterwards.
struct CompFrame {
    overrides: HashMap<String, i64>,
    saved: Vec<(String, Value)>,
    saved_cells: Vec<(String, Value)>,
}

/// One function body under construction.
struct FnLower<'a, 'b> {
    low: &'b mut Lowerer<'a>,
    b: FunctionBuilder,
    scope: &'a SymbolTable,
    sym_map: HashMap<String, i64>,
    child: usize,
    /// Sub-scopes taken ahead of the cursor, to be consumed by `take_child`
    /// in an order that differs from the symtable's (see `requeue`).
    pending: VecDeque<&'a SymbolTable>,
    kind: FnKind,
    qualname: String,
    eff_flavor: Flavor,
    eff_scope: &'a SymbolTable,
    private: Option<String>,
    mangled: Option<HashSet<String>>,
    cells: HashMap<String, Value>,
    /// The exception handler of the region being lowered; every block
    /// created while it is pushed carries it (a may-raise instruction
    /// anywhere in a `try` body must reach the body's landing).
    handlers: Vec<BlockId>,
    unwinds: Vec<Unwind>,
    /// The block the last may-raise instruction was emitted in, so a local
    /// store can leave that block first (see `write_var_at_current`).
    raised_in: Option<BlockId>,
    class_cell: Option<Value>,
    classdict_cell: Option<Value>,
    methods: Vec<(Sym, FnId)>,
    join_count: usize,
    /// CPython's `u_argcount`: positional (pos-only then normal)
    /// parameters, the pool zero-arg `super()` rewrites against.
    n_argcount: usize,
    finally_count: usize,
    terminated: bool,
    /// The enclosing definition is `async def`.
    async_def: bool,
    /// The scope is a generator expression's: `await` is legal inside it
    /// even when the enclosing definition is not async.
    genexp_scope: bool,
    /// Active inlined comprehensions, innermost last (CPython's
    /// `u_in_inlined_comp` count plus the per-frame swap state).
    comp_frames: Vec<CompFrame>,
    /// The next resume-point tag handed to `Yield`/`YieldFrom` (spec
    /// section 12.5); states count from 1.
    yield_state: u32,
}

impl<'a, 'b> FnLower<'a, 'b> {
    fn emit(&mut self, instr: Instr, pos: Pos) -> Result<InstrId, LowerError> {
        let raised = instr.may_raise();
        let id = self.b.push(instr)?;
        if raised && let Some(cur) = self.b.current_block() {
            self.raised_in = Some(cur);
        }
        self.b.record_line(id, pos.line, pos.col)?;
        Ok(id)
    }

    /// A new block under the current region's exception handler.
    fn new_block(&mut self) -> Result<BlockId, LowerError> {
        let block = self.b.new_block();
        if let Some(&h) = self.handlers.last() {
            self.b.set_handler(block, Some(h))?;
        }
        Ok(block)
    }

    /// Move the emission point into a block covered by `landing`, which a
    /// region's body needs: the block the region starts in predates the
    /// handler push, so its earlier instructions belong to the enclosing
    /// region (CPython's SETUP_FINALLY covers only what follows it).
    fn enter_region(&mut self, landing: BlockId) -> Result<(), LowerError> {
        if self.terminated {
            return Ok(());
        }
        let body = self.b.new_block();
        self.b.set_handler(body, Some(landing))?;
        self.jump(body)?;
        self.terminated = false;
        self.b.switch_to(body)?;
        Ok(())
    }

    fn fresh_top(&mut self) -> Value {
        self.b.fresh(Type::Top)
    }

    fn value_of(
        &mut self,
        pos: Pos,
        make: impl FnOnce(Value) -> Instr,
    ) -> Result<Value, LowerError> {
        let into = self.b.fresh(Type::Top);
        self.emit(make(into), pos)?;
        Ok(into)
    }

    fn unboxed_bool(
        &mut self,
        pos: Pos,
        make: impl FnOnce(Value) -> Instr,
    ) -> Result<Value, LowerError> {
        let into = self.b.fresh(Type::Bool);
        self.emit(make(into), pos)?;
        Ok(into)
    }

    fn build_list(&mut self, items: Vec<Value>, pos: Pos) -> Result<Value, LowerError> {
        let into = self.b.fresh(Type::List);
        self.emit(
            Instr::BuildList {
                items: items.into(),
                into,
            },
            pos,
        )?;
        Ok(into)
    }

    fn build_dict(&mut self, items: Vec<(Value, Value)>, pos: Pos) -> Result<Value, LowerError> {
        let into = self.b.fresh(Type::Dict);
        self.emit(
            Instr::BuildDict {
                items: items.into(),
                into,
            },
            pos,
        )?;
        Ok(into)
    }

    fn build_set(&mut self, items: Vec<Value>, pos: Pos) -> Result<Value, LowerError> {
        let into = self.b.fresh(Type::Set);
        self.emit(
            Instr::BuildSet {
                items: items.into(),
                into,
            },
            pos,
        )?;
        Ok(into)
    }

    fn build_str(&mut self, parts: Vec<Value>, pos: Pos) -> Result<Value, LowerError> {
        let into = self.b.fresh(Type::Str);
        self.emit(
            Instr::BuildStr {
                parts: parts.into(),
                into,
            },
            pos,
        )?;
        Ok(into)
    }

    fn build_tuple(&mut self, items: Vec<Value>, pos: Pos) -> Result<Value, LowerError> {
        let ty = Type::Tuple(vec![Type::Top; items.len()].into());
        let into = self.b.fresh(ty);
        self.emit(
            Instr::BuildTuple {
                items: items.into(),
                into,
            },
            pos,
        )?;
        Ok(into)
    }

    fn const_val(&mut self, c: Const, pos: Pos) -> Result<Value, LowerError> {
        let idx = self.low.consts.const_idx(c);
        self.value_of(pos, move |into| Instr::Const { idx, into })
    }

    fn none(&mut self) -> Result<Value, LowerError> {
        self.const_val(Const::None, Pos { line: 0, col: 0 })
    }

    fn jump(&mut self, to: BlockId) -> Result<(), LowerError> {
        if self.terminated {
            return Ok(());
        }
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(at, Terminator::Jump { to })?;
        self.terminated = true;
        Ok(())
    }

    fn branch(&mut self, cond: Value, t: BlockId, f: BlockId) -> Result<(), LowerError> {
        if self.terminated {
            return Ok(());
        }
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(
            at,
            Terminator::Branch {
                cond,
                then_to: t,
                else_to: f,
            },
        )?;
        self.terminated = true;
        Ok(())
    }

    fn ret(&mut self, value: Value) -> Result<(), LowerError> {
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(at, Terminator::Return { value })?;
        self.terminated = true;
        Ok(())
    }

    fn pool(&mut self, name: &str) -> Sym {
        self.low.pool(name)
    }

    fn text(&self, s: FSym) -> String {
        self.low.text(s)
    }

    /// `_Py_Mangle` under this scope's private name; the names bound by an
    /// enclosing PEP 695 type-parameter list are exempt.
    fn mangle(&self, name: &str) -> String {
        if self.mangled.as_ref().is_some_and(|e| e.contains(name)) {
            return name.to_owned();
        }
        match &self.private {
            Some(p) => mangle_name(p, name),
            None => name.to_owned(),
        }
    }

    fn flags_for(&self, name: &str) -> Option<i64> {
        // An active inlined comprehension swapped this name's flags in for
        // its own duration (CPython's temp symbol table); the innermost
        // frame that has it wins.
        for frame in self.comp_frames.iter().rev() {
            if let Some(&flags) = frame.overrides.get(name) {
                return Some(flags);
            }
        }
        self.sym_map.get(name).copied()
    }

    /// True when an active inlined comprehension isolates this name: it
    /// resolves by its comprehension scope, not the parent's.
    fn comp_isolated(&self, mangled: &str) -> bool {
        self.comp_frames
            .iter()
            .rev()
            .any(|f| f.overrides.contains_key(mangled))
    }

    /// The next child scope in CPython's codegen order.
    fn take_child(&mut self) -> Option<&'a SymbolTable> {
        if let Some(front) = self.pending.pop_front() {
            return Some(front);
        }
        let scope: &'a SymbolTable = self.scope;
        let child = scope.children.get(self.child)?;
        self.child += 1;
        Some(child)
    }

    /// The symtable visits a definition's sub-scopes in its own order
    /// (defaults, then decorators), while the code generator evaluates the
    /// decorator expressions first. Take the statement's first
    /// `n_defaults + n_decors` sub-scopes in symtable order and requeue
    /// them in evaluation order, so each sub-expression still binds the
    /// scope the symtable gave it.
    fn requeue(&mut self, n_defaults: usize, n_decors: usize) {
        let group: Vec<&'a SymbolTable> = (0..n_defaults + n_decors)
            .filter_map(|_| self.take_child())
            .collect();
        for i in n_defaults..n_defaults + n_decors {
            if let Some(s) = group.get(i) {
                self.pending.push_back(s);
            }
        }
        for i in 0..n_defaults {
            if let Some(s) = group.get(i) {
                self.pending.push_back(s);
            }
        }
    }

    // ---- names -------------------------------------------------------

    /// Load a bare (unmangled) name, e.g. a method name or keyword argument.
    fn load_plain_name(&mut self, name: &str, pos: Pos) -> Result<Value, LowerError> {
        self.load_scoped_name(name, name, pos)
    }

    /// Load an identifier under the scope rules, applying name mangling.
    fn load_name(&mut self, id: FSym, pos: Pos) -> Result<Value, LowerError> {
        let raw = self.text(id);
        let mangled = self.mangle(&raw);
        self.load_scoped_name(&raw, &mangled, pos)
    }

    fn load_scoped_name(
        &mut self,
        raw: &str,
        mangled: &str,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        // `__debug__` is a compile-time constant unless shadowed; the
        // symtable never creates a symbol for it.
        if raw == "__debug__" && self.flags_for(mangled).is_none() {
            return self.const_val(Const::Bool(self.low.optimize == 0), pos);
        }
        // A class body's namespace is invisible inside an inlined
        // comprehension, so implicit globals load as globals there.
        let class_comp = self.kind == FnKind::Class && !self.comp_frames.is_empty();
        let name_ops = self.kind.uses_name_ops() && !self.comp_isolated(mangled);
        // A name the symtable never saw is an implicit global: exception
        // tuples and pattern helpers load builtins the source never names.
        let flags = match self.flags_for(mangled) {
            Some(flags) => flags,
            None if name_ops && !class_comp => {
                let name = self.pool(mangled);
                return self.value_of(pos, |into| Instr::LoadName { name, into });
            }
            None => {
                let name = self.pool(mangled);
                return self.value_of(pos, |into| Instr::LoadGlobal { name, into });
            }
        };
        match scope_of(flags) {
            symtable::GLOBAL_EXPLICIT => {
                let name = self.pool(mangled);
                self.value_of(pos, |into| Instr::LoadGlobal { name, into })
            }
            symtable::GLOBAL_IMPLICIT if !name_ops => {
                let name = self.pool(mangled);
                self.value_of(pos, |into| Instr::LoadGlobal { name, into })
            }
            symtable::CELL | symtable::FREE => {
                let cell = self.cell_of(mangled, pos)?;
                self.value_of(pos, move |into| Instr::LoadCell { cell, into })
            }
            _ if name_ops => {
                let name = self.pool(mangled);
                if class_comp {
                    self.value_of(pos, |into| Instr::LoadGlobal { name, into })
                } else {
                    self.value_of(pos, |into| Instr::LoadName { name, into })
                }
            }
            _ => {
                let sym = self.pool(mangled);
                let at = self
                    .b
                    .current_block()
                    .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
                let v = self.b.read_variable(sym, at).map_err(LowerError::Build)?;
                Ok(v)
            }
        }
    }

    fn store_name_value(&mut self, id: FSym, value: Value, pos: Pos) -> Result<(), LowerError> {
        let mangled = self.mangle(&self.text(id));
        self.store_scoped_name(&mangled, value, pos)
    }

    fn store_scoped_name(
        &mut self,
        mangled: &str,
        value: Value,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let name_ops = self.kind.uses_name_ops() && !self.comp_isolated(mangled);
        let flags = self.flags_for(mangled).unwrap_or(0);
        match scope_of(flags) {
            symtable::GLOBAL_EXPLICIT => {
                let name = self.pool(mangled);
                self.emit(Instr::StoreGlobal { name, value }, pos)?;
                Ok(())
            }
            symtable::GLOBAL_IMPLICIT if !name_ops => {
                let name = self.pool(mangled);
                self.emit(Instr::StoreGlobal { name, value }, pos)?;
                Ok(())
            }
            symtable::CELL | symtable::FREE => {
                let cell = self.cell_of(mangled, pos)?;
                self.emit(Instr::StoreCell { cell, value }, pos)?;
                Ok(())
            }
            _ if name_ops => {
                let name = self.pool(mangled);
                self.emit(Instr::StoreName { name, value }, pos)?;
                Ok(())
            }
            _ => {
                let sym = self.pool(mangled);
                self.write_var_at_current(sym, value)?;
                Ok(())
            }
        }
    }

    fn cell_of(&mut self, mangled: &str, pos: Pos) -> Result<Value, LowerError> {
        if let Some(cell) = self.cells.get(mangled) {
            return Ok(*cell);
        }
        Err(LowerError::MissingCell {
            name: mangled.to_owned(),
            line: pos.line,
        })
    }

    /// Store a bare (unmangled) name.
    fn store_plain_name(&mut self, name: &str, value: Value, pos: Pos) -> Result<(), LowerError> {
        self.store_scoped_name(name, value, pos)
    }

    fn del_name(&mut self, id: FSym, pos: Pos) -> Result<(), LowerError> {
        let mangled = self.mangle(&self.text(id));
        let name_ops = self.kind.uses_name_ops() && !self.comp_isolated(&mangled);
        let flags = self.flags_for(&mangled).unwrap_or(0);
        match scope_of(flags) {
            symtable::GLOBAL_EXPLICIT | symtable::GLOBAL_IMPLICIT if !name_ops => {
                let name = self.pool(&mangled);
                let value = self.none()?;
                self.emit(Instr::StoreGlobal { name, value }, pos)?;
            }
            symtable::CELL | symtable::FREE => {
                let cell = self.cell_of(&mangled, pos)?;
                let value = self.none()?;
                self.emit(Instr::StoreCell { cell, value }, pos)?;
            }
            _ if name_ops => {
                // DELETE_NAME. The IR has no unbind; storing None leaves the
                // name bound (reads after `del` yield None, not NameError).
                let name = self.pool(&mangled);
                let value = self.none()?;
                self.emit(Instr::StoreName { name, value }, pos)?;
            }
            _ => {
                // DELETE_FAST. SSA has no unbind; a marker store of None.
                let value = self.none()?;
                self.store_scoped_name(&mangled, value, pos)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

fn docstring(body: &[Stmt], interner: &Interner) -> Option<String> {
    let _ = interner;
    match body.first()? {
        Stmt::Expr { value, .. } => match &**value {
            Expr::Constant {
                value: Constant::Str(cps),
                ..
            } => Some(
                cps.iter()
                    .map(|&cp| char::from_u32(cp).unwrap_or('\u{FFFD}'))
                    .collect(),
            ),
            _ => None,
        },
        _ => None,
    }
}

/// CPython's `find_ann`: does this body contain an annotated assignment whose
/// annotation the scope must record in `__annotations__`?
fn find_ann(body: &[Stmt]) -> bool {
    for s in body {
        let hit = match s {
            Stmt::AnnAssign { .. } => true,
            Stmt::If { body, orelse, .. } => find_ann(body) || find_ann(orelse),
            Stmt::While { body, orelse, .. } => find_ann(body) || find_ann(orelse),
            Stmt::For { body, orelse, .. } | Stmt::AsyncFor { body, orelse, .. } => {
                find_ann(body) || find_ann(orelse)
            }
            Stmt::With { body, .. } | Stmt::AsyncWith { body, .. } => find_ann(body),
            Stmt::Try {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            }
            | Stmt::TryStar {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            } => {
                find_ann(body)
                    || handlers.iter().any(|h| match h {
                        crate::ast_generated::Excepthandler::ExceptHandler { body, .. } => {
                            find_ann(body)
                        }
                    })
                    || find_ann(orelse)
                    || find_ann(finalbody)
            }
            Stmt::Match { cases, .. } => cases.iter().any(|c| find_ann(&c.body)),
            _ => false,
        };
        if hit {
            return true;
        }
    }
    false
}

impl<'a, 'b> FnLower<'a, 'b> {
    fn finish_into(mut self, fid: FnId) -> Result<(), LowerError> {
        self.b.seal_all().map_err(|source| LowerError::BuildIn {
            qualname: self.qualname.clone(),
            source,
        })?;
        let f = self.b.finish()?;
        self.low.functions[fid.0 as usize] = f;
        Ok(())
    }

    fn return_none(&mut self) -> Result<(), LowerError> {
        if !self.terminated {
            let v = self.none()?;
            self.ret(v)?;
        }
        Ok(())
    }

    /// A module or class body: `__annotations__` setup, docstring, statements.
    fn lower_namespace_body(&mut self, body: &[Stmt]) -> Result<(), LowerError> {
        let first_pos = body
            .first()
            .map(stmt_pos)
            .unwrap_or(Pos { line: 1, col: 0 });
        if find_ann(body) {
            // SETUP_ANNOTATIONS: create the annotations dict eagerly (the
            // bytecode creates it lazily on first store).
            let d = self.build_dict(Vec::new(), first_pos)?;
            self.store_plain_name("__annotations__", d, first_pos)?;
        }
        let mut start = 0;
        if self.low.optimize < 2
            && let Some(doc) = docstring(body, self.low.interner)
        {
            let pos = first_pos;
            let sym = self.pool(&doc);
            let idx = self.low.consts.const_idx(Const::Str(sym));
            let v = self.value_of(pos, move |into| Instr::Const { idx, into })?;
            self.store_plain_name("__doc__", v, pos)?;
            start = 1;
        }
        for s in &body[start..] {
            self.lower_stmt(s)?;
        }
        Ok(())
    }

    fn lower_body(&mut self, body: &[Stmt]) -> Result<(), LowerError> {
        for s in body {
            self.lower_stmt(s)?;
        }
        Ok(())
    }

    fn lower_stmt(&mut self, s: &Stmt) -> Result<(), LowerError> {
        match s {
            Stmt::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
                ..
            } => self.lower_function_def(
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
                false,
                stmt_pos(s),
            ),
            Stmt::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
                ..
            } => self.lower_class_def(
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
                stmt_pos(s),
            ),
            Stmt::TypeAlias {
                name,
                type_params,
                value,
                ..
            } => self.lower_type_alias(name, type_params, value, stmt_pos(s)),
            Stmt::Return { value, .. } => {
                if self.terminated {
                    return Ok(());
                }
                if value.is_some() && self.scope.generator && self.scope.coroutine {
                    return Err(LowerError::Rejected {
                        message: "'return' with value in async generator".to_owned(),
                        line: stmt_pos(s).line,
                    });
                }
                let v = match value {
                    Some(e) => self.expr(e)?,
                    None => self.none()?,
                };
                self.unwind_and_return(v, stmt_pos(s))
            }
            Stmt::Delete { targets, .. } => {
                for t in targets {
                    self.lower_del_target(t)?;
                }
                Ok(())
            }
            Stmt::Assign { targets, value, .. } => {
                let v = self.expr(value)?;
                for t in targets {
                    self.assign_target(t, v, stmt_pos(s))?;
                }
                Ok(())
            }
            Stmt::AugAssign {
                target, op, value, ..
            } => self.lower_aug_assign(target, op, value, stmt_pos(s)),
            Stmt::AnnAssign {
                target,
                annotation,
                value,
                simple,
                ..
            } => self.lower_ann_assign(target, annotation, value.as_deref(), *simple, stmt_pos(s)),
            Stmt::For {
                target,
                iter,
                body,
                orelse,
                ..
            } => self.lower_for(target, iter, body, orelse, stmt_pos(s)),
            Stmt::While {
                test, body, orelse, ..
            } => self.lower_while(test, body, orelse, stmt_pos(s)),
            Stmt::If {
                test, body, orelse, ..
            } => self.lower_if(test, body, orelse),
            Stmt::Raise { exc, cause, .. } => {
                let e = match exc {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                let c = match cause {
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                if self.terminated {
                    return Ok(());
                }
                let at = self
                    .b
                    .current_block()
                    .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
                self.b
                    .set_term(at, Terminator::Raise { exc: e, cause: c })?;
                self.terminated = true;
                Ok(())
            }
            Stmt::Assert { test, msg, .. } => {
                if self.low.optimize > 0 {
                    return Ok(());
                }
                let pos = stmt_pos(s);
                let ok = self.truth_of(test)?;
                let pass = self.new_block()?;
                let fail = self.new_block()?;
                self.branch(ok, pass, fail)?;
                self.terminated = false;
                self.b.switch_to(fail)?;
                let err_sym = self.pool("AssertionError");
                let err = self.value_of(pos, move |into| Instr::LoadGlobal {
                    name: err_sym,
                    into,
                })?;
                let exc = match msg {
                    Some(m) => {
                        let mval = self.expr(m)?;
                        let args: Box<[Value]> = Box::from([mval]);
                        let kn: Box<[(Sym, Value)]> = Box::from([]);
                        self.value_of(pos, move |into| Instr::Call {
                            callee: err,
                            args,
                            kwnames: kn,
                            into,
                        })?
                    }
                    None => err,
                };
                let at = self
                    .b
                    .current_block()
                    .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
                self.b.set_term(
                    at,
                    Terminator::Raise {
                        exc: Some(exc),
                        cause: None,
                    },
                )?;
                self.terminated = true;
                self.terminated = false;
                self.b.switch_to(pass)?;
                Ok(())
            }
            Stmt::Import { names, .. } => self.lower_import(names, stmt_pos(s)),
            Stmt::ImportFrom {
                module,
                names,
                level,
                ..
            } => self.lower_import_from(module.as_ref(), names, level.unwrap_or(0), stmt_pos(s)),
            Stmt::Expr { value, .. } => {
                self.expr(value)?;
                Ok(())
            }
            Stmt::Break { .. } => {
                if self.terminated {
                    return Ok(());
                }
                self.unwind_to_loop(false, stmt_pos(s))
            }
            Stmt::Continue { .. } => {
                if self.terminated {
                    return Ok(());
                }
                self.unwind_to_loop(true, stmt_pos(s))
            }
            Stmt::Global { .. } | Stmt::Nonlocal { .. } | Stmt::Pass { .. } => Ok(()),
            Stmt::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
                ..
            } => self.lower_function_def(
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
                true,
                stmt_pos(s),
            ),
            Stmt::AsyncFor {
                target,
                iter,
                body,
                orelse,
                ..
            } => self.lower_async_for(target, iter, body, orelse, stmt_pos(s)),
            Stmt::AsyncWith { items, body, .. } => self.lower_async_with(items, body, stmt_pos(s)),
            Stmt::With { items, body, .. } => self.lower_with(items, body, stmt_pos(s)),
            Stmt::Try {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            } => self.lower_try(body, handlers, orelse, finalbody, false, stmt_pos(s)),
            Stmt::TryStar {
                body,
                handlers,
                orelse,
                finalbody,
                ..
            } => self.lower_try(body, handlers, orelse, finalbody, true, stmt_pos(s)),
            Stmt::Match { subject, cases, .. } => self.lower_match(subject, cases),
        }
    }

    fn lower_del_target(&mut self, t: &Expr) -> Result<(), LowerError> {
        let pos = expr_pos(t);
        match t {
            Expr::Name { id, .. } => self.del_name(*id, pos),
            Expr::Attribute { value, attr, .. } => {
                let obj = self.expr(value)?;
                let name = self.pool(&self.mangle(&self.text(*attr)));
                self.emit(Instr::DelAttr { obj, name }, pos)?;
                Ok(())
            }
            Expr::Subscript { value, slice, .. } => {
                let obj = self.expr(value)?;
                let key = self.subscript_key(slice)?;
                self.emit(Instr::DelItem { obj, key }, pos)?;
                Ok(())
            }
            // A tuple or list target is flattened: CPython emits one delete
            // per leaf, in order.
            Expr::Tuple { elts, .. } | Expr::List { elts, .. } => {
                for e in elts {
                    self.lower_del_target(e)?;
                }
                Ok(())
            }
            _ => Err(LowerError::UnexpectedAst {
                what: "invalid del target",
                line: pos.line,
            }),
        }
    }

    /// Assign `value` to a store-context target (CPython's compiler_assign).
    fn assign_target(&mut self, t: &Expr, value: Value, pos: Pos) -> Result<(), LowerError> {
        match t {
            Expr::Name { id, .. } => self.store_name_value(*id, value, pos),
            Expr::Attribute {
                value: obj, attr, ..
            } => {
                let o = self.expr(obj)?;
                let name = self.pool(&self.mangle(&self.text(*attr)));
                self.emit(
                    Instr::SetAttr {
                        obj: o,
                        name,
                        value,
                    },
                    pos,
                )?;
                Ok(())
            }
            Expr::Subscript {
                value: obj, slice, ..
            } => {
                let o = self.expr(obj)?;
                let key = self.subscript_key(slice)?;
                self.emit(Instr::SetItem { obj: o, key, value }, pos)?;
                Ok(())
            }
            Expr::Tuple { elts, .. } | Expr::List { elts, .. } => {
                // Unpack. A bare Starred target inside is the star element.
                let star = elts.iter().position(|e| matches!(e, Expr::Starred { .. }));
                let n = elts.len() as u32;
                let into: Vec<Value> = (0..n).map(|_| self.fresh_top()).collect();
                self.emit(
                    Instr::Unpack {
                        src: value,
                        n,
                        star: star.map(|i| i as u32),
                        into: into.clone().into(),
                    },
                    pos,
                )?;
                for (e, v) in elts.iter().zip(into) {
                    match e {
                        Expr::Starred { value: inner, .. } => self.assign_target(inner, v, pos)?,
                        other => self.assign_target(other, v, pos)?,
                    }
                }
                Ok(())
            }
            _ => Err(LowerError::UnexpectedAst {
                what: "invalid assignment target",
                line: pos.line,
            }),
        }
    }

    fn subscript_key(&mut self, slice: &Expr) -> Result<Value, LowerError> {
        match slice {
            Expr::Slice { .. } => self.slice_value(slice),
            Expr::Tuple { elts, .. } => {
                // `x[a, b:c]`: the key is a tuple whose slice elements
                // become slice objects; `x[a, *b]` builds the tuple at
                // runtime like CPython's LIST_EXTEND + LIST_TO_TUPLE.
                let pos = expr_pos(slice);
                if elts.iter().any(|e| matches!(e, Expr::Starred { .. })) {
                    let lst = self.build_list(Vec::new(), pos)?;
                    for e in elts {
                        let (v, how) = match e {
                            Expr::Starred { value, .. } => (self.expr(value)?, "extend"),
                            other => (self.subscript_element(other)?, "append"),
                        };
                        let m = self.method(&lst, how, pos)?;
                        self.call(m, vec![v], Vec::new(), pos)?;
                    }
                    self.call_global("tuple", vec![lst], pos)
                } else {
                    let mut items = Vec::with_capacity(elts.len());
                    for e in elts {
                        items.push(self.subscript_element(e)?);
                    }
                    self.build_tuple(items, pos)
                }
            }
            other => self.expr(other),
        }
    }

    /// One element of a subscript tuple: slices become slice objects,
    /// everything else is an ordinary expression.
    fn subscript_element(&mut self, e: &Expr) -> Result<Value, LowerError> {
        match e {
            Expr::Slice { .. } => self.slice_value(e),
            other => self.expr(other),
        }
    }

    /// A `lo:up:st` slice expression in subscript position, as a slice
    /// object value.
    fn slice_value(&mut self, slice: &Expr) -> Result<Value, LowerError> {
        let Expr::Slice {
            lower, upper, step, ..
        } = slice
        else {
            return Err(LowerError::UnexpectedAst {
                what: "not a slice expression",
                line: expr_pos(slice).line,
            });
        };
        let pos = expr_pos(slice);
        let lo = match lower {
            Some(e) => Some(self.expr(e)?),
            None => None,
        };
        let up = match upper {
            Some(e) => self.expr(e)?,
            None => self.none()?,
        };
        let st = match step {
            Some(e) => Some(self.expr(e)?),
            None => None,
        };
        self.value_of(pos, move |into| Instr::BuildSlice {
            start: lo,
            stop: up,
            step: st,
            into,
        })
    }

    fn lower_aug_assign(
        &mut self,
        target: &Expr,
        op: &Operator,
        value: &Expr,
        pos: Pos,
    ) -> Result<(), LowerError> {
        // The IR has no in-place operators; `a op= b` lowers to the binary
        // operation (mutation semantics of the in-place variants are lost).
        let bop = bin_op(op);
        match target {
            Expr::Name { id, .. } => {
                let cur = self.load_name(*id, pos)?;
                let rhs = self.expr(value)?;
                let r = self.value_of(pos, move |into| Instr::BinOp {
                    op: bop,
                    lhs: cur,
                    rhs,
                    into,
                })?;
                self.store_name_value(*id, r, pos)
            }
            Expr::Attribute {
                value: obj, attr, ..
            } => {
                let o = self.expr(obj)?;
                let name = self.pool(&self.mangle(&self.text(*attr)));
                let cur = self.value_of(pos, move |into| Instr::GetAttr { obj: o, name, into })?;
                let rhs = self.expr(value)?;
                let r = self.value_of(pos, move |into| Instr::BinOp {
                    op: bop,
                    lhs: cur,
                    rhs,
                    into,
                })?;
                self.emit(
                    Instr::SetAttr {
                        obj: o,
                        name,
                        value: r,
                    },
                    pos,
                )?;
                Ok(())
            }
            Expr::Subscript {
                value: obj, slice, ..
            } => {
                let o = self.expr(obj)?;
                let key = self.subscript_key(slice)?;
                let cur = self.value_of(pos, move |into| Instr::GetItem { obj: o, key, into })?;
                let rhs = self.expr(value)?;
                let r = self.value_of(pos, move |into| Instr::BinOp {
                    op: bop,
                    lhs: cur,
                    rhs,
                    into,
                })?;
                self.emit(
                    Instr::SetItem {
                        obj: o,
                        key,
                        value: r,
                    },
                    pos,
                )?;
                Ok(())
            }
            _ => Err(LowerError::UnexpectedAst {
                what: "invalid augmented assignment target",
                line: pos.line,
            }),
        }
    }

    fn lower_ann_assign(
        &mut self,
        target: &Expr,
        annotation: &Expr,
        value: Option<&Expr>,
        simple: i32,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let name_ops = self.kind.uses_name_ops();
        let future = self.low.future_annotations;
        // value first (CPython order)
        if let Some(v) = value {
            let val = self.expr(v)?;
            self.assign_target(target, val, pos)?;
        }
        if name_ops {
            // module and class scopes record annotations
            if let Expr::Name { id, .. } = target
                && simple != 0
            {
                {
                    let ann_val = if future {
                        let text = self.low.span_text(annotation);
                        let sym = self.pool(&text);
                        let idx = self.low.consts.const_idx(Const::Str(sym));
                        self.value_of(pos, move |into| Instr::Const { idx, into })?
                    } else {
                        self.expr(annotation)?
                    };
                    let d = self.load_plain_name("__annotations__", pos)?;
                    let key = self.mangle(&self.text(*id));
                    let ksym = self.pool(&key);
                    let kidx = self.low.consts.const_idx(Const::Str(ksym));
                    let kv = self.value_of(pos, move |into| Instr::Const { idx: kidx, into })?;
                    self.emit(
                        Instr::SetItem {
                            obj: d,
                            key: kv,
                            value: ann_val,
                        },
                        pos,
                    )?;
                    return Ok(());
                }
            }
            if !future {
                // side effects of the annotation expression still run
                self.expr(annotation)?;
            }
        } else {
            // Function scopes never record annotations and (unlike module
            // and class scopes) never evaluate them either.
            let _ = (annotation, future);
        }
        Ok(())
    }

    fn lower_if(&mut self, test: &Expr, body: &[Stmt], orelse: &[Stmt]) -> Result<(), LowerError> {
        let cond = self.truth_of(test)?;
        let then_blk = self.new_block()?;
        let else_blk = self.new_block()?;
        let end = self.new_block()?;
        self.branch(cond, then_blk, else_blk)?;
        self.terminated = false;
        self.b.switch_to(then_blk)?;
        self.lower_body(body)?;
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(else_blk)?;
        self.lower_body(orelse)?;
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    fn lower_while(
        &mut self,
        test: &Expr,
        body: &[Stmt],
        orelse: &[Stmt],
        _pos: Pos,
    ) -> Result<(), LowerError> {
        let head = self.new_block()?;
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(head)?;
        let cond = self.truth_of(test)?;
        let body_blk = self.new_block()?;
        let else_blk = self.new_block()?;
        let end = self.new_block()?;
        self.branch(cond, body_blk, else_blk)?;
        self.terminated = false;
        self.b.switch_to(body_blk)?;
        self.unwinds.push(Unwind::Loop { end, head });
        self.lower_body(body)?;
        self.unwinds.pop();
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(else_blk)?;
        self.lower_body(orelse)?;
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    fn lower_for(
        &mut self,
        target: &Expr,
        iter: &Expr,
        body: &[Stmt],
        orelse: &[Stmt],
        pos: Pos,
    ) -> Result<(), LowerError> {
        let it_val = self.expr(iter)?;
        let it = self.value_of(pos, |into| Instr::GetIter { obj: it_val, into })?;
        let stop_sym = self.pool("StopIteration");
        let stop_cls = self.value_of(pos, move |into| Instr::LoadGlobal {
            name: stop_sym,
            into,
        })?;
        let types = self.build_tuple(vec![stop_cls], pos)?;
        let head = self.new_block()?;
        let catch = self.new_block()?;
        let body_blk = self.new_block()?;
        let end = self.new_block()?;
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(head)?;
        self.b.set_handler(head, Some(catch))?;
        let next = self.value_of(pos, |into| Instr::Next { iter: it, into })?;
        // The loop variable is written in the normal successor only: the
        // handler edge reads the block's end-of-block definitions, and the
        // Next result is undefined on that edge.
        self.jump(body_blk)?;
        self.terminated = false;
        self.b.switch_to(body_blk)?;
        self.assign_target(target, next, pos)?;
        self.unwinds.push(Unwind::Loop { end, head });
        self.lower_body(body)?;
        self.unwinds.pop();
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(catch)?;
        let matched = self.b.fresh(Type::Bool);
        let exc = self.b.fresh(Type::Top);
        self.emit(
            Instr::CatchBegin {
                types,
                matched,
                exc,
            },
            pos,
        )?;
        let clean = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(matched, clean, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        let at = reraise_blk;
        self.b.set_term(at, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(clean)?;
        self.emit(Instr::CatchEnd, pos)?;
        if orelse.is_empty() {
            self.jump(end)?;
        } else {
            let else_blk = self.new_block()?;
            self.jump(else_blk)?;
            self.terminated = false;
            self.b.switch_to(else_blk)?;
            self.lower_body(orelse)?;
            self.jump(end)?;
        }
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    /// `async for`: the iterator's `__anext__()` coroutine awaited per turn
    /// (spec section 12.5); `StopAsyncIteration` leaves the loop.
    #[allow(clippy::too_many_arguments)]
    fn lower_async_for(
        &mut self,
        target: &Expr,
        iter: &Expr,
        body: &[Stmt],
        orelse: &[Stmt],
        pos: Pos,
    ) -> Result<(), LowerError> {
        if !self.async_def {
            return Err(LowerError::Rejected {
                message: "'async for' outside async function".to_owned(),
                line: pos.line,
            });
        }
        let it_val = self.expr(iter)?;
        let aiter_m = self.method(&it_val, "__aiter__", pos)?;
        let it = self.call(aiter_m, vec![it_val], Vec::new(), pos)?;
        let types = self.except_tuple("StopAsyncIteration", pos)?;
        let head = self.new_block()?;
        let catch = self.new_block()?;
        let cont = self.new_block()?;
        let end = self.new_block()?;
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(head)?;
        // Only the __anext__ await is covered by the handler.
        self.b.set_handler(head, Some(catch))?;
        let anext_m = self.method(&it, "__anext__", pos)?;
        let anext = self.call(anext_m, vec![it], Vec::new(), pos)?;
        let next = self.await_of(anext, pos)?;
        self.jump(cont)?;
        self.terminated = false;
        self.b.switch_to(cont)?;
        self.assign_target(target, next, pos)?;
        self.unwinds.push(Unwind::Loop { end, head });
        self.lower_body(body)?;
        self.unwinds.pop();
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(catch)?;
        let matched = self.b.fresh(Type::Bool);
        let exc = self.b.fresh(Type::Top);
        self.emit(
            Instr::CatchBegin {
                types,
                matched,
                exc,
            },
            pos,
        )?;
        let clean = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(matched, clean, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        self.b.set_term(reraise_blk, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(clean)?;
        self.emit(Instr::CatchEnd, pos)?;
        if orelse.is_empty() {
            self.jump(end)?;
        } else {
            let else_blk = self.new_block()?;
            self.jump(else_blk)?;
            self.terminated = false;
            self.b.switch_to(else_blk)?;
            self.lower_body(orelse)?;
            self.jump(end)?;
        }
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    // ---- try / with / match ------------------------------------------

    /// The exception types an `except` clause tests: a syntactic tuple is
    /// flattened into the operand tuple, anything else is wrapped in a
    /// one-tuple (CPython's exc_match accepts nested tuples at runtime).
    fn except_types(&mut self, ty: &Expr, pos: Pos) -> Result<Value, LowerError> {
        match ty {
            Expr::Tuple { elts, .. } => {
                let mut items = Vec::with_capacity(elts.len());
                for e in elts {
                    items.push(self.expr(e)?);
                }
                self.build_tuple(items, pos)
            }
            other => {
                let v = self.expr(other)?;
                self.build_tuple(vec![v], pos)
            }
        }
    }

    fn base_exception_tuple(&mut self, pos: Pos) -> Result<Value, LowerError> {
        let be = self.load_plain_name("BaseException", pos)?;
        self.build_tuple(vec![be], pos)
    }

    /// A one-element exception tuple over a global class, as `CatchBegin`
    /// wants: `(<name>,)`.
    fn except_tuple(&mut self, name: &str, pos: Pos) -> Result<Value, LowerError> {
        let sym = self.pool(name);
        let cls = self.value_of(pos, move |into| Instr::LoadGlobal { name: sym, into })?;
        self.build_tuple(vec![cls], pos)
    }

    /// The next resume-point tag for a `Yield`/`YieldFrom` (spec section
    /// 12.5).
    fn next_state(&mut self) -> u32 {
        let state = self.yield_state;
        self.yield_state += 1;
        state
    }

    /// `await v` (spec section 12.5): the awaitable's `__await__()`, driven
    /// by a `YieldFrom`. Used by every await site (`await` expressions,
    /// `__anext__`, `__aenter__`/`__aexit__`).
    fn await_of(&mut self, v: Value, pos: Pos) -> Result<Value, LowerError> {
        let m = self.method(&v, "__await__", pos)?;
        let awaiter = self.call(m, vec![v], Vec::new(), pos)?;
        let send = self.none()?;
        let state = self.next_state();
        self.value_of(pos, move |into| Instr::YieldFrom {
            state,
            iter: awaiter,
            send,
            into,
        })
    }

    /// `await v`: `YieldFrom` over `v.__await__()`. Legal inside an `async
    /// def`, and inside a generator expression whose outermost clause is
    /// async (that scope is a coroutine); anything else is a compile error
    /// (CPython's compiler_visit_expr1).
    fn lower_await(&mut self, value: &Expr, pos: Pos) -> Result<Value, LowerError> {
        if !self.kind.function_like() {
            return Err(LowerError::Rejected {
                message: "'await' outside function".to_owned(),
                line: pos.line,
            });
        }
        if !self.async_def && !self.genexp_scope {
            return Err(LowerError::Rejected {
                message: "'await' outside async function".to_owned(),
                line: pos.line,
            });
        }
        let v = self.expr(value)?;
        self.await_of(v, pos)
    }

    /// PEP 479: a `StopIteration` escaping a generator-like body becomes a
    /// `RuntimeError` with the original exception as its cause. Pushes the
    /// wrapper's handler; `fill_stopiteration_landing` fills the landing
    /// once the body is complete. Every generator, coroutine and async
    /// generator body is wrapped (CPython's `wrap_in_stopiteration_handler`).
    fn push_stopiteration_wrapper(&mut self) -> Result<BlockId, LowerError> {
        let landing = self.new_block()?;
        self.handlers.push(landing);
        Ok(landing)
    }

    fn fill_stopiteration_landing(&mut self, landing: BlockId, pos: Pos) -> Result<(), LowerError> {
        let back = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        let back_terminated = self.terminated;
        self.terminated = false;
        self.b.switch_to(landing)?;
        let types = self.except_tuple("StopIteration", pos)?;
        let matched = self.b.fresh(Type::Bool);
        let exc = self.b.fresh(Type::Top);
        self.emit(
            Instr::CatchBegin {
                types,
                matched,
                exc,
            },
            pos,
        )?;
        let conv = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(matched, conv, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        self.b.set_term(reraise_blk, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(conv)?;
        self.emit(Instr::CatchEnd, pos)?;
        let msg = self.const_str("generator raised StopIteration", pos)?;
        let err_cls_sym = self.pool("RuntimeError");
        let err_cls = self.value_of(pos, move |into| Instr::LoadGlobal {
            name: err_cls_sym,
            into,
        })?;
        let err = self.call(err_cls, vec![msg], Vec::new(), pos)?;
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(
            at,
            Terminator::Raise {
                exc: Some(err),
                cause: Some(exc),
            },
        )?;
        self.terminated = true;
        self.terminated = back_terminated;
        self.b.switch_to(back)?;
        Ok(())
    }

    // ---- comprehensions (spec section 12.5, PEP 709) ---------------------

    /// A list/set/dict comprehension, inlined into the enclosing scope
    /// (PEP 709). The comprehension's `for` targets are isolated while it
    /// runs: an SSA local that shadows the outer binding, or a fresh cell
    /// when the comprehension itself made the target a cell (a nested
    /// function captured it). Generator expressions are separate functions
    /// (`lower_genexp`).
    fn lower_inlined_comp(
        &mut self,
        kind: CompKind,
        generators: &[Comprehension],
        elt: &Expr,
        value: Option<&Expr>,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let key = (pos.line as i32, pos.col as i32);
        let entry = self
            .low
            .root
            .inlined_comps
            .iter()
            .find(|(k, _)| *k == key)
            .ok_or(LowerError::UnexpectedAst {
                what: "missing inlined comprehension scope",
                line: pos.line,
            })?;
        let comp_coroutine = entry.1.coroutine;
        let comp_symbols: Vec<(String, i64)> = entry
            .1
            .symbols
            .iter()
            .map(|&(s, flags)| (self.text(s), flags))
            .collect();
        if comp_coroutine && !self.async_def && !self.genexp_scope {
            return Err(LowerError::Rejected {
                message: "asynchronous comprehension outside of an asynchronous function"
                    .to_owned(),
                line: pos.line,
            });
        }
        // The outermost iterator is evaluated in the enclosing scope, before
        // the swap (CPython's compiler_comprehension order).
        let iter_val = self.expr(&generators[0].iter)?;
        let outermost_async = generators[0].is_async != 0;
        let outer_it = if outermost_async {
            let m = self.method(&iter_val, "__aiter__", pos)?;
            self.call(m, vec![iter_val], Vec::new(), pos)?
        } else {
            self.value_of(pos, move |into| Instr::GetIter {
                obj: iter_val,
                into,
            })?
        };
        // CPython's push_inlined_comprehension_state: inside a class body the
        // comprehension runs against its own symbols wholesale (the class
        // namespace is invisible to it); elsewhere only the names whose
        // comprehension scope differs from the outer one are swapped, which
        // in practice is exactly the isolated targets.
        let in_class = self.kind == FnKind::Class && self.comp_frames.is_empty();
        let mut frame = CompFrame {
            overrides: HashMap::new(),
            saved: Vec::new(),
            saved_cells: Vec::new(),
        };
        for (raw_name, comp_flags) in comp_symbols {
            let key = self.mangle(&raw_name);
            let cs = scope_of(comp_flags);
            let ps = self.sym_map.get(&key).copied();
            let ps_scope = ps.map(scope_of).unwrap_or(0);
            let swap = in_class
                || (cs != ps_scope
                    && cs != symtable::FREE
                    && !(cs == symtable::CELL && ps_scope == symtable::FREE));
            if swap {
                frame.overrides.insert(key.clone(), comp_flags);
            }
            // A comp local without DEF_NONLOCAL is a `for` target.
            if comp_flags & symtable::DEF_LOCAL == 0 || comp_flags & symtable::DEF_NONLOCAL != 0 {
                continue;
            }
            if cs == symtable::CELL {
                // Save the outer cell and make a fresh one: nested functions
                // created inside the comprehension capture the fresh cell,
                // the outer binding survives untouched.
                let old = self.cell_of(&key, pos)?;
                let fresh = self.value_of(pos, |into| Instr::MakeCell {
                    initial: None,
                    into,
                })?;
                self.cells.insert(key.clone(), fresh);
                frame.saved_cells.push((key, old));
            } else if ps_scope == symtable::LOCAL {
                // The comprehension shadows an outer local; restore it when
                // the comprehension ends.
                let sym = self.pool(&key);
                let v = self.read_var_at_current(sym)?;
                frame.saved.push((key, v));
            }
        }
        self.comp_frames.push(frame);
        let container = match kind {
            CompKind::List => self.build_list(Vec::new(), pos)?,
            CompKind::Set => self.build_set(Vec::new(), pos)?,
            CompKind::Dict => self.build_dict(Vec::new(), pos)?,
            CompKind::Generator => {
                return Err(LowerError::UnexpectedAst {
                    what: "generator expression lowered as an inlined comprehension",
                    line: pos.line,
                });
            }
        };
        self.comp_generators(
            kind,
            generators,
            0,
            elt,
            value,
            Some(outer_it),
            container,
            pos,
        )?;
        let frame = self.comp_frames.pop().expect("frame pushed above");
        // CPython's pop_inlined_comprehension_state.
        for (name, old) in frame.saved_cells {
            self.cells.insert(name, old);
        }
        for (name, v) in frame.saved {
            let sym = self.pool(&name);
            self.write_var_at_current(sym, v)?;
        }
        Ok(container)
    }

    /// The `for` clauses of a comprehension, innermost recursive: the last
    /// clause evaluates the element into the container. `outer_iter` carries
    /// the already-iterated outermost iterator.
    #[allow(clippy::too_many_arguments)]
    fn comp_generators(
        &mut self,
        kind: CompKind,
        generators: &[Comprehension],
        gen_index: usize,
        elt: &Expr,
        value: Option<&Expr>,
        outer_iter: Option<Value>,
        container: Value,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let g = &generators[gen_index];
        let it = match outer_iter {
            Some(v) => v,
            None => {
                let v = self.expr(&g.iter)?;
                if g.is_async != 0 {
                    let m = self.method(&v, "__aiter__", pos)?;
                    self.call(m, vec![v], Vec::new(), pos)?
                } else {
                    self.value_of(pos, move |into| Instr::GetIter { obj: v, into })?
                }
            }
        };
        if g.is_async != 0 {
            self.async_comp_loop(kind, generators, gen_index, elt, value, it, container, pos)
        } else {
            self.sync_comp_loop(kind, generators, gen_index, elt, value, it, container, pos)
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sync_comp_loop(
        &mut self,
        kind: CompKind,
        generators: &[Comprehension],
        gen_index: usize,
        elt: &Expr,
        value: Option<&Expr>,
        it: Value,
        container: Value,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let g = &generators[gen_index];
        let types = self.except_tuple("StopIteration", pos)?;
        let head = self.new_block()?;
        let catch = self.new_block()?;
        let body = self.new_block()?;
        let after = self.new_block()?;
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(head)?;
        self.b.set_handler(head, Some(catch))?;
        let next = self.value_of(pos, |into| Instr::Next { iter: it, into })?;
        // The target is written in the normal successor only: the handler
        // edge reads end-of-block definitions, where the Next result is
        // undefined.
        self.jump(body)?;
        self.terminated = false;
        self.b.switch_to(body)?;
        self.assign_target(&g.target, next, pos)?;
        for if_expr in &g.ifs {
            let cond = self.truth_of(if_expr)?;
            let next_blk = self.new_block()?;
            self.branch(cond, next_blk, head)?;
            self.terminated = false;
            self.b.switch_to(next_blk)?;
        }
        if gen_index + 1 < generators.len() {
            self.comp_generators(
                kind,
                generators,
                gen_index + 1,
                elt,
                value,
                None,
                container,
                pos,
            )?;
        } else {
            self.comp_element(kind, elt, value, container, pos)?;
        }
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(catch)?;
        let matched = self.b.fresh(Type::Bool);
        let exc = self.b.fresh(Type::Top);
        self.emit(
            Instr::CatchBegin {
                types,
                matched,
                exc,
            },
            pos,
        )?;
        let clean = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(matched, clean, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        self.b.set_term(reraise_blk, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(clean)?;
        self.emit(Instr::CatchEnd, pos)?;
        self.jump(after)?;
        self.terminated = false;
        self.b.switch_to(after)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn async_comp_loop(
        &mut self,
        kind: CompKind,
        generators: &[Comprehension],
        gen_index: usize,
        elt: &Expr,
        value: Option<&Expr>,
        it: Value,
        container: Value,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let g = &generators[gen_index];
        let types = self.except_tuple("StopAsyncIteration", pos)?;
        let head = self.new_block()?;
        let catch = self.new_block()?;
        let cont = self.new_block()?;
        let after = self.new_block()?;
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(head)?;
        // Only the __anext__ await is covered by the handler (CPython's
        // SETUP_FINALLY placement in the async comprehension generator).
        self.b.set_handler(head, Some(catch))?;
        let anext_m = self.method(&it, "__anext__", pos)?;
        let anext = self.call(anext_m, vec![it], Vec::new(), pos)?;
        let next = self.await_of(anext, pos)?;
        self.jump(cont)?;
        self.terminated = false;
        self.b.switch_to(cont)?;
        self.assign_target(&g.target, next, pos)?;
        for if_expr in &g.ifs {
            let cond = self.truth_of(if_expr)?;
            let next_blk = self.new_block()?;
            self.branch(cond, next_blk, head)?;
            self.terminated = false;
            self.b.switch_to(next_blk)?;
        }
        if gen_index + 1 < generators.len() {
            self.comp_generators(
                kind,
                generators,
                gen_index + 1,
                elt,
                value,
                None,
                container,
                pos,
            )?;
        } else {
            self.comp_element(kind, elt, value, container, pos)?;
        }
        self.jump(head)?;
        self.terminated = false;
        self.b.switch_to(catch)?;
        let matched = self.b.fresh(Type::Bool);
        let exc = self.b.fresh(Type::Top);
        self.emit(
            Instr::CatchBegin {
                types,
                matched,
                exc,
            },
            pos,
        )?;
        let clean = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(matched, clean, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        self.b.set_term(reraise_blk, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(clean)?;
        self.emit(Instr::CatchEnd, pos)?;
        self.jump(after)?;
        self.terminated = false;
        self.b.switch_to(after)?;
        Ok(())
    }

    /// The element expression of the innermost clause: appended to the
    /// container, or yielded in a generator expression. A dict comprehension
    /// evaluates its key before its value; the symtable visited them the
    /// other way, so the children swap here.
    fn comp_element(
        &mut self,
        kind: CompKind,
        elt: &Expr,
        value: Option<&Expr>,
        container: Value,
        pos: Pos,
    ) -> Result<(), LowerError> {
        match kind {
            CompKind::Generator => {
                let v = self.expr(elt)?;
                let state = self.next_state();
                self.value_of(pos, move |into| Instr::Yield {
                    state,
                    value: v,
                    into,
                })?;
            }
            CompKind::List => {
                let v = self.expr(elt)?;
                let m = self.method(&container, "append", pos)?;
                self.call(m, vec![v], Vec::new(), pos)?;
            }
            CompKind::Set => {
                let v = self.expr(elt)?;
                let m = self.method(&container, "add", pos)?;
                self.call(m, vec![v], Vec::new(), pos)?;
            }
            CompKind::Dict => {
                let n_value = value.map(expr_scopes).unwrap_or(0);
                let n_key = expr_scopes(elt);
                if n_value != 0 || n_key != 0 {
                    self.requeue(n_value, n_key);
                }
                let k = self.expr(elt)?;
                let v = match value {
                    Some(v) => self.expr(v)?,
                    None => {
                        return Err(LowerError::UnexpectedAst {
                            what: "dict comprehension without a value expression",
                            line: pos.line,
                        });
                    }
                };
                self.emit(
                    Instr::SetItem {
                        obj: container,
                        key: k,
                        value: v,
                    },
                    pos,
                )?;
            }
        }
        Ok(())
    }

    /// A generator expression: a generator function of one parameter, `.0`,
    /// receiving the outermost iterator pre-iterated, its whole body wrapped
    /// so a `StopIteration` raised inside becomes a `RuntimeError` (PEP 479).
    fn lower_genexp(
        &mut self,
        generators: &[Comprehension],
        elt: &Expr,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        // The symtable placed the generator expression's scope after the
        // outermost iterator's scopes; the function value is built first.
        let n_iter = expr_scopes(&generators[0].iter);
        self.requeue(n_iter, 1);
        let scope = self.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing generator expression scope",
            line: pos.line,
        })?;
        let qualname = self.child_qualname("<genexpr>", FnKind::Function);
        let fid = self.low.reserve_function();
        let cells = self.child_cells(scope, pos)?;
        let params = ParamSpec {
            positional: vec![".0".to_owned()],
            vararg: None,
            kwonly: Vec::new(),
            kwarg: None,
        };
        let private = self.private.clone();
        let mangled = self.mangled.clone();
        let mut child = self.low.new_fn(
            scope,
            FnKind::Function,
            "<genexpr>",
            &qualname,
            params,
            private,
            mangled,
            (Flavor::Function, scope),
        );
        child.genexp_scope = true;
        let landing = child.push_stopiteration_wrapper()?;
        child.comp_generators(
            CompKind::Generator,
            generators,
            0,
            elt,
            None,
            Some(Value(0)),
            Value(0),
            pos,
        )?;
        child.return_none()?;
        child.fill_stopiteration_landing(landing, pos)?;
        child.finish_into(fid)?;
        let none_v = self.none()?;
        let fv = self.make_function_value(fid, cells, none_v, none_v, pos)?;
        let iter_val = self.expr(&generators[0].iter)?;
        let it = if generators[0].is_async != 0 {
            let m = self.method(&iter_val, "__aiter__", pos)?;
            self.call(m, vec![iter_val], Vec::new(), pos)?
        } else {
            self.value_of(pos, move |into| Instr::GetIter {
                obj: iter_val,
                into,
            })?
        };
        self.call(fv, vec![it], Vec::new(), pos)
    }

    /// Restore the `exc_info` stack and delete the bound name when control
    /// leaves an `except` handler body (CPython's HANDLER_CLEANUP unwind).
    fn unwind_handler_cleanup(&mut self, name: Option<FSym>, pos: Pos) -> Result<(), LowerError> {
        self.emit(Instr::PopExcInfo, pos)?;
        if let Some(nm) = name {
            let v = self.none()?;
            self.store_name_value(nm, v, pos)?;
            self.del_name(nm, pos)?;
        }
        Ok(())
    }

    /// The bound `__exit__(None, None, None)` call a `with` item needs when
    /// control leaves it by `break`, `continue` or `return`, in a block
    /// outside the item's own handler (CPython pops the block first).
    /// The handler in effect once every frame in `self.unwinds[depth..]` has
    /// been exited. Unwinding a `with` calls `__exit__` from outside the
    /// with's own region: if that call raises, the exception belongs to the
    /// enclosing handler, not the with's landing, which would call `__exit__`
    /// a second time.
    fn handler_outside(&self, depth: usize) -> Option<BlockId> {
        let mut live = self.handlers.len();
        for frame in &self.unwinds[depth..] {
            match frame {
                Unwind::Loop { .. } | Unwind::HandlerCleanup { .. } | Unwind::Finalbody { .. } => {}
                Unwind::Handler
                | Unwind::With { .. }
                | Unwind::WithAsync { .. }
                | Unwind::Finally { .. }
                | Unwind::StarHandler => live -= 1,
            }
        }
        if live == 0 {
            None
        } else {
            Some(self.handlers[live - 1])
        }
    }

    fn unwind_with_exit(
        &mut self,
        exit_m: Value,
        handler: Option<BlockId>,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let exit_blk = self.b.new_block();
        if let Some(h) = handler {
            self.b.set_handler(exit_blk, Some(h))?;
        }
        self.jump(exit_blk)?;
        self.terminated = false;
        self.b.switch_to(exit_blk)?;
        let none_v = self.none()?;
        self.call(exit_m, vec![none_v, none_v, none_v], Vec::new(), pos)?;
        Ok(())
    }

    /// The awaited bound `__aexit__(None, None, None)` an `async with` item
    /// needs when control leaves it by `break`, `continue` or `return`.
    fn unwind_with_async_exit(
        &mut self,
        exit_m: Value,
        handler: Option<BlockId>,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let exit_blk = self.b.new_block();
        if let Some(h) = handler {
            self.b.set_handler(exit_blk, Some(h))?;
        }
        self.jump(exit_blk)?;
        self.terminated = false;
        self.b.switch_to(exit_blk)?;
        let none_v = self.none()?;
        let r = self.call(exit_m, vec![none_v, none_v, none_v], Vec::new(), pos)?;
        self.await_of(r, pos)?;
        Ok(())
    }

    fn finally_sentinel(&mut self, seq: usize, keep_going: bool) -> Result<Value, LowerError> {
        let name = if keep_going {
            format!(".continue{seq}")
        } else {
            format!(".break{seq}")
        };
        let sym = self.pool(&name);
        let idx = self.low.consts.const_idx(Const::Str(sym));
        self.value_of(pos_of(0, 0), move |into| Instr::Const { idx, into })
    }

    /// Unwind the region stack the way `break` (or `continue`) does: act on
    /// each region crossed until the loop, then jump its exit block. The
    /// stack itself is left alone; the statements after the jump are dead.
    fn unwind_to_loop(&mut self, keep_going: bool, pos: Pos) -> Result<(), LowerError> {
        // First pass: mark every crossed finally that its dispatch will see
        // this sentinel, before anything is emitted.
        let mut depth = self.unwinds.len();
        while depth > 0 {
            depth -= 1;
            match &mut self.unwinds[depth] {
                Unwind::Loop { .. } => break,
                Unwind::StarHandler => {
                    return Err(LowerError::UnexpectedAst {
                        what: "'break', 'continue' and 'return' cannot appear in an except* block",
                        line: pos.line,
                    });
                }
                Unwind::Finally {
                    break_sent,
                    continue_sent,
                    ..
                } => {
                    if keep_going {
                        *continue_sent = true;
                    } else {
                        *break_sent = true;
                    }
                }
                Unwind::Finalbody { .. }
                | Unwind::Handler
                | Unwind::HandlerCleanup { .. }
                | Unwind::With { .. }
                | Unwind::WithAsync { .. } => {}
            }
        }
        // Second pass: emit the unwinding.
        let mut depth = self.unwinds.len();
        while depth > 0 {
            depth -= 1;
            match self.unwinds[depth] {
                Unwind::Loop { end, head } => {
                    let to = if keep_going { head } else { end };
                    self.jump(to)?;
                    return Ok(());
                }
                Unwind::Handler => {}
                Unwind::Finalbody { pending } => {
                    self.pop_if_pending(pending, pos)?;
                }
                Unwind::HandlerCleanup { name } => {
                    self.unwind_handler_cleanup(name, pos)?;
                }
                Unwind::With { exit } => {
                    let handler = self.handler_outside(depth);
                    self.unwind_with_exit(exit, handler, pos)?;
                }
                Unwind::WithAsync { exit } => {
                    let handler = self.handler_outside(depth);
                    self.unwind_with_async_exit(exit, handler, pos)?;
                }
                Unwind::Finally {
                    var,
                    pending,
                    entry,
                    seq,
                    ..
                } => {
                    // The sentinel const is re-emitted per site: one shared
                    // definition could not dominate every break. The pool
                    // dedups the string, so identity still holds.
                    let reason = self.finally_sentinel(seq, keep_going)?;
                    let false_v = self.const_val(Const::Bool(false), pos)?;
                    let write_blk = self.new_block()?;
                    self.jump(write_blk)?;
                    self.terminated = false;
                    self.b.switch_to(write_blk)?;
                    self.write_var_at_current(var, reason)?;
                    self.write_var_at_current(pending, false_v)?;
                    self.jump(entry)?;
                    return Ok(());
                }
                Unwind::StarHandler => unreachable!("the first pass rejected it"),
            }
        }
        Err(LowerError::UnexpectedAst {
            what: "'break'/'continue' outside a loop",
            line: pos.line,
        })
    }

    /// `PopExcInfo` only when the finally's landing pushed an in-flight
    /// exception (the reason slot says which exits carry one).
    fn pop_if_pending(&mut self, pending: Sym, pos: Pos) -> Result<(), LowerError> {
        let pend = self.read_var_at_current(pending)?;
        let on = self.unboxed_bool(pos, move |into| Instr::Truth { value: pend, into })?;
        let pop_blk = self.new_block()?;
        let keep_blk = self.new_block()?;
        let after = self.new_block()?;
        self.branch(on, pop_blk, keep_blk)?;
        self.terminated = false;
        self.b.switch_to(pop_blk)?;
        self.emit(Instr::PopExcInfo, pos)?;
        self.jump(after)?;
        self.terminated = false;
        self.b.switch_to(keep_blk)?;
        self.jump(after)?;
        self.terminated = false;
        self.b.switch_to(after)?;
        Ok(())
    }

    /// Unwind every region the way `return` does, then return `value`.
    fn unwind_and_return(&mut self, value: Value, pos: Pos) -> Result<(), LowerError> {
        // First pass: mark every crossed finally that its dispatch will see
        // a wrapped return value, before anything is emitted (the dispatch
        // only lowers that arm when some return actually crossed).
        let mut depth = self.unwinds.len();
        while depth > 0 {
            depth -= 1;
            match &mut self.unwinds[depth] {
                Unwind::Finally { return_sent, .. } => *return_sent = true,
                Unwind::StarHandler => {
                    return Err(LowerError::UnexpectedAst {
                        what: "'break', 'continue' and 'return' cannot appear in an except* block",
                        line: pos.line,
                    });
                }
                Unwind::Loop { .. }
                | Unwind::Finalbody { .. }
                | Unwind::Handler
                | Unwind::HandlerCleanup { .. }
                | Unwind::With { .. }
                | Unwind::WithAsync { .. } => {}
            }
        }
        // Second pass: emit the unwinding.
        let mut depth = self.unwinds.len();
        while depth > 0 {
            depth -= 1;
            match self.unwinds[depth] {
                Unwind::Loop { .. } | Unwind::Handler => {}
                Unwind::Finalbody { pending } => {
                    self.pop_if_pending(pending, pos)?;
                }
                Unwind::HandlerCleanup { name } => {
                    self.unwind_handler_cleanup(name, pos)?;
                }
                Unwind::With { exit } => {
                    let handler = self.handler_outside(depth);
                    self.unwind_with_exit(exit, handler, pos)?;
                }
                Unwind::WithAsync { exit } => {
                    let handler = self.handler_outside(depth);
                    self.unwind_with_async_exit(exit, handler, pos)?;
                }
                Unwind::Finally {
                    var,
                    pending,
                    entry,
                    ..
                } => {
                    // The value rides to the dispatch in a one-tuple so an
                    // exception in flight (also in the slot) stays
                    // distinguishable from it. The write goes in a fresh
                    // block without may-raise instructions: written next to
                    // the value's computation it would ride the exception
                    // edge into the landing's phis.
                    let reason = self.build_tuple(vec![value], pos)?;
                    let false_v = self.const_val(Const::Bool(false), pos)?;
                    let write_blk = self.new_block()?;
                    self.jump(write_blk)?;
                    self.terminated = false;
                    self.b.switch_to(write_blk)?;
                    self.write_var_at_current(var, reason)?;
                    self.write_var_at_current(pending, false_v)?;
                    self.jump(entry)?;
                    return Ok(());
                }
                Unwind::StarHandler => unreachable!("the first pass rejected it"),
            }
        }
        self.ret(value)
    }

    fn lower_try(
        &mut self,
        body: &[Stmt],
        handlers: &[Excepthandler],
        orelse: &[Stmt],
        finalbody: &[Stmt],
        star: bool,
        pos: Pos,
    ) -> Result<(), LowerError> {
        if finalbody.is_empty() {
            return if star {
                self.lower_try_star(body, handlers, orelse, pos)
            } else {
                self.lower_try_except(body, handlers, orelse, pos)
            };
        }
        // try/finally: one reason slot; the finally body is lowered once and
        // a dispatch on the reason continues the control flow that crossed
        // it (CPython instead inlines the body at every exit). `pending_var`
        // records whether the landing pushed the in-flight exception, so an
        // exit that abandons it (break, continue, return in the finalbody)
        // can restore the exc_info stack, as CPython's finally does.
        let reason_var = self.fresh_join_var_named("finally");
        let pending_var = self.fresh_join_var_named("finpending");
        let slot = self.b.fresh(Type::None);
        self.emit(Instr::FinallyEnter { into: slot }, pos)?;
        self.write_var_at_current(reason_var, slot)?;
        let false_v = self.const_val(Const::Bool(false), pos)?;
        self.write_var_at_current(pending_var, false_v)?;
        let seq = self.finally_count;
        self.finally_count += 1;
        let landing = self.new_block()?;
        let entry = self.new_block()?;
        self.handlers.push(landing);
        self.unwinds.push(Unwind::Finally {
            var: reason_var,
            pending: pending_var,
            entry,
            seq,
            break_sent: false,
            continue_sent: false,
            return_sent: false,
        });
        self.enter_region(landing)?;
        if handlers.is_empty() {
            self.lower_body(body)?;
        } else if star {
            self.lower_try_star(body, handlers, orelse, pos)?;
        } else {
            self.lower_try_except(body, handlers, orelse, pos)?;
        }
        let (break_sent, continue_sent, return_sent) = match self.unwinds.pop() {
            Some(Unwind::Finally {
                break_sent,
                continue_sent,
                return_sent,
                ..
            }) => (break_sent, continue_sent, return_sent),
            _ => unreachable!("pushed above"),
        };
        self.handlers.pop();
        if !self.terminated {
            self.jump(entry)?;
        }
        self.terminated = false;
        self.b.switch_to(entry)?;
        // The finally frame is gone: a break, continue or return inside the
        // finalbody unwinds outwards directly, abandoning the reason (the
        // finalbody's own frame below restores the exc_info stack when it
        // abandons an in-flight exception).
        self.unwinds.push(Unwind::Finalbody {
            pending: pending_var,
        });
        self.lower_body(finalbody)?;
        self.unwinds.pop();
        // The landing: the in-flight exception rides the same slot, and the
        // finalbody runs with it current, as CPython's pushed exc_info. Fill
        // it before the dispatch: a finalbody that itself raises, returns or
        // breaks abandons the reason, but the landing stays the target of the
        // body's exception edges and must terminate.
        let after_final = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        let finalbody_terminated = self.terminated;
        self.terminated = false;
        self.b.switch_to(landing)?;
        let exc = self.value_of(pos, |into| Instr::PushExcInfo { into })?;
        self.write_var_at_current(reason_var, exc)?;
        let true_v = self.const_val(Const::Bool(true), pos)?;
        self.write_var_at_current(pending_var, true_v)?;
        self.jump(entry)?;
        self.terminated = false;
        self.b.switch_to(after_final)?;
        if finalbody_terminated {
            self.terminated = true;
            return Ok(());
        }
        let reason = self.read_var_at_current(reason_var)?;
        self.emit(Instr::FinallyExit { reason }, pos)?;
        // Dispatch: None continues past the statement, the sentinels resume
        // the break/continue that crossed, anything else is the wrapped
        // return value.
        let normal = self.new_block()?;
        let none_v = self.none()?;
        let is_none = self.unboxed_bool(pos, move |into| Instr::Compare {
            op: CmpOp::Is,
            lhs: reason,
            rhs: none_v,
            into,
        })?;
        let sent_blk = self.new_block()?;
        self.branch(is_none, normal, sent_blk)?;
        self.terminated = false;
        self.b.switch_to(sent_blk)?;
        let brk_arm = if break_sent {
            Some(self.new_block()?)
        } else {
            None
        };
        let cont_arm = if continue_sent {
            Some(self.new_block()?)
        } else {
            None
        };
        // With no sentinel to test the fallthrough handles the wrapped
        // return directly.
        let ret_arm = match (brk_arm, cont_arm) {
            (Some(_), _) | (None, Some(_)) => Some(self.new_block()?),
            (None, None) => None,
        };
        if break_sent {
            let arm = brk_arm.expect("created above");
            let next = match cont_arm {
                Some(_) => self.new_block()?,
                None => ret_arm.expect("created above"),
            };
            let b = self.finally_sentinel(seq, false)?;
            let c = self.unboxed_bool(pos, move |into| Instr::Compare {
                op: CmpOp::Is,
                lhs: reason,
                rhs: b,
                into,
            })?;
            self.branch(c, arm, next)?;
            self.terminated = false;
            self.b.switch_to(next)?;
        }
        if continue_sent {
            let arm = cont_arm.expect("created above");
            let next = ret_arm.expect("created above");
            let cv = self.finally_sentinel(seq, true)?;
            let c = self.unboxed_bool(pos, move |into| Instr::Compare {
                op: CmpOp::Is,
                lhs: reason,
                rhs: cv,
                into,
            })?;
            self.branch(c, arm, next)?;
            self.terminated = false;
            self.b.switch_to(next)?;
        }
        if return_sent {
            let value = self.b.fresh(Type::Top);
            self.emit(
                Instr::Unpack {
                    src: reason,
                    n: 1,
                    star: None,
                    into: Box::from([value]),
                },
                pos,
            )?;
            self.unwind_and_return(value, pos)?;
        } else {
            // No return crossed this finally, so nothing wraps a value in
            // the slot: the only reasons past the sentinel tests are
            // exceptions, which FinallyExit above never lets through. The
            // fallthrough is unreachable but must still terminate.
            let at = self
                .b
                .current_block()
                .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
            self.b.set_term(at, Terminator::Reraise)?;
            self.terminated = true;
        }
        if let Some(arm) = brk_arm {
            self.terminated = false;
            self.b.switch_to(arm)?;
            self.unwind_to_loop(false, pos)?;
        }
        if let Some(arm) = cont_arm {
            self.terminated = false;
            self.b.switch_to(arm)?;
            self.unwind_to_loop(true, pos)?;
        }
        self.terminated = false;
        self.b.switch_to(normal)?;
        Ok(())
    }

    fn lower_try_except(
        &mut self,
        body: &[Stmt],
        handlers: &[Excepthandler],
        orelse: &[Stmt],
        pos: Pos,
    ) -> Result<(), LowerError> {
        let landing = self.new_block()?;
        self.handlers.push(landing);
        self.unwinds.push(Unwind::Handler);
        self.enter_region(landing)?;
        self.lower_body(body)?;
        self.unwinds.pop();
        self.handlers.pop();
        // The orelse runs outside the protected region, as in CPython.
        if self.terminated && !orelse.is_empty() {
            let dead = self.new_block()?;
            self.terminated = false;
            self.b.switch_to(dead)?;
        }
        self.lower_body(orelse)?;
        let end = self.new_block()?;
        if !self.terminated {
            self.jump(end)?;
        }
        // The landing reads the raise-point locals: the builder merges every
        // predecessor's end-of-block values over the exception edges, and the
        // spill model keeps each definition before its block's first
        // may-raise (see `write_var_at_current`), so those values are exactly
        // the ones the raising instruction saw.
        self.terminated = false;
        self.b.switch_to(landing)?;
        let exc0 = self.value_of(pos, |into| Instr::PushExcInfo { into })?;
        for h in handlers {
            let Excepthandler::ExceptHandler {
                r#type,
                name,
                body: hbody,
                ..
            } = h;
            // A bare `except:` is a match against BaseException, exactly as
            // CPython's unconditional CHECK_EXC_MATCH behaves.
            let types = match r#type {
                Some(t) => self.except_types(t, pos)?,
                None => self.base_exception_tuple(pos)?,
            };
            let matched = self.b.fresh(Type::Bool);
            let exc = self.b.fresh(Type::Top);
            self.emit(
                Instr::CatchBegin {
                    types,
                    matched,
                    exc,
                },
                pos,
            )?;
            let hbody_blk = self.new_block()?;
            let next_blk = self.new_block()?;
            self.branch(matched, hbody_blk, next_blk)?;
            self.terminated = false;
            self.b.switch_to(hbody_blk)?;
            if let Some(nm) = name {
                self.store_name_value(*nm, exc0, pos)?;
            }
            // The handler body's own exceptions and unwinding restore the
            // exc_info stack and delete the bound name on their way out.
            self.unwinds.push(Unwind::HandlerCleanup { name: *name });
            self.lower_body(hbody)?;
            self.unwinds.pop();
            if !self.terminated {
                self.emit(Instr::CatchEnd, pos)?;
                if let Some(nm) = name {
                    let v = self.none()?;
                    self.store_name_value(*nm, v, pos)?;
                    self.del_name(*nm, pos)?;
                }
                self.emit(Instr::PopExcInfo, pos)?;
                self.jump(end)?;
            }
            self.terminated = false;
            self.b.switch_to(next_blk)?;
        }
        // No handler matched: restore the exc_info stack and re-raise.
        self.emit(Instr::PopExcInfo, pos)?;
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(at, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    fn lower_try_star(
        &mut self,
        body: &[Stmt],
        handlers: &[Excepthandler],
        orelse: &[Stmt],
        pos: Pos,
    ) -> Result<(), LowerError> {
        let landing = self.new_block()?;
        self.handlers.push(landing);
        self.unwinds.push(Unwind::Handler);
        self.enter_region(landing)?;
        self.lower_body(body)?;
        self.unwinds.pop();
        self.handlers.pop();
        if self.terminated && !orelse.is_empty() {
            let dead = self.new_block()?;
            self.terminated = false;
            self.b.switch_to(dead)?;
        }
        self.lower_body(orelse)?;
        let end = self.new_block()?;
        if !self.terminated {
            self.jump(end)?;
        }
        self.terminated = false;
        self.b.switch_to(landing)?;
        // The original exception and the outcome list, defined before any
        // may-raise instruction: the exception edges out of this block can
        // read them.
        let orig = self.value_of(pos, |into| Instr::PushExcInfo { into })?;
        let res = self.build_list(Vec::new(), pos)?;
        let rest_var = self.fresh_join_var_named("egrest");
        self.write_var_at_current(rest_var, orig)?;
        for h in handlers {
            let Excepthandler::ExceptHandler {
                r#type,
                name,
                body: hbody,
                ..
            } = h;
            let Some(ty) = r#type else {
                return Err(LowerError::UnexpectedAst {
                    what: "except* without a type",
                    line: pos.line,
                });
            };
            let cur_rest = self.read_var_at_current(rest_var)?;
            let types = self.except_types(ty, pos)?;
            let matched = self.b.fresh(Type::Bool);
            let m = self.b.fresh(Type::Top);
            let rest = self.b.fresh(Type::Top);
            self.emit(
                Instr::CatchStarBegin {
                    src: cur_rest,
                    types,
                    matched,
                    exc: m,
                    rest,
                },
                pos,
            )?;
            // Whichever way the handler ends, the next one splits the rest.
            self.write_var_at_current(rest_var, rest)?;
            let hcatch = self.new_block()?;
            let next_blk = self.new_block()?;
            // break, continue and return cannot leave an except* block, and
            // the body's own exceptions land in the catch below.
            self.handlers.push(hcatch);
            self.unwinds.push(Unwind::StarHandler);
            let hbody_blk = self.new_block()?;
            self.branch(matched, hbody_blk, next_blk)?;
            self.terminated = false;
            self.b.switch_to(hbody_blk)?;
            if let Some(nm) = name {
                self.store_name_value(*nm, m, pos)?;
            }
            self.lower_body(hbody)?;
            self.unwinds.pop();
            self.handlers.pop();
            if !self.terminated {
                self.emit(Instr::CatchEnd, pos)?;
                if let Some(nm) = name {
                    let v = self.none()?;
                    self.store_name_value(*nm, v, pos)?;
                    self.del_name(*nm, pos)?;
                }
                self.jump(next_blk)?;
            }
            self.terminated = false;
            self.b.switch_to(hcatch)?;
            // An exception raised in the handler body joins the outcome
            // list; the remaining handlers still run against the rest, which
            // reaches this block over the body's exception edges with its
            // raise-point value.
            let e2 = self.b.fresh(Type::Top);
            {
                let types = self.base_exception_tuple(pos)?;
                let m2 = self.b.fresh(Type::Bool);
                self.emit(
                    Instr::CatchBegin {
                        types,
                        matched: m2,
                        exc: e2,
                    },
                    pos,
                )?;
            }
            if let Some(nm) = name {
                let v = self.none()?;
                self.store_name_value(*nm, v, pos)?;
                self.del_name(*nm, pos)?;
            }
            let append = self.method(&res, "append", pos)?;
            self.call(append, vec![e2], Vec::new(), pos)?;
            self.emit(Instr::CatchEnd, pos)?;
            self.jump(next_blk)?;
            self.terminated = false;
            self.b.switch_to(next_blk)?;
        }
        // The unmatched remainder (None when everything was consumed) joins
        // the list; the combined outcome decides whether anything propagates.
        let final_rest = self.read_var_at_current(rest_var)?;
        let append = self.method(&res, "append", pos)?;
        self.call(append, vec![final_rest], Vec::new(), pos)?;
        let to_raise = self.value_of(pos, |into| Instr::PrepReraiseStar {
            orig,
            excs: res,
            into,
        })?;
        let none_v = self.none()?;
        let handled = self.unboxed_bool(pos, move |into| Instr::Compare {
            op: CmpOp::Is,
            lhs: to_raise,
            rhs: none_v,
            into,
        })?;
        let pop_blk = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(handled, pop_blk, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(pop_blk)?;
        self.emit(Instr::PopExcInfo, pos)?;
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        self.emit(Instr::PopExcInfo, pos)?;
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(
            at,
            Terminator::Raise {
                exc: Some(to_raise),
                // CPython's tail is a bare RERAISE: the prepared group
                // keeps its own context and chains implicitly.
                cause: None,
            },
        )?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    /// `with` items nest: each item's region encloses the rest, so the inner
    /// items' exits run first, as in CPython's recursive compiler_with.
    fn lower_with(
        &mut self,
        items: &[Withitem],
        body: &[Stmt],
        pos: Pos,
    ) -> Result<(), LowerError> {
        let Some(item) = items.first() else {
            return self.lower_body(body);
        };
        let Withitem {
            context_expr,
            optional_vars,
        } = item;
        // __exit__ resolves before __enter__ runs, and neither is covered by
        // this item's handler. Both are the bound methods, so the manager
        // itself is not an argument (CPython calls the bound __enter__ with
        // no arguments and __exit__ with three).
        let mgr = self.expr(context_expr)?;
        let exit_m = self.method(&mgr, "__exit__", pos)?;
        let enter_m = self.method(&mgr, "__enter__", pos)?;
        let value = self.call(enter_m, Vec::new(), Vec::new(), pos)?;
        if let Some(vars) = optional_vars {
            self.assign_target(vars, value, pos)?;
        }
        let landing = self.new_block()?;
        self.handlers.push(landing);
        self.unwinds.push(Unwind::With { exit: exit_m });
        self.enter_region(landing)?;
        self.lower_with(&items[1..], body, pos)?;
        self.unwinds.pop();
        self.handlers.pop();
        let end = self.new_block()?;
        if !self.terminated {
            // Normal path: exit(None, None, None) outside the region.
            let exit_blk = self.new_block()?;
            self.jump(exit_blk)?;
            self.terminated = false;
            self.b.switch_to(exit_blk)?;
            let none_v = self.none()?;
            self.call(exit_m, vec![none_v, none_v, none_v], Vec::new(), pos)?;
            self.jump(end)?;
        }
        self.terminated = false;
        self.b.switch_to(landing)?;
        let exc = self.value_of(pos, |into| Instr::PushExcInfo { into })?;
        let ty = self.call_global("type", vec![exc], pos)?;
        let tb_sym = self.pool("__traceback__");
        let tb = self.value_of(pos, move |into| Instr::GetAttr {
            obj: exc,
            name: tb_sym,
            into,
        })?;
        let r = self.call(exit_m, vec![ty, exc, tb], Vec::new(), pos)?;
        let suppressed = self.unboxed_bool(pos, move |into| Instr::Truth { value: r, into })?;
        let supp_blk = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(suppressed, supp_blk, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(supp_blk)?;
        self.emit(Instr::PopExcInfo, pos)?;
        // The suppressed exception never propagates, so the raise-point
        // locals stay as the body left them (CPython keeps them too).
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        let at = reraise_blk;
        self.b.set_term(at, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    /// `async with`: like `with`, with `__aenter__`/`__aexit__` awaited
    /// (spec section 12.5).
    fn lower_async_with(
        &mut self,
        items: &[Withitem],
        body: &[Stmt],
        pos: Pos,
    ) -> Result<(), LowerError> {
        if !self.async_def {
            return Err(LowerError::Rejected {
                message: "'async with' outside async function".to_owned(),
                line: pos.line,
            });
        }
        let Some(item) = items.first() else {
            return self.lower_body(body);
        };
        let Withitem {
            context_expr,
            optional_vars,
        } = item;
        // __aexit__ resolves before __aenter__ runs, and neither is covered
        // by this item's handler. Both are the bound methods, awaited where
        // CPython awaits them, with no manager argument.
        let mgr = self.expr(context_expr)?;
        let exit_m = self.method(&mgr, "__aexit__", pos)?;
        let enter_m = self.method(&mgr, "__aenter__", pos)?;
        let aenter = self.call(enter_m, Vec::new(), Vec::new(), pos)?;
        let value = self.await_of(aenter, pos)?;
        if let Some(vars) = optional_vars {
            self.assign_target(vars, value, pos)?;
        }
        let landing = self.new_block()?;
        self.handlers.push(landing);
        self.unwinds.push(Unwind::WithAsync { exit: exit_m });
        self.enter_region(landing)?;
        self.lower_async_with(&items[1..], body, pos)?;
        self.unwinds.pop();
        self.handlers.pop();
        let end = self.new_block()?;
        if !self.terminated {
            // Normal path: await aexit(None, None, None) outside the region.
            let exit_blk = self.new_block()?;
            self.jump(exit_blk)?;
            self.terminated = false;
            self.b.switch_to(exit_blk)?;
            let none_v = self.none()?;
            let r = self.call(exit_m, vec![none_v, none_v, none_v], Vec::new(), pos)?;
            self.await_of(r, pos)?;
            self.jump(end)?;
        }
        self.terminated = false;
        self.b.switch_to(landing)?;
        let exc = self.value_of(pos, |into| Instr::PushExcInfo { into })?;
        let ty = self.call_global("type", vec![exc], pos)?;
        let tb_sym = self.pool("__traceback__");
        let tb = self.value_of(pos, move |into| Instr::GetAttr {
            obj: exc,
            name: tb_sym,
            into,
        })?;
        let r = self.call(exit_m, vec![ty, exc, tb], Vec::new(), pos)?;
        let awaited = self.await_of(r, pos)?;
        let suppressed = self.unboxed_bool(pos, move |into| Instr::Truth {
            value: awaited,
            into,
        })?;
        let supp_blk = self.new_block()?;
        let reraise_blk = self.new_block()?;
        self.branch(suppressed, supp_blk, reraise_blk)?;
        self.terminated = false;
        self.b.switch_to(supp_blk)?;
        self.emit(Instr::PopExcInfo, pos)?;
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(reraise_blk)?;
        let at = reraise_blk;
        self.b.set_term(at, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    /// Fills `landing` so the exceptions in `types` jump to `fail` and
    /// anything else re-raises. Returns with the previous block current.
    fn miss_or_reraise(
        &mut self,
        landing: BlockId,
        types: Value,
        fail: BlockId,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let back = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        let back_terminated = self.terminated;
        self.terminated = false;
        self.b.switch_to(landing)?;
        let matched = self.b.fresh(Type::Bool);
        let exc = self.b.fresh(Type::Top);
        self.emit(
            Instr::CatchBegin {
                types,
                matched,
                exc,
            },
            pos,
        )?;
        let hit = self.new_block()?;
        let reraise = self.new_block()?;
        self.branch(matched, hit, reraise)?;
        self.terminated = false;
        self.b.switch_to(reraise)?;
        self.b.set_term(reraise, Terminator::Reraise)?;
        self.terminated = true;
        self.terminated = false;
        self.b.switch_to(hit)?;
        self.emit(Instr::CatchEnd, pos)?;
        self.jump(fail)?;
        self.terminated = back_terminated;
        self.b.switch_to(back)?;
        Ok(())
    }

    /// `getattr(obj, name)` with an AttributeError treated as a jump to
    /// `miss`. The result continues in a fresh block, so only the attribute
    /// read itself is covered by `miss`.
    fn getattr_or_miss(
        &mut self,
        obj: Value,
        name: Sym,
        miss: BlockId,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let look = self.new_block()?;
        self.jump(look)?;
        self.terminated = false;
        self.b.switch_to(look)?;
        self.b.set_handler(look, Some(miss))?;
        let v = self.value_of(pos, move |into| Instr::GetAttr { obj, name, into })?;
        let cont = self.new_block()?;
        self.jump(cont)?;
        self.terminated = false;
        self.b.switch_to(cont)?;
        Ok(v)
    }

    /// The same, for a runtime-computed attribute name: the `getattr`
    /// builtin, since GetAttr's name operand is a constant.
    fn getattr_dyn_or_miss(
        &mut self,
        obj: Value,
        name: Value,
        miss: BlockId,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let look = self.new_block()?;
        self.jump(look)?;
        self.terminated = false;
        self.b.switch_to(look)?;
        self.b.set_handler(look, Some(miss))?;
        let v = self.call_global("getattr", vec![obj, name], pos)?;
        let cont = self.new_block()?;
        self.jump(cont)?;
        self.terminated = false;
        self.b.switch_to(cont)?;
        Ok(v)
    }

    /// CPython's match_class_attr checks each attribute name against the
    /// ones already extracted before reading it; a repeat is a TypeError,
    /// not a failed match. Names may be runtime values (`__match_args__`).
    fn dup_attr_check(
        &mut self,
        cls_v: Value,
        name: Value,
        seen: &[Value],
        pos: Pos,
    ) -> Result<(), LowerError> {
        for prev in seen {
            let same = self.unboxed_bool(pos, move |into| Instr::Compare {
                op: CmpOp::Eq,
                lhs: name,
                rhs: *prev,
                into,
            })?;
            let dup = self.new_block()?;
            let more = self.new_block()?;
            self.branch(same, dup, more)?;
            self.terminated = false;
            self.b.switch_to(dup)?;
            let cls_name = self.cls_tp_name(cls_v, pos)?;
            let mid = self.const_str("() got multiple sub-patterns for attribute ", pos)?;
            let r = self.call_global("repr", vec![name], pos)?;
            let msg = self.build_str(vec![cls_name, mid, r], pos)?;
            self.raise_builtin("TypeError", msg, pos)?;
            self.terminated = false;
            self.b.switch_to(more)?;
        }
        Ok(())
    }

    fn attribute_error_types(&mut self, pos: Pos) -> Result<Value, LowerError> {
        let ae = self.load_plain_name("AttributeError", pos)?;
        self.build_tuple(vec![ae], pos)
    }

    fn key_error_types(&mut self, pos: Pos) -> Result<Value, LowerError> {
        let ke = self.load_plain_name("KeyError", pos)?;
        self.build_tuple(vec![ke], pos)
    }

    /// A `collections.abc` member. `Import("collections.abc", None, 0)`
    /// returns the top module, so the member needs two attribute reads.
    fn abc_class(&mut self, name: &str, pos: Pos) -> Result<Value, LowerError> {
        let mod_sym = self.pool("collections.abc");
        let none_v = self.none()?;
        let level = self.const_int(0, pos)?;
        let m = self.value_of(pos, move |into| Instr::Import {
            name: mod_sym,
            fromlist: none_v,
            level,
            into,
        })?;
        let abc_sym = self.pool("abc");
        let abc = self.value_of(pos, move |into| Instr::GetAttr {
            obj: m,
            name: abc_sym,
            into,
        })?;
        let name_sym = self.pool(name);
        self.value_of(pos, move |into| Instr::GetAttr {
            obj: abc,
            name: name_sym,
            into,
        })
    }

    /// A tuple of the named builtins, built at the use site.
    fn global_types_tuple(&mut self, names: &[&str], pos: Pos) -> Result<Value, LowerError> {
        let mut items = Vec::with_capacity(names.len());
        for n in names {
            items.push(self.load_plain_name(n, pos)?);
        }
        self.build_tuple(items, pos)
    }

    /// `len(subj) op want` with a constant `want`, as an unboxed Bool.
    fn len_at_least_or_eq(
        &mut self,
        subj: Value,
        op: CmpOp,
        want: i32,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let len = self.len_boxed(subj, pos)?;
        let rhs = self.const_int(want, pos)?;
        self.unboxed_bool(pos, move |into| Instr::Compare {
            op,
            lhs: len,
            rhs,
            into,
        })
    }

    fn len_boxed(&mut self, v: Value, pos: Pos) -> Result<Value, LowerError> {
        let n = self.b.fresh(Type::Int64);
        self.emit(Instr::Len { value: v, into: n }, pos)?;
        self.value_of(pos, move |into| Instr::Box {
            src: n,
            scalar: Scalar::Int64,
            into,
        })
    }

    fn attr_str(&mut self, obj: Value, attr: &str, pos: Pos) -> Result<Value, LowerError> {
        let name = self.pool(attr);
        self.value_of(pos, move |into| Instr::GetAttr { obj, name, into })
    }

    fn cls_tp_name(&mut self, cls_v: Value, pos: Pos) -> Result<Value, LowerError> {
        let t = self.call_global("type", vec![cls_v], pos)?;
        self.attr_str(t, "__name__", pos)
    }

    fn raise_type_error(&mut self, msg: Value, pos: Pos) -> Result<(), LowerError> {
        self.raise_builtin("TypeError", msg, pos)
    }

    /// `raise <name>(msg)` in the current block.
    fn raise_builtin(&mut self, name: &str, msg: Value, pos: Pos) -> Result<(), LowerError> {
        let exc = self.call_global(name, vec![msg], pos)?;
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.set_term(
            at,
            Terminator::Raise {
                exc: Some(exc),
                cause: None,
            },
        )?;
        self.terminated = true;
        Ok(())
    }

    /// CPython's class-pattern arity error:
    /// `{name}() accepts {allowed} positional sub-pattern(s) ({given} given)`.
    fn raise_arity_error(
        &mut self,
        cls_v: Value,
        allowed: Arity,
        given: usize,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let name = self.cls_tp_name(cls_v, pos)?;
        let head = self.const_str("() accepts ", pos)?;
        match allowed {
            Arity::Static(a) => {
                let plural = if a == 1 { "" } else { "s" };
                let mut parts = vec![name, head];
                parts.push(self.const_str(&a.to_string(), pos)?);
                parts.push(self.const_str(" positional sub-pattern", pos)?);
                parts.push(self.const_str(plural, pos)?);
                parts.push(self.const_str(" (", pos)?);
                parts.push(self.const_str(&given.to_string(), pos)?);
                parts.push(self.const_str(" given)", pos)?);
                let msg = self.build_str(parts, pos)?;
                self.raise_type_error(msg, pos)
            }
            Arity::Runtime(v) => {
                let allowed_s = self.call_global("str", vec![v], pos)?;
                let one = self.const_int(1, pos)?;
                let is_one = self.unboxed_bool(pos, move |into| Instr::Compare {
                    op: CmpOp::Eq,
                    lhs: v,
                    rhs: one,
                    into,
                })?;
                let sing = self.new_block()?;
                let plur = self.new_block()?;
                self.branch(is_one, sing, plur)?;
                for (blk, plural) in [(sing, ""), (plur, "s")] {
                    self.terminated = false;
                    self.b.switch_to(blk)?;
                    let tail = self.const_str(
                        &format!(" positional sub-pattern{plural} ({given} given)"),
                        pos,
                    )?;
                    let msg = self.build_str(vec![name, head, allowed_s, tail], pos)?;
                    self.raise_type_error(msg, pos)?;
                }
                self.terminated = true;
                Ok(())
            }
        }
    }

    fn lower_match(&mut self, subject: &Expr, cases: &[MatchCase]) -> Result<(), LowerError> {
        let subj = self.expr(subject)?;
        let end = self.new_block()?;
        let first = self.new_block()?;
        self.jump(first)?;
        self.terminated = false;
        self.b.switch_to(first)?;
        for case in cases {
            let MatchCase {
                pattern,
                guard,
                body,
            } = case;
            // The fail block is the next case's test block; on it the
            // captures this case would have bound hold their prior values
            // (CPython never binds them either).
            let fail = self.new_block()?;
            // Captures bind only when the whole pattern matches: CPython
            // defers the stores to just before the guard, so a later
            // sub-pattern that fails or raises leaves them unbound.
            let mut pending: Vec<(FSym, Value)> = Vec::new();
            self.lower_pattern(subj, pattern, fail, &mut pending)?;
            let ppos = pattern_pos(pattern);
            for (nm, v) in pending {
                self.store_name_value(nm, v, ppos)?;
            }
            match guard {
                Some(g) => {
                    // Bindings from the pattern persist into a failed guard,
                    // which falls through like a pattern mismatch.
                    let body_blk = self.new_block()?;
                    let cond = self.truth_of(g)?;
                    self.branch(cond, body_blk, fail)?;
                    self.terminated = false;
                    self.b.switch_to(body_blk)?;
                    self.lower_body(body)?;
                    self.jump(end)?;
                    self.terminated = false;
                }
                None => {
                    self.lower_body(body)?;
                    self.jump(end)?;
                    self.terminated = false;
                }
            }
            self.b.switch_to(fail)?;
            self.terminated = false;
        }
        self.jump(end)?;
        self.terminated = false;
        self.b.switch_to(end)?;
        Ok(())
    }

    /// Lower one pattern against `subj`: on success the current block
    /// continues, every failing path leaves for `fail`, and captures
    /// accumulate in `pending`, which the caller stores once the whole
    /// pattern matches (CPython defers the stores to just before the guard).
    fn lower_pattern(
        &mut self,
        subj: Value,
        p: &Pattern,
        fail: BlockId,
        pending: &mut Vec<(FSym, Value)>,
    ) -> Result<(), LowerError> {
        let pos = pattern_pos(p);
        match p {
            Pattern::MatchValue { value, .. } => {
                let v = self.expr(value)?;
                let c = self.unboxed_bool(pos, move |into| Instr::Compare {
                    op: CmpOp::Eq,
                    lhs: subj,
                    rhs: v,
                    into,
                })?;
                let ok = self.new_block()?;
                self.branch(c, ok, fail)?;
                self.terminated = false;
                self.b.switch_to(ok)?;
            }
            Pattern::MatchSingleton { value, .. } => {
                let v = self.const_value(value, pos)?;
                let c = self.unboxed_bool(pos, move |into| Instr::Compare {
                    op: CmpOp::Is,
                    lhs: subj,
                    rhs: v,
                    into,
                })?;
                let ok = self.new_block()?;
                self.branch(c, ok, fail)?;
                self.terminated = false;
                self.b.switch_to(ok)?;
            }
            Pattern::MatchSequence { patterns, .. } => {
                self.lower_sequence_pattern(subj, patterns, fail, pos, pending)?;
            }
            Pattern::MatchMapping {
                keys,
                patterns,
                rest,
                ..
            } => self.lower_mapping_pattern(subj, keys, patterns, *rest, fail, pos, pending)?,
            Pattern::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
                ..
            } => self.lower_class_pattern(
                subj,
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
                fail,
                pos,
                pending,
            )?,
            Pattern::MatchStar { name, .. } => {
                if let Some(nm) = name {
                    pending.push((*nm, subj));
                }
            }
            Pattern::MatchAs { pattern, name, .. } => {
                if let Some(inner) = pattern {
                    self.lower_pattern(subj, inner, fail, pending)?;
                }
                if let Some(nm) = name {
                    pending.push((*nm, subj));
                }
            }
            Pattern::MatchOr { patterns, .. } => {
                // Alternatives in order; the first success wins. Each
                // alternative copies its captures into join vars at its own
                // success point, and the first alternative's order governs
                // the stores after the join (CPython requires every
                // alternative to bind the same names).
                let ok = self.new_block()?;
                let last = patterns.len().saturating_sub(1);
                let mut names: Vec<FSym> = Vec::new();
                let mut vars: Vec<Sym> = Vec::new();
                for (i, alt) in patterns.iter().enumerate() {
                    let alt_fail = if i == last { fail } else { self.new_block()? };
                    let mut alt_caps: Vec<(FSym, Value)> = Vec::new();
                    self.lower_pattern(subj, alt, alt_fail, &mut alt_caps)?;
                    if i == 0 {
                        for (nm, v) in &alt_caps {
                            let var = self.fresh_join_var_named("matchcap");
                            self.write_var_at_current(var, *v)?;
                            vars.push(var);
                            names.push(*nm);
                        }
                    } else {
                        let mismatch = || LowerError::UnexpectedAst {
                            what: "match or-pattern alternatives bind different names",
                            line: pos.line,
                        };
                        if alt_caps.len() != names.len() {
                            return Err(mismatch());
                        }
                        for (nm, var) in names.iter().zip(&vars) {
                            let v = alt_caps
                                .iter()
                                .find(|(n, _)| n == nm)
                                .map(|(_, v)| *v)
                                .ok_or_else(mismatch)?;
                            self.write_var_at_current(*var, v)?;
                        }
                    }
                    if !self.terminated {
                        self.jump(ok)?;
                    }
                    if i != last {
                        self.terminated = false;
                        self.b.switch_to(alt_fail)?;
                    }
                }
                self.terminated = false;
                self.b.switch_to(ok)?;
                for (nm, var) in names.iter().zip(&vars) {
                    let v = self.read_var_at_current(*var)?;
                    pending.push((*nm, v));
                }
            }
        }
        Ok(())
    }

    fn lower_sequence_pattern(
        &mut self,
        subj: Value,
        pats: &[Pattern],
        fail: BlockId,
        pos: Pos,
        pending: &mut Vec<(FSym, Value)>,
    ) -> Result<(), LowerError> {
        // Py_TPFLAGS_SEQUENCE is set by registration with
        // collections.abc.Sequence; str, bytes and bytearray never register,
        // so the flag test is the isinstance pair below.
        let seq = self.abc_class("Sequence", pos)?;
        let is_seq = self.call_global("isinstance", vec![subj, seq], pos)?;
        let t1 = self.unboxed_bool(pos, move |into| Instr::Truth {
            value: is_seq,
            into,
        })?;
        let notstr_blk = self.new_block()?;
        self.branch(t1, notstr_blk, fail)?;
        self.terminated = false;
        self.b.switch_to(notstr_blk)?;
        let text_types = self.global_types_tuple(&["str", "bytes", "bytearray"], pos)?;
        let is_text = self.call_global("isinstance", vec![subj, text_types], pos)?;
        let t2 = self.unboxed_bool(pos, move |into| Instr::Truth {
            value: is_text,
            into,
        })?;
        let len_blk = self.new_block()?;
        self.branch(t2, fail, len_blk)?;
        self.terminated = false;
        self.b.switch_to(len_blk)?;
        let star = pats
            .iter()
            .position(|p| matches!(p, Pattern::MatchStar { .. }));
        let (op, want) = match star {
            Some(_) => (CmpOp::Ge, pats.len() - 1),
            None => (CmpOp::Eq, pats.len()),
        };
        let len_ok = self.len_at_least_or_eq(subj, op, want as i32, pos)?;
        let unpack_blk = self.new_block()?;
        self.branch(len_ok, unpack_blk, fail)?;
        self.terminated = false;
        self.b.switch_to(unpack_blk)?;
        let into: Vec<Value> = pats.iter().map(|_| self.fresh_top()).collect();
        self.emit(
            Instr::Unpack {
                src: subj,
                n: pats.len() as u32,
                star: star.map(|i| i as u32),
                into: into.clone().into(),
            },
            pos,
        )?;
        for (p, v) in pats.iter().zip(into) {
            self.lower_pattern(v, p, fail, pending)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_mapping_pattern(
        &mut self,
        subj: Value,
        keys: &[Expr],
        pats: &[Pattern],
        rest: Option<FSym>,
        fail: BlockId,
        pos: Pos,
        pending: &mut Vec<(FSym, Value)>,
    ) -> Result<(), LowerError> {
        // Py_TPFLAGS_MAPPING is set by registration with
        // collections.abc.Mapping.
        let map = self.abc_class("Mapping", pos)?;
        let is_map = self.call_global("isinstance", vec![subj, map], pos)?;
        let t = self.unboxed_bool(pos, move |into| Instr::Truth {
            value: is_map,
            into,
        })?;
        let after_blk = self.new_block()?;
        self.branch(t, after_blk, fail)?;
        self.terminated = false;
        self.b.switch_to(after_blk)?;
        if !keys.is_empty() {
            let len_ok = self.len_at_least_or_eq(subj, CmpOp::Ge, keys.len() as i32, pos)?;
            let look_blk = self.new_block()?;
            self.branch(len_ok, look_blk, fail)?;
            self.terminated = false;
            self.b.switch_to(look_blk)?;
        }
        // A missing key fails the case: CPython's MATCH_KEYS reads with a
        // sentinel default, so a KeyError from the lookup means missing. The
        // tuple and the key expressions come before the look blocks so they
        // dominate the landing.
        let miss = self.new_block()?;
        let ke_types = self.key_error_types(pos)?;
        let mut key_vals = Vec::with_capacity(keys.len());
        for k in keys {
            key_vals.push(self.expr(k)?);
        }
        let mut vals = Vec::with_capacity(keys.len());
        for (i, kv) in key_vals.iter().enumerate() {
            let kv = *kv;
            let look = self.new_block()?;
            self.jump(look)?;
            self.terminated = false;
            self.b.switch_to(look)?;
            // CPython's MATCH_KEYS checks each key against the earlier ones
            // just before its own lookup; a duplicate is a ValueError, not a
            // failed match, and an earlier missing key skips the later
            // checks.
            for prev in &key_vals[..i] {
                let same = self.unboxed_bool(pos, move |into| Instr::Compare {
                    op: CmpOp::Eq,
                    lhs: kv,
                    rhs: *prev,
                    into,
                })?;
                let dup = self.new_block()?;
                let more = self.new_block()?;
                self.branch(same, dup, more)?;
                self.terminated = false;
                self.b.switch_to(dup)?;
                let head = self.const_str("mapping pattern checks duplicate key (", pos)?;
                let r = self.call_global("repr", vec![kv], pos)?;
                let tail = self.const_str(")", pos)?;
                let msg = self.build_str(vec![head, r, tail], pos)?;
                self.raise_builtin("ValueError", msg, pos)?;
                self.terminated = false;
                self.b.switch_to(more)?;
            }
            let get = self.new_block()?;
            self.jump(get)?;
            self.terminated = false;
            self.b.switch_to(get)?;
            self.b.set_handler(get, Some(miss))?;
            vals.push(self.value_of(pos, move |into| Instr::GetItem {
                obj: subj,
                key: kv,
                into,
            })?);
            let next = self.new_block()?;
            self.jump(next)?;
            self.terminated = false;
            self.b.switch_to(next)?;
        }
        self.miss_or_reraise(miss, ke_types, fail, pos)?;
        for (p, v) in pats.iter().zip(vals) {
            self.lower_pattern(v, p, fail, pending)?;
        }
        if let Some(r) = rest {
            let copy = self.call_global("dict", vec![subj], pos)?;
            for kv in &key_vals {
                let k = *kv;
                self.emit(Instr::DelItem { obj: copy, key: k }, pos)?;
            }
            pending.push((r, copy));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn lower_class_pattern(
        &mut self,
        subj: Value,
        cls: &Expr,
        pats: &[Pattern],
        kwd_attrs: &[FSym],
        kwd_pats: &[Pattern],
        fail: BlockId,
        pos: Pos,
        pending: &mut Vec<(FSym, Value)>,
    ) -> Result<(), LowerError> {
        let cls_v = self.expr(cls)?;
        let inst = self.call_global("isinstance", vec![subj, cls_v], pos)?;
        let t = self.unboxed_bool(pos, move |into| Instr::Truth { value: inst, into })?;
        let inst_blk = self.new_block()?;
        self.branch(t, inst_blk, fail)?;
        self.terminated = false;
        self.b.switch_to(inst_blk)?;
        let nargs = pats.len();
        let mut pos_vals: Vec<Value> = Vec::new();
        // Attribute names already extracted, in CPython's order: positional
        // then keyword. For a single positional pattern the name arrives
        // through a join var, filled after the paths converge.
        let mut seen: Vec<Value> = Vec::new();

        let cont = if nargs > 0 {
            // __match_args__ on the class; a missing attribute selects the
            // match-self path for the builtins CPython flags (inherited by
            // subclasses), and is an arity TypeError for anything else.
            let noattr = self.new_block()?;
            let look = self.new_block()?;
            self.jump(look)?;
            self.terminated = false;
            self.b.switch_to(look)?;
            self.b.set_handler(look, Some(noattr))?;
            let ma_sym = self.pool("__match_args__");
            let ma = self.value_of(pos, move |into| Instr::GetAttr {
                obj: cls_v,
                name: ma_sym,
                into,
            })?;
            let found = self.new_block()?;
            self.jump(found)?;
            self.terminated = false;
            self.b.switch_to(found)?;

            // __match_args__ must be exactly a tuple.
            let ma_type = self.call_global("type", vec![ma], pos)?;
            let tuple_cls = self.load_plain_name("tuple", pos)?;
            let is_tuple = self.unboxed_bool(pos, move |into| Instr::Compare {
                op: CmpOp::Is,
                lhs: ma_type,
                rhs: tuple_cls,
                into,
            })?;
            let tuple_ok = self.new_block()?;
            let not_tuple = self.new_block()?;
            self.branch(is_tuple, tuple_ok, not_tuple)?;
            self.terminated = false;
            self.b.switch_to(not_tuple)?;
            let cls_name = self.cls_tp_name(cls_v, pos)?;
            let got_type = self.call_global("type", vec![ma], pos)?;
            let got = self.attr_str(got_type, "__name__", pos)?;
            let mid = self.const_str(".__match_args__ must be a tuple (got ", pos)?;
            let tail = self.const_str(")", pos)?;
            let msg = self.build_str(vec![cls_name, mid, got, tail], pos)?;
            self.raise_type_error(msg, pos)?;
            self.terminated = false;
            self.b.switch_to(tuple_ok)?;

            // It must hold every positional name the pattern asks for.
            let len_ok = self.len_at_least_or_eq(ma, CmpOp::Ge, nargs as i32, pos)?;
            let enough = self.new_block()?;
            let arity_err = self.new_block()?;
            self.branch(len_ok, enough, arity_err)?;
            self.terminated = false;
            self.b.switch_to(arity_err)?;
            let nma = self.len_boxed(ma, pos)?;
            self.raise_arity_error(cls_v, Arity::Runtime(nma), nargs, pos)?;
            self.terminated = false;
            self.b.switch_to(enough)?;

            let mut names = Vec::with_capacity(nargs);
            for i in 0..nargs {
                let iv = self.const_int(i as i32, pos)?;
                let key = self.build_tuple(vec![iv], pos)?;
                names.push(self.value_of(pos, move |into| Instr::GetItem {
                    obj: ma,
                    key,
                    into,
                })?);
            }

            // With one positional pattern the __match_args__ path and the
            // match-self path converge: the extracted attribute or the
            // subject itself.
            let pos_join = if nargs == 1 {
                Some((
                    self.fresh_join_var_named("matchpos"),
                    // The match-self path extracts no attribute, so it
                    // contributes no name to the duplicate set; None never
                    // equals a keyword name.
                    self.fresh_join_var_named("matchname"),
                    self.new_block()?,
                ))
            } else {
                None
            };

            // A missing subject attribute fails the case; the tuple comes
            // before the look blocks so it dominates the landing.
            let attrmiss = self.new_block()?;
            let ae_types2 = self.attribute_error_types(pos)?;
            for name in &names {
                self.dup_attr_check(cls_v, *name, &seen, pos)?;
                let v = self.getattr_dyn_or_miss(subj, *name, attrmiss, pos)?;
                match &pos_join {
                    Some((var, nvar, _)) => {
                        self.write_var_at_current(*var, v)?;
                        self.write_var_at_current(*nvar, *name)?;
                    }
                    None => {
                        pos_vals.push(v);
                        seen.push(*name);
                    }
                }
            }
            let last = self
                .b
                .current_block()
                .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
            // The extraction path joins the match-self path at `join`.
            if let Some((_, _, join)) = &pos_join {
                self.jump(*join)?;
            }

            // The noattr landing: only AttributeError from the
            // __match_args__ lookup itself takes the match-self path.
            let sentinel = self.new_block()?;
            self.terminated = false;
            self.b.switch_to(noattr)?;
            let ae_types = self.attribute_error_types(pos)?;
            let matched = self.b.fresh(Type::Bool);
            let exc = self.b.fresh(Type::Top);
            self.emit(
                Instr::CatchBegin {
                    types: ae_types,
                    matched,
                    exc,
                },
                pos,
            )?;
            let reraise = self.new_block()?;
            self.branch(matched, sentinel, reraise)?;
            self.terminated = false;
            self.b.switch_to(reraise)?;
            self.b.set_term(reraise, Terminator::Reraise)?;
            self.terminated = true;
            self.terminated = false;
            self.b.switch_to(sentinel)?;
            self.emit(Instr::CatchEnd, pos)?;
            let ms = self.global_types_tuple(
                &[
                    "int",
                    "str",
                    "bytes",
                    "bytearray",
                    "float",
                    "list",
                    "tuple",
                    "dict",
                    "set",
                    "frozenset",
                ],
                pos,
            )?;
            let is_sub = self.call_global("issubclass", vec![cls_v, ms], pos)?;
            let ts = self.unboxed_bool(pos, move |into| Instr::Truth {
                value: is_sub,
                into,
            })?;
            let ms_blk = self.new_block()?;
            let tyerr_blk = self.new_block()?;
            self.branch(ts, ms_blk, tyerr_blk)?;
            self.terminated = false;
            self.b.switch_to(tyerr_blk)?;
            self.raise_arity_error(cls_v, Arity::Static(0), nargs, pos)?;
            self.terminated = false;
            self.b.switch_to(ms_blk)?;
            match &pos_join {
                Some((var, nvar, join)) => {
                    self.write_var_at_current(*var, subj)?;
                    let none_name = self.none()?;
                    self.write_var_at_current(*nvar, none_name)?;
                    self.jump(*join)?;
                    self.terminated = false;
                }
                None => self.raise_arity_error(cls_v, Arity::Static(1), nargs, pos)?,
            }
            self.miss_or_reraise(attrmiss, ae_types2, fail, pos)?;
            match pos_join {
                Some((var, nvar, join)) => {
                    self.b.switch_to(join)?;
                    pos_vals.push(self.read_var_at_current(var)?);
                    let name = self.read_var_at_current(nvar)?;
                    seen.push(name);
                    join
                }
                None => last,
            }
        } else {
            inst_blk
        };
        self.terminated = false;
        self.b.switch_to(cont)?;

        // Keyword attributes: a missing attribute fails the case; the tuple
        // comes before the look blocks so it dominates the landing.
        let mut kw_vals = Vec::with_capacity(kwd_attrs.len());
        if !kwd_attrs.is_empty() {
            let kwmiss = self.new_block()?;
            let ae_types3 = self.attribute_error_types(pos)?;
            for attr in kwd_attrs {
                let name_str = self.text(*attr);
                let name_v = self.const_str(&name_str, pos)?;
                self.dup_attr_check(cls_v, name_v, &seen, pos)?;
                let name = self.pool(&name_str);
                kw_vals.push(self.getattr_or_miss(subj, name, kwmiss, pos)?);
                seen.push(name_v);
            }
            self.miss_or_reraise(kwmiss, ae_types3, fail, pos)?;
        }

        // Positional sub-patterns, then keyword sub-patterns.
        for (p, v) in pats.iter().zip(pos_vals) {
            self.lower_pattern(v, p, fail, pending)?;
        }
        for (p, v) in kwd_pats.iter().zip(kw_vals) {
            self.lower_pattern(v, p, fail, pending)?;
        }
        Ok(())
    }

    fn lower_import(&mut self, names: &[Alias], pos: Pos) -> Result<(), LowerError> {
        for alias in names {
            let Alias { name, asname, .. } = alias;
            let module_str = self.text(*name);
            let level_v = self.const_int(0, pos)?;
            let none_v = self.none()?;
            let name_sym = self.pool(&module_str);
            let mod_v = self.value_of(pos, move |into| Instr::Import {
                name: name_sym,
                fromlist: none_v,
                level: level_v,
                into,
            })?;
            match asname {
                Some(asname) => {
                    // `import a.b.c as d`: the import yields the top module
                    // `a`; walk the attrs after it (IMPORT_FROM b, c), never
                    // reading `a` back from a namespace.
                    let mut parts = module_str.split('.');
                    parts.next();
                    let mut cur = mod_v;
                    for part in parts {
                        let attr_sym = self.pool(part);
                        cur = self.value_of(pos, move |into| Instr::GetAttr {
                            obj: cur,
                            name: attr_sym,
                            into,
                        })?;
                    }
                    self.store_name_value(*asname, cur, pos)?;
                }
                None => {
                    let first = module_str.split('.').next().unwrap_or("");
                    self.store_plain_name(first, mod_v, pos)?;
                }
            }
        }
        Ok(())
    }

    fn lower_import_from(
        &mut self,
        module: Option<&FSym>,
        names: &[Alias],
        level: i32,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let module_str = module.map(|m| self.text(*m)).unwrap_or_default();
        let name_syms: Vec<String> = names.iter().map(|a| self.text(a.name)).collect();
        let mut const_idxs = Vec::new();
        for n in &name_syms {
            let sym = self.pool(n);
            const_idxs.push(self.low.consts.const_idx(Const::Str(sym)));
        }
        let tuple_idx = self.low.consts.const_idx(Const::Tuple(const_idxs.into()));
        let fromlist = self.value_of(pos, move |into| Instr::Const {
            idx: tuple_idx,
            into,
        })?;
        let level_v = self.const_int(level, pos)?;
        let name_sym = self.pool(&module_str);
        let mod_v = self.value_of(pos, move |into| Instr::Import {
            name: name_sym,
            fromlist,
            level: level_v,
            into,
        })?;
        for alias in names {
            let Alias { name, asname, .. } = alias;
            let raw = self.text(*name);
            if raw == "*" {
                self.emit(Instr::ImportStar { module: mod_v }, pos)?;
                return Ok(());
            }
            let mangled = self.mangle(&raw);
            let attr_sym = self.pool(&mangled);
            let v = self.value_of(pos, move |into| Instr::GetAttr {
                obj: mod_v,
                name: attr_sym,
                into,
            })?;
            match asname {
                Some(asname) => self.store_name_value(*asname, v, pos)?,
                None => {
                    let m = self.mangle(&raw);
                    self.store_scoped_name(&m, v, pos)?;
                }
            }
        }
        Ok(())
    }

    fn const_int(&mut self, v: i32, pos: Pos) -> Result<Value, LowerError> {
        let idx = self.low.consts.const_idx(Const::Int(i64::from(v)));
        self.value_of(pos, move |into| Instr::Const { idx, into })
    }
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

fn bin_op(op: &Operator) -> BinOp {
    match op {
        Operator::Add => BinOp::Add,
        Operator::Sub => BinOp::Sub,
        Operator::Mult => BinOp::Mul,
        Operator::Div => BinOp::Div,
        Operator::FloorDiv => BinOp::FloorDiv,
        Operator::Mod => BinOp::Mod,
        Operator::Pow => BinOp::Pow,
        Operator::LShift => BinOp::LShift,
        Operator::RShift => BinOp::RShift,
        Operator::BitAnd => BinOp::BitAnd,
        Operator::BitOr => BinOp::BitOr,
        Operator::BitXor => BinOp::BitXor,
        Operator::MatMult => BinOp::MatMul,
    }
}

fn cmp_op(op: &Cmpop) -> CmpOp {
    match op {
        Cmpop::Eq => CmpOp::Eq,
        Cmpop::NotEq => CmpOp::Ne,
        Cmpop::Lt => CmpOp::Lt,
        Cmpop::LtE => CmpOp::Le,
        Cmpop::Gt => CmpOp::Gt,
        Cmpop::GtE => CmpOp::Ge,
        Cmpop::Is => CmpOp::Is,
        Cmpop::IsNot => CmpOp::IsNot,
        Cmpop::In => CmpOp::In,
        Cmpop::NotIn => CmpOp::NotIn,
    }
}

fn unary_op(op: &Unaryop) -> UnaryOp {
    match op {
        Unaryop::Invert => UnaryOp::Invert,
        Unaryop::Not => UnaryOp::Not,
        Unaryop::UAdd => UnaryOp::Pos,
        Unaryop::USub => UnaryOp::Neg,
    }
}

impl<'a, 'b> FnLower<'a, 'b> {
    /// The truth value of an expression, as the unboxed Bool a Branch needs.
    fn truth_of(&mut self, e: &Expr) -> Result<Value, LowerError> {
        let v = self.expr(e)?;
        let pos = expr_pos(e);
        self.unboxed_bool(pos, move |into| Instr::Truth { value: v, into })
    }

    fn const_str(&mut self, s: &str, pos: Pos) -> Result<Value, LowerError> {
        let sym = self.pool(s);
        let idx = self.low.consts.const_idx(Const::Str(sym));
        self.value_of(pos, move |into| Instr::Const { idx, into })
    }

    fn const_bool_unboxed(&mut self, b: bool, pos: Pos) -> Result<Value, LowerError> {
        let idx = self.low.consts.const_idx(Const::Bool(b));
        let into = self.b.fresh(Type::Bool);
        self.emit(Instr::Const { idx, into }, pos)?;
        Ok(into)
    }

    fn box_bool(&mut self, v: Value, pos: Pos) -> Result<Value, LowerError> {
        self.value_of(pos, move |into| Instr::Box {
            src: v,
            scalar: Scalar::Bool,
            into,
        })
    }

    fn call(
        &mut self,
        callee: Value,
        args: Vec<Value>,
        kwnames: Vec<(Sym, Value)>,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        self.value_of(pos, move |into| Instr::Call {
            callee,
            args: args.into(),
            kwnames: kwnames.into(),
            into,
        })
    }

    fn expr(&mut self, e: &Expr) -> Result<Value, LowerError> {
        let pos = expr_pos(e);
        match e {
            Expr::Constant { value: c, .. } => self.const_value(c, pos),
            Expr::Name { id, .. } => self.load_name(*id, pos),
            Expr::BoolOp { op, values, .. } => self.lower_bool_op(op, values, pos),
            Expr::NamedExpr { target, value, .. } => {
                let v = self.expr(value)?;
                if let Expr::Name { id, .. } = &**target {
                    self.store_name_value(*id, v, pos)?;
                } else {
                    return Err(LowerError::UnexpectedAst {
                        what: "named expression target is not a name",
                        line: pos.line,
                    });
                }
                Ok(v)
            }
            Expr::BinOp {
                left, op, right, ..
            } => {
                let l = self.expr(left)?;
                let r = self.expr(right)?;
                let bop = bin_op(op);
                self.value_of(pos, move |into| Instr::BinOp {
                    op: bop,
                    lhs: l,
                    rhs: r,
                    into,
                })
            }
            Expr::UnaryOp { op, operand, .. } => {
                let v = self.expr(operand)?;
                let uop = unary_op(op);
                self.value_of(pos, move |into| Instr::UnaryOp {
                    op: uop,
                    operand: v,
                    into,
                })
            }
            Expr::Lambda { args, body, .. } => self.lower_lambda(args, body, pos),
            Expr::IfExp {
                test, body, orelse, ..
            } => {
                let cond = self.truth_of(test)?;
                let then_blk = self.new_block()?;
                let else_blk = self.new_block()?;
                let join = self.new_block()?;
                let var = self.fresh_join_var();
                self.branch(cond, then_blk, else_blk)?;
                self.terminated = false;
                self.b.switch_to(then_blk)?;
                let tv = self.expr(body)?;
                self.write_var_at_current(var, tv)?;
                self.jump(join)?;
                self.terminated = false;
                self.b.switch_to(else_blk)?;
                let ev = self.expr(orelse)?;
                self.write_var_at_current(var, ev)?;
                self.jump(join)?;
                self.terminated = false;
                self.b.switch_to(join)?;
                self.read_var_at_current(var)
            }
            Expr::Dict { keys, values, .. } => self.lower_dict(keys, values, pos),
            Expr::Set { elts, .. } => {
                if elts.iter().any(|e| matches!(e, Expr::Starred { .. })) {
                    // CPython's SET_UPDATE / SET_ADD split: an empty set,
                    // `update` per starred element, `add` per plain one.
                    let s = self.build_set(Vec::new(), pos)?;
                    for e in elts {
                        match e {
                            Expr::Starred { value, .. } => {
                                let v = self.expr(value)?;
                                let m = self.method(&s, "update", pos)?;
                                self.call(m, vec![v], Vec::new(), pos)?;
                            }
                            other => {
                                let v = self.expr(other)?;
                                let m = self.method(&s, "add", pos)?;
                                self.call(m, vec![v], Vec::new(), pos)?;
                            }
                        }
                    }
                    return Ok(s);
                }
                let mut items = Vec::with_capacity(elts.len());
                for e in elts {
                    items.push(self.expr(e)?);
                }
                self.build_set(items, pos)
            }
            Expr::ListComp {
                elt, generators, ..
            } => self.lower_inlined_comp(CompKind::List, generators, elt, None, pos),
            Expr::SetComp {
                elt, generators, ..
            } => self.lower_inlined_comp(CompKind::Set, generators, elt, None, pos),
            Expr::DictComp {
                key,
                value,
                generators,
                ..
            } => self.lower_inlined_comp(CompKind::Dict, generators, key, Some(value), pos),
            Expr::GeneratorExp {
                elt, generators, ..
            } => self.lower_genexp(generators, elt, pos),
            Expr::Await { value, .. } => self.lower_await(value, pos),
            Expr::Yield { value, .. } => {
                if !self.kind.function_like() {
                    return Err(LowerError::Rejected {
                        message: "'yield' outside function".to_owned(),
                        line: pos.line,
                    });
                }
                let v = match value {
                    Some(v) => self.expr(v)?,
                    None => self.none()?,
                };
                let state = self.next_state();
                self.value_of(pos, move |into| Instr::Yield {
                    state,
                    value: v,
                    into,
                })
            }
            Expr::YieldFrom { value, .. } => {
                if !self.kind.function_like() {
                    return Err(LowerError::Rejected {
                        message: "'yield from' outside function".to_owned(),
                        line: pos.line,
                    });
                }
                if self.async_def {
                    return Err(LowerError::Rejected {
                        message: "'yield from' inside async function".to_owned(),
                        line: pos.line,
                    });
                }
                let v = self.expr(value)?;
                let it = self.value_of(pos, move |into| Instr::GetIter { obj: v, into })?;
                let send = self.none()?;
                let state = self.next_state();
                self.value_of(pos, move |into| Instr::YieldFrom {
                    state,
                    iter: it,
                    send,
                    into,
                })
            }
            Expr::Compare {
                left,
                ops,
                comparators,
                ..
            } => self.lower_compare(left, ops, comparators, pos),
            Expr::Call {
                func,
                args,
                keywords,
                ..
            } => self.lower_call(func, args, keywords, pos),
            Expr::FormattedValue {
                value,
                conversion,
                format_spec,
                ..
            } => {
                let v = self.expr(value)?;
                let v = match *conversion {
                    -1 => v,
                    115 => self.call_global("str", vec![v], pos)?,
                    114 => self.call_global("repr", vec![v], pos)?,
                    97 => self.call_global("ascii", vec![v], pos)?,
                    _ => {
                        return Err(LowerError::UnexpectedAst {
                            what: "unknown f-string conversion",
                            line: pos.line,
                        });
                    }
                };
                let spec = match format_spec {
                    Some(s) => self.expr(s)?,
                    None => self.const_str("", pos)?,
                };
                self.value_of(pos, move |into| Instr::Format {
                    value: v,
                    spec,
                    into,
                })
            }
            Expr::JoinedStr { values, .. } => {
                let mut parts = Vec::with_capacity(values.len());
                for v in values {
                    parts.push(self.expr(v)?);
                }
                self.build_str(parts, pos)
            }
            Expr::Attribute { value, attr, .. } => {
                let obj = self.expr(value)?;
                let name = self.pool(&self.mangle(&self.text(*attr)));
                self.value_of(pos, move |into| Instr::GetAttr { obj, name, into })
            }
            Expr::Subscript { value, slice, .. } => {
                let obj = self.expr(value)?;
                let key = self.subscript_key(slice)?;
                self.value_of(pos, move |into| Instr::GetItem { obj, key, into })
            }
            Expr::Starred { .. } => Err(LowerError::UnexpectedAst {
                what: "starred expression outside a call or assignment",
                line: pos.line,
            }),
            Expr::List { elts, .. } => self.lower_seq(true, elts, pos),
            Expr::Tuple { elts, .. } => self.lower_seq(false, elts, pos),
            Expr::Slice { .. } => Err(LowerError::UnexpectedAst {
                what: "slice expression outside a subscript",
                line: pos.line,
            }),
        }
    }

    fn call_global(&mut self, name: &str, args: Vec<Value>, pos: Pos) -> Result<Value, LowerError> {
        let sym = self.pool(name);
        let callee = self.value_of(pos, move |into| Instr::LoadGlobal { name: sym, into })?;
        self.call(callee, args, Vec::new(), pos)
    }

    fn const_value(&mut self, c: &Constant, pos: Pos) -> Result<Value, LowerError> {
        match c {
            Constant::None => self.const_val(Const::None, pos),
            Constant::Bool(b) => {
                let idx = self.low.consts.const_idx(Const::Bool(*b));
                self.value_of(pos, move |into| Instr::Const { idx, into })
            }
            Constant::Int(s) => match s.parse::<i64>() {
                Ok(v) => {
                    let idx = self.low.consts.const_idx(Const::Int(v));
                    self.value_of(pos, move |into| Instr::Const { idx, into })
                }
                Err(_) => {
                    let idx = self.low.consts.const_idx(Const::BigInt(s.clone()));
                    self.value_of(pos, move |into| Instr::Const { idx, into })
                }
            },
            Constant::Float(f) => {
                let idx = self.low.consts.const_idx(Const::Float(*f));
                self.value_of(pos, move |into| Instr::Const { idx, into })
            }
            Constant::Str(cps) => {
                let s: String = cps
                    .iter()
                    .map(|&cp| char::from_u32(cp).unwrap_or('\u{FFFD}'))
                    .collect();
                let sym = self.pool(&s);
                let idx = self.low.consts.const_idx(Const::Str(sym));
                self.value_of(pos, move |into| Instr::Const { idx, into })
            }
            Constant::Bytes(bs) => {
                let idx = self.low.consts.const_idx(Const::Bytes(bs.clone().into()));
                self.value_of(pos, move |into| Instr::Const { idx, into })
            }
            Constant::Ellipsis => {
                let idx = self.low.consts.const_idx(Const::Ellipsis);
                self.value_of(pos, move |into| Instr::Const { idx, into })
            }
            Constant::Complex { real, imag } => {
                // The const pool has no complex; build it at runtime.
                let re = {
                    let idx = self.low.consts.const_idx(Const::Float(*real));
                    self.value_of(pos, move |into| Instr::Const { idx, into })?
                };
                let im = {
                    let idx = self.low.consts.const_idx(Const::Float(*imag));
                    self.value_of(pos, move |into| Instr::Const { idx, into })?
                };
                self.call_global("complex", vec![re, im], pos)
            }
        }
    }

    /// A list or tuple literal, honouring `*` elements (CPython builds a
    /// list, extends it per star, then freezes to a tuple).
    fn lower_seq(&mut self, is_list: bool, elts: &[Expr], pos: Pos) -> Result<Value, LowerError> {
        let has_star = elts.iter().any(|e| matches!(e, Expr::Starred { .. }));
        if !has_star {
            let mut items = Vec::with_capacity(elts.len());
            for e in elts {
                items.push(self.expr(e)?);
            }
            return if is_list {
                self.build_list(items, pos)
            } else {
                self.build_tuple(items, pos)
            };
        }
        let lst = self.build_list(Vec::new(), pos)?;
        for e in elts {
            match e {
                Expr::Starred { value, .. } => {
                    let v = self.expr(value)?;
                    let m = self.method(&lst, "extend", pos)?;
                    self.call(m, vec![v], Vec::new(), pos)?;
                }
                other => {
                    let v = self.expr(other)?;
                    let m = self.method(&lst, "append", pos)?;
                    self.call(m, vec![v], Vec::new(), pos)?;
                }
            }
        }
        if is_list {
            Ok(lst)
        } else {
            self.call_global("tuple", vec![lst], pos)
        }
    }

    fn method(&mut self, obj: &Value, name: &str, pos: Pos) -> Result<Value, LowerError> {
        let sym = self.pool(name);
        let o = *obj;
        self.value_of(pos, move |into| Instr::GetAttr {
            obj: o,
            name: sym,
            into,
        })
    }

    /// `{k: v, **unpack}`: a literal dict plus runtime updates for `**`.
    fn lower_dict(
        &mut self,
        keys: &[Option<Expr>],
        values: &[Expr],
        pos: Pos,
    ) -> Result<Value, LowerError> {
        if keys.iter().all(|k| k.is_some()) {
            let mut items = Vec::with_capacity(keys.len());
            for (k, v) in keys.iter().zip(values) {
                let kv = self.expr(k.as_ref().expect("checked: no dict unpack"))?;
                let vv = self.expr(v)?;
                items.push((kv, vv));
            }
            return self.build_dict(items, pos);
        }
        let d = self.build_dict(Vec::new(), pos)?;
        for (k, v) in keys.iter().zip(values) {
            match k {
                Some(k) => {
                    let kv = self.expr(k)?;
                    let vv = self.expr(v)?;
                    self.emit(
                        Instr::SetItem {
                            obj: d,
                            key: kv,
                            value: vv,
                        },
                        pos,
                    )?;
                }
                None => {
                    let vv = self.expr(v)?;
                    let m = self.method(&d, "update", pos)?;
                    self.call(m, vec![vv], Vec::new(), pos)?;
                }
            }
        }
        Ok(d)
    }

    /// `and` / `or` short-circuit chains: the running value is kept and the
    /// next operand is evaluated only when it can become the result.
    fn lower_bool_op(
        &mut self,
        op: &Boolop,
        values: &[Expr],
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let mut v = self.expr(&values[0])?;
        for e in &values[1..] {
            let t = self.unboxed_bool(pos, move |into| Instr::Truth { value: v, into })?;
            let rhs_blk = self.new_block()?;
            let short_blk = self.new_block()?;
            let join = self.new_block()?;
            let var = self.fresh_join_var();
            match op {
                Boolop::And => self.branch(t, rhs_blk, short_blk)?,
                Boolop::Or => self.branch(t, short_blk, rhs_blk)?,
            }
            self.terminated = false;
            self.b.switch_to(rhs_blk)?;
            let nv = self.expr(e)?;
            self.write_var_at_current(var, nv)?;
            self.jump(join)?;
            self.terminated = false;
            self.b.switch_to(short_blk)?;
            self.write_var_at_current(var, v)?;
            self.jump(join)?;
            self.terminated = false;
            self.b.switch_to(join)?;
            v = self.read_var_at_current(var)?;
        }
        Ok(v)
    }

    /// `a < b < c` and friends: each link short-circuits to false. Every
    /// operand is evaluated exactly once (CPython copies the middle on the
    /// stack rather than re-evaluating it).
    fn lower_compare(
        &mut self,
        left: &Expr,
        ops: &[Cmpop],
        comparators: &[Expr],
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let l = self.expr(left)?;
        self.lower_compare_rest(l, ops, comparators, pos)
    }

    /// The chain after `lhs`: compare against `comparators[0]`, then either
    /// short-circuit or continue with that same value as the next left
    /// operand.
    fn lower_compare_rest(
        &mut self,
        lhs: Value,
        ops: &[Cmpop],
        comparators: &[Expr],
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let r0 = self.expr(&comparators[0])?;
        let cop = cmp_op(&ops[0]);
        let c0 = self.unboxed_bool(pos, move |into| Instr::Compare {
            op: cop,
            lhs,
            rhs: r0,
            into,
        })?;
        if ops.len() == 1 {
            return self.box_bool(c0, pos);
        }
        let rhs_blk = self.new_block()?;
        let false_blk = self.new_block()?;
        let join = self.new_block()?;
        let var = self.fresh_join_var();
        self.branch(c0, rhs_blk, false_blk)?;
        self.terminated = false;
        self.b.switch_to(false_blk)?;
        {
            let f = self.const_bool_unboxed(false, pos)?;
            let fb = self.box_bool(f, pos)?;
            self.write_var_at_current(var, fb)?;
        }
        self.jump(join)?;
        self.terminated = false;
        self.b.switch_to(rhs_blk)?;
        let rest = self.lower_compare_rest(r0, &ops[1..], &comparators[1..], pos)?;
        self.write_var_at_current(var, rest)?;
        self.jump(join)?;
        self.terminated = false;
        self.b.switch_to(join)?;
        self.read_var_at_current(var)
    }

    fn lower_call(
        &mut self,
        func: &Expr,
        args: &[Expr],
        keywords: &[Keyword],
        pos: Pos,
    ) -> Result<Value, LowerError> {
        if let Some(v) = self.try_super_call(func, args, keywords, pos)? {
            return Ok(v);
        }
        let callee = self.expr(func)?;
        let has_star = args.iter().any(|a| matches!(a, Expr::Starred { .. }))
            || keywords.iter().any(|k| k.arg.is_none());
        if !has_star {
            let mut arg_vals = Vec::with_capacity(args.len());
            for a in args {
                arg_vals.push(self.expr(a)?);
            }
            let mut kwnames = Vec::with_capacity(keywords.len());
            for k in keywords {
                let name = self.text(k.arg.expect("checked: no kw unpack"));
                let sym = self.pool(&name);
                let v = self.expr(&k.value)?;
                kwnames.push((sym, v));
            }
            return self.call(callee, arg_vals, kwnames, pos);
        }
        // CallEx: build the positional tuple and keyword dict at runtime.
        let lst = self.build_list(Vec::new(), pos)?;
        for a in args {
            match a {
                Expr::Starred { value, .. } => {
                    let v = self.expr(value)?;
                    let m = self.method(&lst, "extend", pos)?;
                    self.call(m, vec![v], Vec::new(), pos)?;
                }
                other => {
                    let v = self.expr(other)?;
                    let m = self.method(&lst, "append", pos)?;
                    self.call(m, vec![v], Vec::new(), pos)?;
                }
            }
        }
        let args_tuple = self.call_global("tuple", vec![lst], pos)?;
        let kwargs = if keywords.is_empty() {
            self.none()?
        } else {
            let d = self.build_dict(Vec::new(), pos)?;
            for k in keywords {
                match k.arg {
                    Some(name) => {
                        let name_str = self.text(name);
                        let kv = self.const_str(&name_str, pos)?;
                        let vv = self.expr(&k.value)?;
                        self.emit(
                            Instr::SetItem {
                                obj: d,
                                key: kv,
                                value: vv,
                            },
                            pos,
                        )?;
                    }
                    None => {
                        let vv = self.expr(&k.value)?;
                        let m = self.method(&d, "update", pos)?;
                        self.call(m, vec![vv], Vec::new(), pos)?;
                    }
                }
            }
            d
        };
        self.value_of(pos, move |into| Instr::CallEx {
            callee,
            args: args_tuple,
            kwargs,
            into,
        })
    }

    /// The zero-argument `super()` rewrite, under CPython's exact conditions.
    fn try_super_call(
        &mut self,
        func: &Expr,
        args: &[Expr],
        keywords: &[Keyword],
        pos: Pos,
    ) -> Result<Option<Value>, LowerError> {
        let Expr::Name { id, .. } = func else {
            return Ok(None);
        };
        if self.text(*id) != "super" || !args.is_empty() || !keywords.is_empty() {
            return Ok(None);
        }
        if self.kind != FnKind::Function {
            return Ok(None);
        }
        match self.flags_for("super") {
            Some(f) if scope_of(f) != symtable::GLOBAL_IMPLICIT => return Ok(None),
            None => {}
            _ => {}
        }
        if let Some(f) = self.low.root_flags("super") {
            let _ = f;
            return Ok(None);
        }
        match self.flags_for("__class__") {
            Some(f) if scope_of(f) == symtable::FREE => {}
            _ => return Ok(None),
        }
        if self.n_argcount == 0 {
            return Ok(None);
        }
        let class_cell = match self.cell_of("__class__", pos) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let sym = self.pool("super");
        let callee = self.value_of(pos, move |into| Instr::LoadGlobal { name: sym, into })?;
        let first_param = Value(0);
        let v = self.call(callee, vec![class_cell, first_param], Vec::new(), pos)?;
        Ok(Some(v))
    }

    fn fresh_join_var(&mut self) -> Sym {
        self.join_count += 1;
        self.pool(&format!(".join{}", self.join_count))
    }

    fn fresh_join_var_named(&mut self, kind: &str) -> Sym {
        self.join_count += 1;
        self.pool(&format!(".{kind}{}", self.join_count))
    }

    /// Write `var` in the current block, leaving it first when the block has
    /// already emitted a may-raise instruction. The verifier's spill model
    /// lets a handler read only values that certainly exist at the raise
    /// point: a definition at or after its block's first may-raise never
    /// crosses the exception edge. Starting a fresh block keeps every local
    /// definition before its block's first may-raise, so the raising block's
    /// edge carries the previous value — exactly CPython's raise-point
    /// semantics — and handlers read the merged raise-point values instead
    /// of a region-entry snapshot.
    fn write_var_at_current(&mut self, var: Sym, value: Value) -> Result<(), LowerError> {
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        if !self.terminated && self.raised_in == Some(at) {
            let handler = self.b.handler_of(at);
            let next = self.b.new_block();
            if let Some(h) = handler {
                self.b.set_handler(next, Some(h))?;
            }
            self.b.set_term(at, Terminator::Jump { to: next })?;
            self.terminated = false;
            self.b.switch_to(next)?;
        }
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.write_variable(var, at, value)?;
        Ok(())
    }

    fn read_var_at_current(&mut self, var: Sym) -> Result<Value, LowerError> {
        let at = self
            .b
            .current_block()
            .ok_or(LowerError::Build(BuildError::NoCurrentBlock))?;
        self.b.read_variable(var, at).map_err(LowerError::Build)
    }

    fn lower_lambda(
        &mut self,
        args: &Arguments,
        body: &Expr,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        // The symtable and the code generator both visit the defaults
        // before the lambda's own scope.
        let (defaults, kwdefaults) = self.lower_default_values(args, pos)?;
        let scope = self.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing lambda scope",
            line: pos.line,
        })?;
        let qualname = self.child_qualname("<lambda>", FnKind::Function);
        let fid = self.low.reserve_function();
        let cells = self.child_cells(scope, pos)?;
        let params = param_names(self, args);
        let private = self.private.clone();
        let mangled = self.mangled.clone();
        let mut child = self.low.new_fn(
            scope,
            FnKind::Function,
            "<lambda>",
            &qualname,
            params,
            private,
            mangled,
            (Flavor::Function, scope),
        );
        let landing = if scope.generator {
            Some(child.push_stopiteration_wrapper()?)
        } else {
            None
        };
        let v = child.expr(body)?;
        child.ret(v)?;
        if let Some(landing) = landing {
            child.fill_stopiteration_landing(landing, pos)?;
        }
        child.finish_into(fid)?;
        self.make_function_value(fid, cells, defaults, kwdefaults, pos)
    }

    /// The two default values of CPython's MAKE_FUNCTION convention: the
    /// positional-defaults tuple (or None) and the keyword-defaults dict
    /// (or None), evaluated here in the defining scope.
    fn lower_default_values(
        &mut self,
        args: &Arguments,
        pos: Pos,
    ) -> Result<(Value, Value), LowerError> {
        let d = if args.defaults.is_empty() {
            self.none()?
        } else {
            let mut items = Vec::with_capacity(args.defaults.len());
            for e in &args.defaults {
                items.push(self.expr(e)?);
            }
            self.build_tuple(items, pos)?
        };
        let k = if args.kw_defaults.iter().any(Option::is_some) {
            let mut items = Vec::new();
            for (arg, dflt) in args.kwonlyargs.iter().zip(&args.kw_defaults) {
                if let Some(e) = dflt {
                    let key_str = self.mangle(&self.text(arg.arg));
                    let kv = self.const_str(&key_str, pos)?;
                    let vv = self.expr(e)?;
                    items.push((kv, vv));
                }
            }
            self.build_dict(items, pos)?
        } else {
            self.none()?
        };
        Ok((d, k))
    }

    fn make_function_value(
        &mut self,
        fid: FnId,
        cells: Vec<(String, Value)>,
        defaults: Value,
        kwdefaults: Value,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let cell_vals: Vec<Value> = cells.into_iter().map(|(_, v)| v).collect();
        self.value_of(pos, move |into| Instr::MakeFunction {
            func: fid,
            cells: cell_vals.into(),
            defaults: Box::from([defaults, kwdefaults]),
            into,
        })
    }
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

/// A function's parameters classified by kind; `ordered()` gives CPython's
/// value order: positional (pos-only then normal), `*vararg`, keyword-only,
/// `**kwarg`.
struct ParamSpec {
    positional: Vec<String>,
    vararg: Option<String>,
    kwonly: Vec<String>,
    kwarg: Option<String>,
}

impl ParamSpec {
    fn none() -> Self {
        Self {
            positional: Vec::new(),
            vararg: None,
            kwonly: Vec::new(),
            kwarg: None,
        }
    }

    fn ordered(&self) -> Vec<String> {
        let mut out = self.positional.clone();
        if let Some(v) = &self.vararg {
            out.push(v.clone());
        }
        out.extend(self.kwonly.iter().cloned());
        if let Some(v) = &self.kwarg {
            out.push(v.clone());
        }
        out
    }
}

fn param_names(f: &mut FnLower, args: &Arguments) -> ParamSpec {
    let mut positional = Vec::new();
    for a in args.posonlyargs.iter().chain(&args.args) {
        positional.push(f.mangle(&f.text(a.arg)));
    }
    ParamSpec {
        positional,
        vararg: args.vararg.as_ref().map(|v| f.mangle(&f.text(v.arg))),
        kwonly: args
            .kwonlyargs
            .iter()
            .map(|a| f.mangle(&f.text(a.arg)))
            .collect(),
        kwarg: args.kwarg.as_ref().map(|v| f.mangle(&f.text(v.arg))),
    }
}

impl<'a, 'b> FnLower<'a, 'b> {
    fn flags_in(&self, scope: &SymbolTable, name: &str) -> Option<i64> {
        for &(s, flags) in &scope.symbols {
            if self.text(s) == name {
                return Some(flags);
            }
        }
        None
    }

    /// The qualname of a scope nested directly in this one, per CPython's
    /// `compiler_set_qualname`.
    fn child_qualname(&self, name: &str, kind: FnKind) -> String {
        if matches!(kind, FnKind::Function | FnKind::Class) {
            let mangled = self.mangle(name);
            if let Some(f) = self.flags_in(self.eff_scope, &mangled)
                && scope_of(f) == symtable::GLOBAL_EXPLICIT
            {
                // force_global: the def says `global name`, so the
                // function lives at module level under its raw name.
                return name.to_owned();
            }
        }
        match self.eff_flavor {
            Flavor::Module => name.to_owned(),
            Flavor::Function => format!("{}.<locals>.{}", self.qualname, name),
            Flavor::Class => format!("{}.{}", self.qualname, name),
        }
    }

    /// The cells a nested scope closes over, in its symbol order.
    fn child_cells(
        &mut self,
        scope: &'a SymbolTable,
        pos: Pos,
    ) -> Result<Vec<(String, Value)>, LowerError> {
        let mut out = Vec::new();
        for &(s, flags) in &scope.symbols {
            if scope_of(flags) != symtable::FREE && flags & symtable::DEF_FREE_CLASS == 0 {
                continue;
            }
            let name = self.text(s);
            // `__class__` and `__classdict__` are free in direct children
            // but are not class-scope symbols; they bind to the implicit
            // cells the class body fabricates (CPython's classcell and
            // classdictcell).
            if self.kind == FnKind::Class {
                if name == "__class__"
                    && let Some(cell) = self.class_cell
                {
                    out.push((name, cell));
                    continue;
                }
                if name == "__classdict__"
                    && let Some(cell) = self.classdict_cell
                {
                    out.push((name, cell));
                    continue;
                }
            }
            out.push((name.clone(), self.cell_of(&name, pos)?));
        }
        Ok(out)
    }

    /// An annotation expression. `*Ts` (PEP 646, only on `*args`) evaluates
    /// as CPython compiles it: the operand must unpack into exactly one
    /// element, which becomes a one-tuple.
    fn annotation_value(&mut self, e: &Expr) -> Result<Value, LowerError> {
        if let Expr::Starred { value, .. } = e {
            let pos = expr_pos(e);
            let src = self.expr(value)?;
            let into = self.fresh_top();
            self.emit(
                Instr::Unpack {
                    src,
                    n: 1,
                    star: None,
                    into: Box::from([into]),
                },
                pos,
            )?;
            return self.build_tuple(vec![into], pos);
        }
        self.expr(e)
    }

    /// The `__annotations__` dict of a function, built in the defining
    /// scope; `None` when there is nothing to record.
    fn lower_annotations_dict(
        &mut self,
        args: &Arguments,
        returns: &Option<Box<Expr>>,
        pos: Pos,
    ) -> Result<Option<Value>, LowerError> {
        let mut items: Vec<(Value, Value)> = Vec::new();
        let anns = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .chain(args.vararg.iter().map(|a| &**a))
            .chain(args.kwarg.iter().map(|a| &**a))
            .filter(|a| a.annotation.is_some());
        for arg in anns {
            let annotation = arg.annotation.as_deref().expect("filtered");
            let key_str = self.mangle(&self.text(arg.arg));
            let kv = self.const_str(&key_str, pos)?;
            let vv = if self.low.future_annotations {
                self.const_str(&self.low.span_text(annotation), pos)?
            } else {
                self.annotation_value(annotation)?
            };
            items.push((kv, vv));
        }
        if let Some(returns) = returns {
            let kv = self.const_str("return", pos)?;
            let vv = if self.low.future_annotations {
                self.const_str(&self.low.span_text(returns), pos)?
            } else {
                self.annotation_value(returns)?
            };
            items.push((kv, vv));
        }
        if items.is_empty() {
            return Ok(None);
        }
        let v = self.build_dict(items, pos)?;
        Ok(Some(v))
    }

    fn set_doc_attr(&mut self, fv: Value, body: &[Stmt], pos: Pos) -> Result<(), LowerError> {
        if self.low.optimize < 2
            && let Some(doc) = docstring(body, self.low.interner)
        {
            let dv = self.const_str(&doc, pos)?;
            let name = self.pool("__doc__");
            self.emit(
                Instr::SetAttr {
                    obj: fv,
                    name,
                    value: dv,
                },
                pos,
            )?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_function_def(
        &mut self,
        name: &FSym,
        args: &Arguments,
        body: &[Stmt],
        decorator_list: &[Expr],
        returns: &Option<Box<Expr>>,
        type_params: &[TypeParam],
        async_def: bool,
        pos: Pos,
    ) -> Result<(), LowerError> {
        // The symtable visits the defaults' sub-scopes before the
        // decorators', but the code generator evaluates the decorator
        // expressions first; requeue the statement's scopes so both orders
        // hold.
        let n_defaults = args.defaults.iter().map(expr_scopes).sum::<usize>()
            + args
                .kw_defaults
                .iter()
                .flatten()
                .map(expr_scopes)
                .sum::<usize>();
        let n_decors = decorator_list.iter().map(expr_scopes).sum::<usize>();
        self.requeue(n_defaults, n_decors);
        let mut decos = Vec::with_capacity(decorator_list.len());
        for d in decorator_list {
            decos.push(self.expr(d)?);
        }
        let (defaults, kwdefaults) = self.lower_default_values(args, pos)?;
        let raw = self.text(*name);
        let qualname = self.child_qualname(&raw, FnKind::Function);
        let (fid, fv, annotations) = if type_params.is_empty() {
            // Decorators, defaults and annotations are visited before the
            // function scope, so their sub-scopes precede it in order.
            let annotations = self.lower_annotations_dict(args, returns, pos)?;
            let scope = self.take_child().ok_or(LowerError::UnexpectedAst {
                what: "missing function scope",
                line: pos.line,
            })?;
            let cells = self.child_cells(scope, pos)?;
            let params = param_names(self, args);
            let fid = self.low.reserve_function();
            let private = self.private.clone();
            let mangled = self.mangled.clone();
            let mut child = self.low.new_fn(
                scope,
                FnKind::Function,
                &raw,
                &qualname,
                params,
                private,
                mangled,
                (Flavor::Function, scope),
            );
            child.async_def = async_def;
            let landing = if scope.generator || scope.coroutine {
                Some(child.push_stopiteration_wrapper()?)
            } else {
                None
            };
            child.lower_body(body)?;
            child.return_none()?;
            if let Some(landing) = landing {
                child.fill_stopiteration_landing(landing, pos)?;
            }
            child.finish_into(fid)?;
            let fv = self.make_function_value(fid, cells, defaults, kwdefaults, pos)?;
            (fid, fv, annotations)
        } else {
            let (fid, fv) = self.lower_generic_function(
                &raw,
                args,
                body,
                returns,
                type_params,
                async_def,
                defaults,
                kwdefaults,
                pos,
            )?;
            (fid, fv, None)
        };
        if let Some(dict) = annotations {
            let name_sym = self.pool("__annotations__");
            self.emit(
                Instr::SetAttr {
                    obj: fv,
                    name: name_sym,
                    value: dict,
                },
                pos,
            )?;
        }
        self.set_doc_attr(fv, body, pos)?;
        let mut v = fv;
        for d in decos.into_iter().rev() {
            v = self.call(d, vec![v], Vec::new(), pos)?;
        }
        self.store_name_value(*name, v, pos)?;
        if self.kind == FnKind::Class {
            let key = self.mangle(&raw);
            let sym = self.pool(&key);
            self.methods.push((sym, fid));
        }
        Ok(())
    }

    /// A PEP 695 generic function: a synthetic function that builds the type
    /// parameters, the closure of the real function, and returns it, called
    /// here with the defaults as arguments (CPython's type-parameters scope).
    #[allow(clippy::too_many_arguments)]
    fn lower_generic_function(
        &mut self,
        raw: &str,
        args: &Arguments,
        body: &[Stmt],
        returns: &Option<Box<Expr>>,
        type_params: &[TypeParam],
        async_def: bool,
        defaults: Value,
        kwdefaults: Value,
        pos: Pos,
    ) -> Result<(FnId, Value), LowerError> {
        let tp_qualname = self.child_qualname(
            &format!("<generic parameters of {raw}>"),
            FnKind::TypeParams,
        );
        let tp_scope = self.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing type parameters scope",
            line: pos.line,
        })?;
        let has_defaults = !args.defaults.is_empty();
        let has_kwdefaults = args.kw_defaults.iter().any(Option::is_some);
        let mut tp_positional = Vec::new();
        if has_defaults {
            tp_positional.push(".defaults".to_owned());
        }
        if has_kwdefaults {
            tp_positional.push(".kwdefaults".to_owned());
        }
        let tp_params = ParamSpec {
            positional: tp_positional,
            vararg: None,
            kwonly: Vec::new(),
            kwarg: None,
        };
        let tp_cells = self.child_cells(tp_scope, pos)?;
        let tp_fid = self.low.reserve_function();
        let private = self.private.clone();
        let mangled = self.mangled.clone();
        let fn_qualname = self.child_qualname(raw, FnKind::Function);
        let eff = (self.eff_flavor, self.eff_scope);
        let mut tp = self.low.new_fn(
            tp_scope,
            FnKind::TypeParams,
            raw,
            &tp_qualname,
            tp_params,
            private,
            mangled,
            eff,
        );
        // build the type parameters and the real function inside; CPython
        // evaluates the annotations inside the type-parameters scope, after
        // the parameters themselves
        let (tp_tuple, _typing) = tp.lower_type_params(type_params, pos)?;
        let d = if has_defaults { Value(0) } else { tp.none()? };
        let k = if has_kwdefaults {
            Value(u32::from(has_defaults))
        } else {
            tp.none()?
        };
        let annotations = tp.lower_annotations_dict(args, returns, pos)?;
        let fid = tp.low.reserve_function();
        let scope = tp.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing function scope",
            line: pos.line,
        })?;
        let cells = tp.child_cells(scope, pos)?;
        let params = param_names(&mut tp, args);
        let private = tp.private.clone();
        let mangled = tp.mangled.clone();
        let mut child = tp.low.new_fn(
            scope,
            FnKind::Function,
            raw,
            &fn_qualname,
            params,
            private,
            mangled,
            (Flavor::Function, scope),
        );
        child.async_def = async_def;
        let landing = if scope.generator || scope.coroutine {
            Some(child.push_stopiteration_wrapper()?)
        } else {
            None
        };
        child.lower_body(body)?;
        child.return_none()?;
        if let Some(landing) = landing {
            child.fill_stopiteration_landing(landing, pos)?;
        }
        child.finish_into(fid)?;
        let fv = tp.make_function_value(fid, cells, d, k, pos)?;
        let name_sym = tp.pool("__type_params__");
        tp.emit(
            Instr::SetAttr {
                obj: fv,
                name: name_sym,
                value: tp_tuple,
            },
            pos,
        )?;
        if let Some(dict) = annotations {
            let name_sym = tp.pool("__annotations__");
            tp.emit(
                Instr::SetAttr {
                    obj: fv,
                    name: name_sym,
                    value: dict,
                },
                pos,
            )?;
        }
        tp.set_doc_attr(fv, body, pos)?;
        tp.ret(fv)?;
        tp.finish_into(tp_fid)?;
        // the outer side: closure of the type-parameters function, called
        // with the defaults
        let none_v = self.none()?;
        let closure = self.value_of(pos, move |into| Instr::MakeFunction {
            func: tp_fid,
            cells: tp_cells
                .into_iter()
                .map(|(_, v)| v)
                .collect::<Vec<_>>()
                .into(),
            defaults: Box::from([none_v, none_v]),
            into,
        })?;
        let mut call_args = Vec::new();
        if has_defaults {
            call_args.push(defaults);
        }
        if has_kwdefaults {
            call_args.push(kwdefaults);
        }
        let fv = self.call(closure, call_args, Vec::new(), pos)?;
        Ok((fid, fv))
    }

    /// CPython's `compiler_type_params`, approximated with `typing`
    /// constructors. Bounds are evaluated eagerly (CPython defers them
    /// behind the thunk).
    fn lower_type_params(
        &mut self,
        tps: &[TypeParam],
        pos: Pos,
    ) -> Result<(Value, Value), LowerError> {
        let typing_sym = self.pool("typing");
        let none_v = self.none()?;
        let zero_v = self.const_int(0, pos)?;
        let typing = self.value_of(pos, move |into| Instr::Import {
            name: typing_sym,
            fromlist: none_v,
            level: zero_v,
            into,
        })?;
        let mut vars = Vec::with_capacity(tps.len());
        for tp in tps {
            match tp {
                TypeParam::TypeVar { name, bound, .. } => {
                    let name_str = self.text(*name);
                    let ctor_sym = self.pool("TypeVar");
                    let ctor = self.value_of(pos, move |into| Instr::GetAttr {
                        obj: typing,
                        name: ctor_sym,
                        into,
                    })?;
                    let name_v = self.const_str(&name_str, pos)?;
                    let v = match bound {
                        Some(bound) => {
                            let thunk_qual = self.child_qualname(&name_str, FnKind::TypeVarBound);
                            let bound_val =
                                self.lower_bound_thunk(bound, &name_str, &thunk_qual, pos)?;
                            if matches!(&**bound, Expr::Tuple { .. }) {
                                self.call(ctor, vec![name_v, bound_val], Vec::new(), pos)?
                            } else {
                                let kw_sym = self.pool("bound");
                                self.call(ctor, vec![name_v], vec![(kw_sym, bound_val)], pos)?
                            }
                        }
                        None => self.call(ctor, vec![name_v], Vec::new(), pos)?,
                    };
                    vars.push(v);
                    // The symtable records the class-mangled name
                    // (`__T` as `_C__T`); the TypeVar's own string stays
                    // raw, as CPython passes it.
                    let key = self.mangle(&name_str);
                    self.store_scoped_name(&key, v, pos)?;
                }
                TypeParam::TypeVarTuple { name, .. } => {
                    let name_str = self.text(*name);
                    let ctor_sym = self.pool("TypeVarTuple");
                    let ctor = self.value_of(pos, move |into| Instr::GetAttr {
                        obj: typing,
                        name: ctor_sym,
                        into,
                    })?;
                    let name_v = self.const_str(&name_str, pos)?;
                    let v = self.call(ctor, vec![name_v], Vec::new(), pos)?;
                    vars.push(v);
                    let key = self.mangle(&name_str);
                    self.store_scoped_name(&key, v, pos)?;
                }
                TypeParam::ParamSpec { name, .. } => {
                    let name_str = self.text(*name);
                    let ctor_sym = self.pool("ParamSpec");
                    let ctor = self.value_of(pos, move |into| Instr::GetAttr {
                        obj: typing,
                        name: ctor_sym,
                        into,
                    })?;
                    let name_v = self.const_str(&name_str, pos)?;
                    let v = self.call(ctor, vec![name_v], Vec::new(), pos)?;
                    vars.push(v);
                    let key = self.mangle(&name_str);
                    self.store_scoped_name(&key, v, pos)?;
                }
            }
        }
        let tuple = self.build_tuple(vars, pos)?;
        Ok((tuple, typing))
    }

    /// The bound of a TypeVar: a real nested function (the symtable's
    /// TypeVarBound scope), called immediately.
    fn lower_bound_thunk(
        &mut self,
        bound: &Expr,
        name_str: &str,
        qualname: &str,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let scope = self.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing type-var bound scope",
            line: pos.line,
        })?;
        let fid = self.low.reserve_function();
        let cells = self.child_cells(scope, pos)?;
        let private = self.private.clone();
        let mangled = self.mangled.clone();
        let eff = (self.eff_flavor, self.eff_scope);
        let mut child = self.low.new_fn(
            scope,
            FnKind::TypeVarBound,
            name_str,
            qualname,
            ParamSpec::none(),
            private,
            mangled,
            eff,
        );
        let v = child.expr(bound)?;
        child.ret(v)?;
        child.finish_into(fid)?;
        let none_v = self.none()?;
        let cell_vals: Vec<Value> = cells.into_iter().map(|(_, v)| v).collect();
        let thunk = self.value_of(pos, move |into| Instr::MakeFunction {
            func: fid,
            cells: cell_vals.into(),
            defaults: Box::from([none_v, none_v]),
            into,
        })?;
        self.call(thunk, Vec::new(), Vec::new(), pos)
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_class_def(
        &mut self,
        name: &FSym,
        bases: &[Expr],
        keywords: &[Keyword],
        body: &[Stmt],
        decorator_list: &[Expr],
        type_params: &[TypeParam],
        pos: Pos,
    ) -> Result<(), LowerError> {
        let mut decos = Vec::with_capacity(decorator_list.len());
        for d in decorator_list {
            decos.push(self.expr(d)?);
        }
        let raw = self.text(*name);
        let qualname = self.child_qualname(&raw, FnKind::Class);
        let class_id = self.low.reserve_class();
        let dyn_bases = bases.iter().any(|b| matches!(b, Expr::Starred { .. }));
        let (body_fid, cls, methods) = if type_params.is_empty() {
            // Bases and keywords are evaluated before the class body runs;
            // their sub-scopes precede the class scope in symtable order.
            let bases_list = self.star_bases_list(bases, None, pos)?;
            let mut bases_v = Vec::with_capacity(bases.len());
            if bases_list.is_none() {
                for b in bases {
                    bases_v.push(self.expr(b)?);
                }
            }
            let kwds = self.class_kwds(keywords, pos)?;
            let scope = self.take_child().ok_or(LowerError::UnexpectedAst {
                what: "missing class scope",
                line: pos.line,
            })?;
            let cells = self.child_cells(scope, pos)?;
            let fid = self.low.reserve_function();
            let mut child = self.low.new_fn(
                scope,
                FnKind::Class,
                &raw,
                &qualname,
                ParamSpec::none(),
                Some(raw.clone()),
                None,
                (Flavor::Class, scope),
            );
            child.lower_class_body(body, false, pos)?;
            let methods = std::mem::take(&mut child.methods);
            child.finish_into(fid)?;
            let cell_vals: Vec<Value> = cells.into_iter().map(|(_, v)| v).collect();
            let cls = if let Some(lst) = bases_list {
                self.build_class_call(fid, cell_vals, &raw, lst, kwds, pos)?
            } else {
                self.value_of(pos, move |into| Instr::MakeClass {
                    desc: class_id,
                    bases: bases_v.into(),
                    cells: cell_vals.into(),
                    kwds,
                    into,
                })?
            };
            (fid, cls, methods)
        } else {
            let tp_qualname = self.child_qualname(
                &format!("<generic parameters of {raw}>"),
                FnKind::TypeParams,
            );
            let tp_scope = self.take_child().ok_or(LowerError::UnexpectedAst {
                what: "missing type parameters scope",
                line: pos.line,
            })?;
            let tp_cells = self.child_cells(tp_scope, pos)?;
            let tp_fid = self.low.reserve_function();
            let eff = (self.eff_flavor, self.eff_scope);
            let mut tp = self.low.new_fn(
                tp_scope,
                FnKind::TypeParams,
                &raw,
                &tp_qualname,
                ParamSpec::none(),
                Some(raw.clone()),
                type_param_names(self, tp_scope),
                eff,
            );
            let (tp_tuple, typing) = tp.lower_type_params(type_params, pos)?;
            tp.store_scoped_name(".type_params", tp_tuple, pos)?;
            // the synthetic Generic[...] base
            let generic_sym = tp.pool("Generic");
            let g = tp.value_of(pos, move |into| Instr::GetAttr {
                obj: typing,
                name: generic_sym,
                into,
            })?;
            let gbase = tp.value_of(pos, move |into| Instr::GetItem {
                obj: g,
                key: tp_tuple,
                into,
            })?;
            tp.store_scoped_name(".generic_base", gbase, pos)?;
            // Bases and keywords are evaluated before the class body runs;
            // their sub-scopes precede the class scope in symtable order.
            let bases_list = tp.star_bases_list(bases, Some(gbase), pos)?;
            let mut bases_v = Vec::with_capacity(bases.len() + 1);
            if bases_list.is_none() {
                for b in bases {
                    bases_v.push(tp.expr(b)?);
                }
            }
            let kwds = tp.class_kwds(keywords, pos)?;
            let fid = tp.low.reserve_function();
            let scope = tp.take_child().ok_or(LowerError::UnexpectedAst {
                what: "missing class scope",
                line: pos.line,
            })?;
            let cells = tp.child_cells(scope, pos)?;
            let mut child = tp.low.new_fn(
                scope,
                FnKind::Class,
                &raw,
                &qualname,
                ParamSpec::none(),
                Some(raw.clone()),
                None,
                (Flavor::Class, scope),
            );
            child.lower_class_body(body, true, pos)?;
            let methods = std::mem::take(&mut child.methods);
            child.finish_into(fid)?;
            if bases_list.is_none() {
                bases_v.push(gbase);
            }
            let cell_vals: Vec<Value> = cells.into_iter().map(|(_, v)| v).collect();
            let cls = if let Some(lst) = bases_list {
                tp.build_class_call(fid, cell_vals, &raw, lst, kwds, pos)?
            } else {
                tp.value_of(pos, move |into| Instr::MakeClass {
                    desc: class_id,
                    bases: bases_v.into(),
                    cells: cell_vals.into(),
                    kwds,
                    into,
                })?
            };
            tp.ret(cls)?;
            tp.finish_into(tp_fid)?;
            let none_v = self.none()?;
            let closure = self.value_of(pos, move |into| Instr::MakeFunction {
                func: tp_fid,
                cells: tp_cells
                    .into_iter()
                    .map(|(_, v)| v)
                    .collect::<Vec<_>>()
                    .into(),
                defaults: Box::from([none_v, none_v]),
                into,
            })?;
            let cls = self.call(closure, Vec::new(), Vec::new(), pos)?;
            (fid, cls, methods)
        };
        self.low.classes[class_id.0 as usize] = ClassDesc {
            name: self.pool(&raw),
            // A runtime base list has no static shape to record.
            bases: if dyn_bases {
                Vec::new().into()
            } else {
                vec![Type::Top; bases.len()].into()
            },
            body: body_fid,
            methods: methods.into(),
            has_ext_base: false,
        };
        let mut v = cls;
        for d in decos.into_iter().rev() {
            v = self.call(d, vec![v], Vec::new(), pos)?;
        }
        self.store_name_value(*name, v, pos)?;
        Ok(())
    }

    /// The runtime base list of `class C(*a, b)`: CPython's LIST_EXTEND /
    /// LIST_APPEND sequence, with `extra` (the synthetic Generic base)
    /// appended last. `None` when no base is starred and the base list is
    /// static.
    fn star_bases_list(
        &mut self,
        bases: &[Expr],
        extra: Option<Value>,
        pos: Pos,
    ) -> Result<Option<Value>, LowerError> {
        if !bases.iter().any(|b| matches!(b, Expr::Starred { .. })) {
            return Ok(None);
        }
        let lst = self.build_list(Vec::new(), pos)?;
        for b in bases {
            let (v, how) = match b {
                Expr::Starred { value, .. } => (self.expr(value)?, "extend"),
                other => (self.expr(other)?, "append"),
            };
            let m = self.method(&lst, how, pos)?;
            self.call(m, vec![v], Vec::new(), pos)?;
        }
        if let Some(g) = extra {
            let m = self.method(&lst, "append", pos)?;
            self.call(m, vec![g], Vec::new(), pos)?;
        }
        Ok(Some(lst))
    }

    /// `__build_class__(body, name, *bases, **kwds)`: CPython's own dynamic
    /// form for a base list built at runtime, which the static `MakeClass`
    /// shape cannot express. `bases` is the prebuilt runtime list.
    fn build_class_call(
        &mut self,
        body_fid: FnId,
        cells: Vec<Value>,
        raw: &str,
        bases_list: Value,
        kwds: Value,
        pos: Pos,
    ) -> Result<Value, LowerError> {
        let bc_sym = self.pool("__build_class__");
        let bc = self.value_of(pos, move |into| Instr::LoadGlobal { name: bc_sym, into })?;
        let none_v = self.none()?;
        let body = self.value_of(pos, move |into| Instr::MakeFunction {
            func: body_fid,
            cells: cells.into(),
            defaults: Box::from([none_v, none_v]),
            into,
        })?;
        let name_v = self.const_str(raw, pos)?;
        let lst = self.build_list(vec![body, name_v], pos)?;
        let m = self.method(&lst, "extend", pos)?;
        self.call(m, vec![bases_list], Vec::new(), pos)?;
        let args = self.call_global("tuple", vec![lst], pos)?;
        self.value_of(pos, move |into| Instr::CallEx {
            callee: bc,
            args,
            kwargs: kwds,
            into,
        })
    }

    /// The `**kwds` value for `__build_class__`: a dict or the None const.
    fn class_kwds(&mut self, keywords: &[Keyword], pos: Pos) -> Result<Value, LowerError> {
        if keywords.is_empty() {
            return self.none();
        }
        let d = self.build_dict(Vec::new(), pos)?;
        for k in keywords {
            match k.arg {
                Some(name) => {
                    let name_str = self.text(name);
                    let kv = self.const_str(&name_str, pos)?;
                    let vv = self.expr(&k.value)?;
                    self.emit(
                        Instr::SetItem {
                            obj: d,
                            key: kv,
                            value: vv,
                        },
                        pos,
                    )?;
                }
                None => {
                    let vv = self.expr(&k.value)?;
                    let m = self.method(&d, "update", pos)?;
                    self.call(m, vec![vv], Vec::new(), pos)?;
                }
            }
        }
        Ok(d)
    }

    fn lower_class_body(
        &mut self,
        body: &[Stmt],
        generic: bool,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let name_v = self.load_plain_name("__name__", pos)?;
        self.store_plain_name("__module__", name_v, pos)?;
        let qualname = self.qualname.clone();
        let q = self.const_str(&qualname, pos)?;
        self.store_plain_name("__qualname__", q, pos)?;
        if generic {
            let tp = self.load_scoped_name(".type_params", ".type_params", pos)?;
            self.store_plain_name("__type_params__", tp, pos)?;
        }
        let needs_classdict = self.scope.children.iter().any(|c| {
            c.symbols
                .iter()
                .any(|&(s, f)| scope_of(f) == symtable::FREE && self.text(s) == "__classdict__")
        });
        if needs_classdict {
            let cell = self.value_of(pos, |into| Instr::MakeCell {
                initial: None,
                into,
            })?;
            let locals_sym = self.pool("locals");
            let locals_fn = self.value_of(pos, move |into| Instr::LoadName {
                name: locals_sym,
                into,
            })?;
            let d = self.call(locals_fn, Vec::new(), Vec::new(), pos)?;
            self.emit(Instr::StoreCell { cell, value: d }, pos)?;
            self.classdict_cell = Some(cell);
            let sym = self.pool("__classdict__");
            self.b.add_local(LocalMeta {
                name: sym,
                kind: LocalKind::Cell,
            });
        }
        let needs_closure = self.scope.children.iter().any(|c| {
            c.symbols
                .iter()
                .any(|&(s, f)| scope_of(f) == symtable::FREE && self.text(s) == "__class__")
        });
        if needs_closure {
            let cell = self.value_of(pos, |into| Instr::MakeCell {
                initial: None,
                into,
            })?;
            self.class_cell = Some(cell);
            let sym = self.pool("__class__");
            self.b.add_local(LocalMeta {
                name: sym,
                kind: LocalKind::Cell,
            });
        }
        self.lower_namespace_body(body)?;
        if let Some(cell) = self.classdict_cell {
            let v = self.value_of(pos, move |into| Instr::LoadCell { cell, into })?;
            self.store_plain_name("__classdictcell__", v, pos)?;
        }
        if let Some(cell) = self.class_cell {
            let v = self.value_of(pos, move |into| Instr::LoadCell { cell, into })?;
            self.store_plain_name("__classcell__", v, pos)?;
            self.ret(v)?;
        } else {
            self.return_none()?;
        }
        Ok(())
    }

    /// The `typing.TypeAliasType` constructor and the alias-name constant,
    /// built in this function so their values never cross a scope boundary.
    fn alias_ctor(&mut self, raw: &str, pos: Pos) -> Result<(Value, Value), LowerError> {
        let typing_sym = self.pool("typing");
        let none_v = self.none()?;
        let zero_v = self.const_int(0, pos)?;
        let typing = self.value_of(pos, move |into| Instr::Import {
            name: typing_sym,
            fromlist: none_v,
            level: zero_v,
            into,
        })?;
        let ctor_sym = self.pool("TypeAliasType");
        let ctor = self.value_of(pos, move |into| Instr::GetAttr {
            obj: typing,
            name: ctor_sym,
            into,
        })?;
        let name_v = self.const_str(raw, pos)?;
        Ok((ctor, name_v))
    }

    /// A PEP 695 `type X = ...` statement: a lazy value function handed to
    /// `typing.TypeAliasType`.
    fn lower_type_alias(
        &mut self,
        name: &Expr,
        type_params: &[TypeParam],
        value: &Expr,
        pos: Pos,
    ) -> Result<(), LowerError> {
        let Expr::Name { id, .. } = name else {
            return Err(LowerError::UnexpectedAst {
                what: "type alias target is not a name",
                line: pos.line,
            });
        };
        let raw = self.text(*id);
        let thunk_qualname = self.child_qualname(&raw, FnKind::TypeAlias);
        if type_params.is_empty() {
            let (ctor, name_v) = self.alias_ctor(&raw, pos)?;
            let scope = self.take_child().ok_or(LowerError::UnexpectedAst {
                what: "missing type alias scope",
                line: pos.line,
            })?;
            let fid = self.low.reserve_function();
            let cells = self.child_cells(scope, pos)?;
            let private = self.private.clone();
            let mangled = self.mangled.clone();
            let mut child = self.low.new_fn(
                scope,
                FnKind::TypeAlias,
                &raw,
                &thunk_qualname,
                ParamSpec::none(),
                private,
                mangled,
                (Flavor::Function, scope),
            );
            let v = child.expr(value)?;
            child.ret(v)?;
            child.finish_into(fid)?;
            let dv = self.none()?;
            let cell_vals: Vec<Value> = cells.into_iter().map(|(_, v)| v).collect();
            let thunk = self.value_of(pos, move |into| Instr::MakeFunction {
                func: fid,
                cells: cell_vals.into(),
                defaults: Box::from([dv, dv]),
                into,
            })?;
            let kw_sym = self.pool("type_params");
            let none_v = self.none()?;
            let alias = self.call(ctor, vec![name_v, thunk], vec![(kw_sym, none_v)], pos)?;
            self.store_name_value(*id, alias, pos)?;
            return Ok(());
        }
        let tp_qualname = self.child_qualname(
            &format!("<generic parameters of {raw}>"),
            FnKind::TypeParams,
        );
        let tp_scope = self.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing type parameters scope",
            line: pos.line,
        })?;
        let tp_cells = self.child_cells(tp_scope, pos)?;
        let tp_fid = self.low.reserve_function();
        let private = self.private.clone();
        let mangled = self.mangled.clone();
        let eff = (self.eff_flavor, self.eff_scope);
        let mut tp = self.low.new_fn(
            tp_scope,
            FnKind::TypeParams,
            &raw,
            &tp_qualname,
            ParamSpec::none(),
            private,
            mangled,
            eff,
        );
        let (tp_tuple, _typing) = tp.lower_type_params(type_params, pos)?;
        let (ctor, name_v) = tp.alias_ctor(&raw, pos)?;
        let scope = tp.take_child().ok_or(LowerError::UnexpectedAst {
            what: "missing type alias scope",
            line: pos.line,
        })?;
        let fid = tp.low.reserve_function();
        let cells = tp.child_cells(scope, pos)?;
        let mut child = tp.low.new_fn(
            scope,
            FnKind::TypeAlias,
            &raw,
            &thunk_qualname,
            ParamSpec::none(),
            tp.private.clone(),
            tp.mangled.clone(),
            (Flavor::Function, scope),
        );
        let v = child.expr(value)?;
        child.ret(v)?;
        child.finish_into(fid)?;
        let dv = tp.none()?;
        let cell_vals: Vec<Value> = cells.into_iter().map(|(_, v)| v).collect();
        let thunk = tp.value_of(pos, move |into| Instr::MakeFunction {
            func: fid,
            cells: cell_vals.into(),
            defaults: Box::from([dv, dv]),
            into,
        })?;
        let kw_sym = tp.pool("type_params");
        let alias = tp.call(ctor, vec![name_v, thunk], vec![(kw_sym, tp_tuple)], pos)?;
        tp.ret(alias)?;
        tp.finish_into(tp_fid)?;
        let outer_none = self.none()?;
        let closure = self.value_of(pos, move |into| Instr::MakeFunction {
            func: tp_fid,
            cells: tp_cells
                .into_iter()
                .map(|(_, v)| v)
                .collect::<Vec<_>>()
                .into(),
            defaults: Box::from([outer_none, outer_none]),
            into,
        })?;
        let alias = self.call(closure, Vec::new(), Vec::new(), pos)?;
        self.store_name_value(*id, alias, pos)?;
        Ok(())
    }
}

fn type_param_names(f: &FnLower, scope: &SymbolTable) -> Option<HashSet<String>> {
    let mut set = HashSet::new();
    let mut any = false;
    for &(s, flags) in &scope.symbols {
        if flags & symtable::DEF_TYPE_PARAM != 0 {
            set.insert(f.text(s));
            any = true;
        }
    }
    if any { Some(set) } else { None }
}

// ---------------------------------------------------------------------------
// Symtable child accounting
// ---------------------------------------------------------------------------

fn target_scopes(e: &Expr) -> usize {
    match e {
        Expr::Name { .. } => 0,
        Expr::Attribute { value, .. } => expr_scopes(value),
        Expr::Subscript { value, slice, .. } => expr_scopes(value) + slice_scopes(slice),
        Expr::Tuple { elts, .. } | Expr::List { elts, .. } => elts.iter().map(target_scopes).sum(),
        Expr::Starred { value, .. } => target_scopes(value),
        _ => 0,
    }
}

fn slice_scopes(e: &Expr) -> usize {
    match e {
        Expr::Slice {
            lower, upper, step, ..
        } => {
            lower.as_deref().map(expr_scopes).unwrap_or(0)
                + upper.as_deref().map(expr_scopes).unwrap_or(0)
                + step.as_deref().map(expr_scopes).unwrap_or(0)
        }
        other => expr_scopes(other),
    }
}

/// Scopes this expression occupies in the enclosing scope's children list.
fn expr_scopes(e: &Expr) -> usize {
    match e {
        Expr::Lambda { args, .. } => {
            args.defaults.iter().map(expr_scopes).sum::<usize>()
                + args
                    .kw_defaults
                    .iter()
                    .flatten()
                    .map(expr_scopes)
                    .sum::<usize>()
                + 1
        }
        Expr::ListComp {
            elt, generators, ..
        }
        | Expr::SetComp {
            elt, generators, ..
        } => comp_scopes(generators, elt, None),
        Expr::DictComp {
            key,
            value,
            generators,
            ..
        } => comp_scopes(generators, key, Some(value)),
        Expr::GeneratorExp { generators, .. } => {
            // its scope stays whole in the parent's list (its inner scopes
            // are nested inside it); the outermost iter still evaluates here
            expr_scopes(&generators[0].iter) + 1
        }
        Expr::BoolOp { values, .. } => values.iter().map(expr_scopes).sum(),
        Expr::NamedExpr { value, .. } => expr_scopes(value),
        Expr::BinOp { left, right, .. } => expr_scopes(left) + expr_scopes(right),
        Expr::UnaryOp { operand, .. } => expr_scopes(operand),
        Expr::IfExp {
            test, body, orelse, ..
        } => expr_scopes(test) + expr_scopes(body) + expr_scopes(orelse),
        Expr::Dict { keys, values, .. } => {
            keys.iter().flatten().map(expr_scopes).sum::<usize>()
                + values.iter().map(expr_scopes).sum::<usize>()
        }
        Expr::Set { elts, .. } | Expr::List { elts, .. } | Expr::Tuple { elts, .. } => elts
            .iter()
            .map(|e| match e {
                Expr::Starred { value, .. } => expr_scopes(value),
                other => expr_scopes(other),
            })
            .sum(),
        Expr::Compare {
            left, comparators, ..
        } => expr_scopes(left) + comparators.iter().map(expr_scopes).sum::<usize>(),
        Expr::Call {
            func,
            args,
            keywords,
            ..
        } => {
            expr_scopes(func)
                + args
                    .iter()
                    .map(|a| match a {
                        Expr::Starred { value, .. } => expr_scopes(value),
                        other => expr_scopes(other),
                    })
                    .sum::<usize>()
                + keywords
                    .iter()
                    .map(|k| expr_scopes(&k.value))
                    .sum::<usize>()
        }
        Expr::FormattedValue {
            value, format_spec, ..
        } => expr_scopes(value) + format_spec.as_deref().map(expr_scopes).unwrap_or(0),
        Expr::JoinedStr { values, .. } => values.iter().map(expr_scopes).sum(),
        Expr::Constant { .. } | Expr::Name { .. } => 0,
        Expr::Attribute { value, .. } => expr_scopes(value),
        Expr::Subscript { value, slice, .. } => expr_scopes(value) + slice_scopes(slice),
        Expr::Starred { value, .. } => expr_scopes(value),
        Expr::Slice { .. } => 0,
        Expr::Await { value, .. } => expr_scopes(value),
        Expr::Yield { value, .. } => value.as_deref().map(expr_scopes).unwrap_or(0),
        Expr::YieldFrom { value, .. } => expr_scopes(value),
    }
}

/// An inlined comprehension occupies its inner scopes' slots in the parent:
/// the outermost iterator's scopes, plus everything the comprehension scope
/// itself created.
fn comp_scopes(generators: &[Comprehension], elt: &Expr, value: Option<&Expr>) -> usize {
    let mut n = expr_scopes(&generators[0].iter);
    n += generators[0].ifs.iter().map(expr_scopes).sum::<usize>();
    for g in &generators[1..] {
        n += target_scopes(&g.target) + expr_scopes(&g.iter);
        n += g.ifs.iter().map(expr_scopes).sum::<usize>();
    }
    n += expr_scopes(elt);
    if let Some(v) = value {
        n += expr_scopes(v);
    }
    n
}

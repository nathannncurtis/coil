//! A port of CPython 3.12.10's `Python/symtable.c` over [`crate::ast`]: the
//! two passes that turn a module AST into symbol tables. The first pass
//! records raw name facts per scope (definitions, uses, `global`/`nonlocal`
//! directives); the second resolves every name to local, global, nonlocal,
//! cell or free, inlines non-generator comprehension scopes into their parent
//! the way 3.12 codegen does, and splices the inlined scopes' children into
//! the parent, so the result matches what the `symtable` module reports.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::intern::{Interner, Sym};

/// Flags for def-use information (`pycore_symtable.h`).
pub const DEF_GLOBAL: i64 = 1;
pub const DEF_LOCAL: i64 = 2;
pub const DEF_PARAM: i64 = 2 << 1;
pub const DEF_NONLOCAL: i64 = 2 << 2;
pub const USE: i64 = 2 << 3;
pub const DEF_FREE: i64 = 2 << 4;
pub const DEF_FREE_CLASS: i64 = 2 << 5;
pub const DEF_IMPORT: i64 = 2 << 6;
pub const DEF_ANNOT: i64 = 2 << 7;
pub const DEF_COMP_ITER: i64 = 2 << 8;
pub const DEF_TYPE_PARAM: i64 = 2 << 9;
pub const DEF_COMP_CELL: i64 = 2 << 10;

pub const DEF_BOUND: i64 = DEF_LOCAL | DEF_PARAM | DEF_IMPORT;

/// Scope values, stored in the flag word at `SCOPE_OFFSET`.
pub const SCOPE_OFFSET: u32 = 12;
pub const SCOPE_MASK: i64 = DEF_GLOBAL | DEF_LOCAL | DEF_PARAM | DEF_NONLOCAL;
pub const LOCAL: i64 = 1;
pub const GLOBAL_EXPLICIT: i64 = 2;
pub const GLOBAL_IMPLICIT: i64 = 3;
pub const FREE: i64 = 4;
pub const CELL: i64 = 5;

const UNDERSCORE: u32 = b'_' as u32;
const DOT: u32 = b'.' as u32;

/// `_Py_block_ty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Module,
    Function,
    Class,
    Annotation,
    TypeVarBound,
    TypeAlias,
    TypeParam,
}

impl BlockKind {
    /// `SymbolTable.get_type()`'s spelling.
    pub fn reported_name(self) -> &'static str {
        match self {
            BlockKind::Module => "module",
            BlockKind::Function => "function",
            BlockKind::Class => "class",
            BlockKind::Annotation => "annotation",
            BlockKind::TypeVarBound => "TypeVar bound",
            BlockKind::TypeAlias => "type alias",
            BlockKind::TypeParam => "type parameter",
        }
    }

    /// `SymbolTable.is_optimized()`.
    pub fn is_optimized(self) -> bool {
        self == BlockKind::Function
    }
}

/// `_Py_comprehension_ty`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComprehensionKind {
    None,
    List,
    Dict,
    Set,
    Generator,
}

/// The exception a failed `symtable` build raises, as `front_diff` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymtableError {
    pub kind: ErrorKind,
    pub message: String,
    pub lineno: Option<i32>,
    pub offset: Option<i32>,
    pub end_lineno: Option<i32>,
    pub end_offset: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Syntax,
    Runtime,
}

impl ErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Syntax => "SyntaxError",
            ErrorKind::Runtime => "RuntimeError",
        }
    }
}

/// A finished symbol table, as the `symtable` module reports it.
pub struct SymbolTable {
    pub name: Sym,
    pub kind: BlockKind,
    pub lineno: i32,
    pub is_nested: bool,
    /// The block contains `yield`/`yield from`.
    pub generator: bool,
    /// The block is a coroutine: `async def`, `await`, or an async
    /// comprehension (including the flag an async comprehension propagates
    /// to its enclosing scope).
    pub coroutine: bool,
    /// Each name's final flag word (scope in the `SCOPE_OFFSET` bits), in
    /// first-seen order.
    pub symbols: Vec<(Sym, i64)>,
    /// The `for` targets of inlined comprehensions in this block: names
    /// whose outer binding is saved while the comprehension runs.
    pub comp_bound: Vec<Sym>,
    /// Child blocks in `get_children()` order.
    pub children: Vec<SymbolTable>,
    /// The inlined comprehensions of the whole module, keyed by the
    /// `(lineno, col_offset)` of the comprehension expression. Carried by
    /// the root table only; empty on children.
    pub inlined_comps: Vec<((i32, i32), InlinedComp)>,
}

/// An inlined comprehension scope, as lowering needs it after analysis.
pub struct InlinedComp {
    pub coroutine: bool,
    /// The comprehension scope's final flag words, `.0` excluded, in
    /// first-seen order.
    pub symbols: Vec<(Sym, i64)>,
}

impl SymbolTable {
    /// The true `symtable.Symbol` predicates for a symbol with `flags`, in the
    /// sorted order `front_oracle.py` reports. `module_scope` is the table
    /// name's "top" quirk in `symtable.py`; `is_namespace` says whether a
    /// child block carries this name.
    pub fn symbol_predicates(
        flags: i64,
        module_scope: bool,
        is_namespace: bool,
    ) -> Vec<&'static str> {
        let scope = (flags >> SCOPE_OFFSET) & SCOPE_MASK;
        [
            ("is_annotated", flags & DEF_ANNOT != 0),
            ("is_assigned", flags & DEF_LOCAL != 0),
            ("is_declared_global", scope == GLOBAL_EXPLICIT),
            ("is_free", scope == FREE),
            (
                "is_global",
                scope == GLOBAL_IMPLICIT
                    || scope == GLOBAL_EXPLICIT
                    || (module_scope && flags & DEF_BOUND != 0),
            ),
            ("is_imported", flags & DEF_IMPORT != 0),
            (
                "is_local",
                scope == LOCAL || scope == CELL || (module_scope && flags & DEF_BOUND != 0),
            ),
            ("is_namespace", is_namespace),
            ("is_nonlocal", flags & DEF_NONLOCAL != 0),
            ("is_parameter", flags & DEF_PARAM != 0),
            ("is_referenced", flags & USE != 0),
        ]
        .into_iter()
        .filter(|(_, holds)| *holds)
        .map(|(name, _)| name)
        .collect()
    }
}

/// A scope's names with their flag words, in insertion order like CPython's
/// dicts; `insert` updates an existing name in place.
#[derive(Default)]
struct SymMap {
    entries: Vec<(Sym, i64)>,
    index: HashMap<Sym, usize>,
}

impl SymMap {
    fn get(&self, name: Sym) -> Option<i64> {
        self.index.get(&name).map(|&i| self.entries[i].1)
    }

    fn insert(&mut self, name: Sym, flags: i64) {
        match self.index.get(&name) {
            Some(&i) => self.entries[i].1 = flags,
            None => {
                self.index.insert(name, self.entries.len());
                self.entries.push((name, flags));
            }
        }
    }

    fn remove(&mut self, name: Sym) {
        if let Some(i) = self.index.remove(&name) {
            self.entries.remove(i);
            for slot in self.index.values_mut() {
                if *slot > i {
                    *slot -= 1;
                }
            }
        }
    }

    fn iter(&self) -> impl Iterator<Item = (Sym, i64)> + '_ {
        self.entries.iter().copied()
    }
}

/// A node or block location as the symtable's errors carry it.
#[derive(Clone, Copy, Debug)]
struct Loc {
    lineno: i32,
    col_offset: i32,
    end_lineno: i32,
    end_col_offset: i32,
}

impl Loc {
    /// A parsed node's location; the end positions are always present.
    fn new(
        lineno: i32,
        col_offset: i32,
        end_lineno: Option<i32>,
        end_col_offset: Option<i32>,
    ) -> Loc {
        Loc {
            lineno,
            col_offset,
            end_lineno: end_lineno.unwrap_or(lineno),
            end_col_offset: end_col_offset.unwrap_or(col_offset),
        }
    }

    fn ranged(self) -> (Option<i32>, Option<i32>, Option<i32>, Option<i32>) {
        (
            Some(self.lineno),
            Some(self.col_offset + 1),
            Some(self.end_lineno),
            Some(self.end_col_offset + 1),
        )
    }
}

/// A scope under construction (`PySTEntryObject`); only the fields the
/// symbol table itself reads are carried.
struct Ste {
    name: Sym,
    symbols: SymMap,
    children: Vec<usize>,
    kind: BlockKind,
    nested: bool,
    free: bool,
    child_free: bool,
    generator: bool,
    coroutine: bool,
    comprehension: ComprehensionKind,
    comp_inlined: bool,
    comp_iter_target: bool,
    can_see_class_scope: bool,
    comp_iter_expr: i32,
    /// The `for` targets of comprehensions inlined into this block.
    comp_bound: Vec<Sym>,
    lineno: i32,
    col_offset: i32,
    end_lineno: i32,
    end_col_offset: i32,
    /// `global`/`nonlocal` directive locations, keyed by mangled name.
    directives: Vec<(Sym, Loc)>,
    /// Type parameter names of a generic class's parameter block: these
    /// mangle while everything else in the block does not.
    mangled_names: Option<HashSet<Sym>>,
}

/// The fixed names the port compares against, interned once.
struct Names {
    top: Sym,
    lambda: Sym,
    genexpr: Sym,
    listcomp: Sym,
    setcomp: Sym,
    dictcomp: Sym,
    annotation: Sym,
    dot0: Sym,
    dot_type_params: Sym,
    dot_generic_base: Sym,
    dot_defaults: Sym,
    dot_kwdefaults: Sym,
    class: Sym,
    classdict: Sym,
    type_params: Sym,
    super_: Sym,
    star: Sym,
}

impl Names {
    fn new(interner: &mut Interner) -> Names {
        Names {
            top: interner.intern("top"),
            lambda: interner.intern("lambda"),
            genexpr: interner.intern("genexpr"),
            listcomp: interner.intern("listcomp"),
            setcomp: interner.intern("setcomp"),
            dictcomp: interner.intern("dictcomp"),
            annotation: interner.intern("_annotation"),
            dot0: interner.intern(".0"),
            dot_type_params: interner.intern(".type_params"),
            dot_generic_base: interner.intern(".generic_base"),
            dot_defaults: interner.intern(".defaults"),
            dot_kwdefaults: interner.intern(".kwdefaults"),
            class: interner.intern("__class__"),
            classdict: interner.intern("__classdict__"),
            type_params: interner.intern("__type_params__"),
            super_: interner.intern("super"),
            star: interner.intern("*"),
        }
    }
}

/// Which statement kind a type parameter block belongs to; the block's
/// synthetic `.type_params`/`.generic_base` names only appear for classes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefKind {
    FunctionDef,
    AsyncFunctionDef,
    ClassDef,
    TypeAlias,
}

struct Builder<'a> {
    interner: &'a mut Interner,
    names: Names,
    future_annotations: bool,
    stes: Vec<Ste>,
    stack: Vec<usize>,
    top: usize,
    private: Option<Sym>,
}

/// Builds the symbol table for a parsed module. `interner` must be the one
/// the module's identifiers came from; mangled names are interned into it.
pub fn symtable(module: &Mod, interner: &mut Interner) -> Result<SymbolTable, SymtableError> {
    let future_annotations = future_annotations(module, interner)?;
    let names = Names::new(interner);
    let mut st = Builder {
        interner,
        names,
        future_annotations,
        stes: Vec::new(),
        stack: Vec::new(),
        top: 0,
        private: None,
    };
    st.enter_block(
        st.names.top,
        BlockKind::Module,
        Loc::new(0, 0, Some(0), Some(0)),
    )?;
    st.top = st.cur();
    let visited = match module {
        Mod::Module { body, .. } | Mod::Interactive { body } => {
            for stmt in body {
                st.visit_stmt(stmt)?;
            }
            Ok(())
        }
        Mod::Expression { body } => st.visit_expr(body),
        Mod::FunctionType { .. } => Err(SymtableError {
            kind: ErrorKind::Runtime,
            message: "this compiler does not handle FunctionTypes".to_string(),
            lineno: None,
            offset: None,
            end_lineno: None,
            end_offset: None,
        }),
    };
    visited?;
    st.exit_block()?;
    st.analyze()?;
    let inlined_comps = st
        .stes
        .iter()
        .filter(|entry| entry.comp_inlined)
        .map(|entry| {
            (
                (entry.lineno, entry.col_offset),
                InlinedComp {
                    coroutine: entry.coroutine,
                    symbols: entry
                        .symbols
                        .iter()
                        .filter(|&(_, flags)| flags & DEF_PARAM == 0)
                        .collect(),
                },
            )
        })
        .collect();
    Ok(st.finalize(st.top, inlined_comps))
}

impl Builder<'_> {
    fn cur(&self) -> usize {
        *self.stack.last().expect("symbol table stack")
    }

    fn is_function_like(&self, ste: usize) -> bool {
        matches!(
            self.stes[ste].kind,
            BlockKind::Function
                | BlockKind::TypeVarBound
                | BlockKind::TypeAlias
                | BlockKind::TypeParam
        )
    }

    fn ste_loc(&self, ste: usize) -> Loc {
        let ste = &self.stes[ste];
        Loc {
            lineno: ste.lineno,
            col_offset: ste.col_offset,
            end_lineno: ste.end_lineno,
            end_col_offset: ste.end_col_offset,
        }
    }

    fn name_text(&self, name: Sym) -> String {
        self.interner
            .resolve(name)
            .iter()
            .map(|&cp| char::from_u32(cp).unwrap_or('\u{FFFD}'))
            .collect()
    }

    fn runtime_error(message: &str) -> SymtableError {
        SymtableError {
            kind: ErrorKind::Runtime,
            message: message.to_string(),
            lineno: None,
            offset: None,
            end_lineno: None,
            end_offset: None,
        }
    }

    /// A SyntaxError ranged over `loc`, with CPython's 1-based offsets.
    fn ranged_error(&self, message: String, loc: Loc) -> SymtableError {
        let (lineno, offset, end_lineno, end_offset) = loc.ranged();
        SymtableError {
            kind: ErrorKind::Syntax,
            message,
            lineno,
            offset,
            end_lineno,
            end_offset,
        }
    }

    /// Re-aims an already-built error at the directive's recorded location.
    fn directive_error(&self, ste: usize, name: Sym, message: String) -> SymtableError {
        for (mangled, loc) in &self.stes[ste].directives {
            if *mangled == name {
                return self.ranged_error(message, *loc);
            }
        }
        Self::runtime_error("BUG: internal directive bookkeeping broken")
    }

    // --- name mangling ----------------------------------------------------

    fn mangle(&mut self, private: Option<Sym>, name: Sym) -> Sym {
        let Some(private) = private else {
            return name;
        };
        let name_cp = self.interner.resolve(name).to_vec();
        if !(name_cp.first() == Some(&UNDERSCORE) && name_cp.get(1) == Some(&UNDERSCORE)) {
            return name;
        }
        // Don't mangle __dunder__ names or dotted import names.
        let nlen = name_cp.len();
        if (name_cp[nlen - 1] == UNDERSCORE && name_cp[nlen - 2] == UNDERSCORE)
            || name_cp.contains(&DOT)
        {
            return name;
        }
        let private_cp = self.interner.resolve(private).to_vec();
        let mut ipriv = 0;
        while private_cp.get(ipriv) == Some(&UNDERSCORE) {
            ipriv += 1;
        }
        if ipriv == private_cp.len() {
            return name;
        }
        let mut mangled = Vec::with_capacity(1 + private_cp.len() - ipriv + nlen);
        mangled.push(UNDERSCORE);
        mangled.extend_from_slice(&private_cp[ipriv..]);
        mangled.extend_from_slice(&name_cp);
        self.interner.intern_code_points(mangled)
    }

    fn maybe_mangle(&mut self, private: Option<Sym>, ste: usize, name: Sym) -> Sym {
        let in_set = self.stes[ste]
            .mangled_names
            .as_ref()
            .is_none_or(|set| set.contains(&name));
        if !in_set {
            return name;
        }
        self.mangle(private, name)
    }

    // --- scopes -----------------------------------------------------------

    fn enter_block(&mut self, name: Sym, kind: BlockKind, loc: Loc) -> Result<(), SymtableError> {
        let nested = match self.stack.last() {
            Some(&prev) => self.stes[prev].nested || self.is_function_like(prev),
            None => false,
        };
        let ste = Ste {
            name,
            symbols: SymMap::default(),
            children: Vec::new(),
            kind,
            nested,
            free: false,
            child_free: false,
            generator: false,
            coroutine: false,
            comprehension: ComprehensionKind::None,
            comp_inlined: false,
            comp_iter_target: false,
            can_see_class_scope: false,
            comp_iter_expr: 0,
            comp_bound: Vec::new(),
            lineno: loc.lineno,
            col_offset: loc.col_offset,
            end_lineno: loc.end_lineno,
            end_col_offset: loc.end_col_offset,
            directives: Vec::new(),
            mangled_names: None,
        };
        let index = self.stes.len();
        self.stes.push(ste);
        self.stack.push(index);
        if self.stack.len() >= 2 {
            let prev = self.stack[self.stack.len() - 2];
            // bpo-37757: the guard propagates through nested scopes.
            self.stes[index].comp_iter_expr = self.stes[prev].comp_iter_expr;
            // No need to inherit mangled names in classes, where all names
            // are mangled anyway.
            if self.stes[prev].mangled_names.is_some() && kind != BlockKind::Class {
                self.stes[index].mangled_names = self.stes[prev].mangled_names.clone();
            }
        }
        // Annotation blocks have no effect on the symbol table: they are not
        // linked into their parent's children, so analysis never sees them.
        if kind == BlockKind::Annotation {
            return Ok(());
        }
        if self.stack.len() >= 2 {
            let prev = self.stack[self.stack.len() - 2];
            self.stes[prev].children.push(index);
        }
        Ok(())
    }

    fn exit_block(&mut self) -> Result<(), SymtableError> {
        self.stack.pop();
        Ok(())
    }

    fn enter_type_param_block(
        &mut self,
        name: Sym,
        has_defaults: bool,
        has_kwdefaults: bool,
        def_kind: DefKind,
        loc: Loc,
    ) -> Result<(), SymtableError> {
        let enclosing_kind = self.stes[self.cur()].kind;
        self.enter_block(name, BlockKind::TypeParam, loc)?;
        if enclosing_kind == BlockKind::Class {
            let ste = self.cur();
            self.stes[ste].can_see_class_scope = true;
            self.add_def(self.names.classdict, USE, loc)?;
        }
        if def_kind == DefKind::ClassDef {
            // ".type_params" is set when the type params tuple is built and
            // used while the bases are built up; ".generic_base" sets the
            // generic base.
            self.add_def(self.names.dot_type_params, DEF_LOCAL, loc)?;
            self.add_def(self.names.dot_type_params, USE, loc)?;
            self.add_def(self.names.dot_generic_base, DEF_LOCAL, loc)?;
            self.add_def(self.names.dot_generic_base, USE, loc)?;
        }
        if has_defaults {
            self.add_def(self.names.dot_defaults, DEF_PARAM, loc)?;
        }
        if has_kwdefaults {
            self.add_def(self.names.dot_kwdefaults, DEF_PARAM, loc)?;
        }
        Ok(())
    }

    // --- definitions and lookups ------------------------------------------

    fn lookup(&mut self, name: Sym) -> Result<i64, SymtableError> {
        let ste = self.cur();
        self.lookup_entry(ste, name)
    }

    fn lookup_entry(&mut self, ste: usize, name: Sym) -> Result<i64, SymtableError> {
        let private = self.private;
        let mangled = self.maybe_mangle(private, ste, name);
        Ok(self.stes[ste].symbols.get(mangled).unwrap_or(0))
    }

    fn record_directive(&mut self, name: Sym, loc: Loc) -> Result<(), SymtableError> {
        let private = self.private;
        let ste = self.cur();
        let mangled = self.maybe_mangle(private, ste, name);
        self.stes[ste].directives.push((mangled, loc));
        Ok(())
    }

    fn add_def(&mut self, name: Sym, flag: i64, loc: Loc) -> Result<(), SymtableError> {
        if flag & DEF_TYPE_PARAM != 0 {
            let ste = self.cur();
            if let Some(set) = &mut self.stes[ste].mangled_names {
                set.insert(name);
            }
        }
        let ste = self.cur();
        self.add_def_helper(name, flag, ste, loc)
    }

    fn add_def_helper(
        &mut self,
        name: Sym,
        flag: i64,
        ste: usize,
        loc: Loc,
    ) -> Result<(), SymtableError> {
        let private = self.private;
        let cur = self.cur();
        let mangled = self.maybe_mangle(private, cur, name);
        let mut val;
        match self.stes[ste].symbols.get(mangled) {
            Some(existing) => {
                val = existing;
                if flag & DEF_PARAM != 0 && val & DEF_PARAM != 0 {
                    let message = format!(
                        "duplicate argument '{}' in function definition",
                        self.name_text(name)
                    );
                    return Err(self.ranged_error(message, loc));
                }
                if flag & DEF_TYPE_PARAM != 0 && val & DEF_TYPE_PARAM != 0 {
                    let message = format!("duplicate type parameter '{}'", self.name_text(name));
                    return Err(self.ranged_error(message, loc));
                }
                val |= flag;
            }
            None => val = flag,
        }
        if self.stes[ste].comp_iter_target {
            // This name is an iteration variable in a comprehension, so
            // check for a binding conflict with any named expressions.
            if val & (DEF_GLOBAL | DEF_NONLOCAL) != 0 {
                let message = format!(
                    "comprehension inner loop cannot rebind assignment expression target '{}'",
                    self.name_text(name)
                );
                return Err(self.ranged_error(message, loc));
            }
            val |= DEF_COMP_ITER;
        }
        self.stes[ste].symbols.insert(mangled, val);
        if flag & DEF_PARAM == 0 && flag & DEF_GLOBAL != 0 {
            let merged = match self.stes[self.top].symbols.get(mangled) {
                Some(existing) => existing | flag,
                None => flag,
            };
            self.stes[self.top].symbols.insert(mangled, merged);
        }
        Ok(())
    }

    // --- first pass: the visitors ------------------------------------------

    fn visit_stmt(&mut self, s: &Stmt) -> Result<(), SymtableError> {
        match s {
            Stmt::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.add_def(*name, DEF_LOCAL, loc)?;
                for default in &args.defaults {
                    self.visit_expr(default)?;
                }
                for default in args.kw_defaults.iter().flatten() {
                    self.visit_expr(default)?;
                }
                for decorator in decorator_list {
                    self.visit_expr(decorator)?;
                }
                if !type_params.is_empty() {
                    self.enter_type_param_block(
                        *name,
                        // `args->defaults != NULL` in CPython: the sequence is
                        // always allocated by the parser.
                        true,
                        args.kw_defaults.iter().any(Option::is_some),
                        DefKind::FunctionDef,
                        loc,
                    )?;
                    for type_param in type_params {
                        self.visit_type_param(type_param)?;
                    }
                }
                self.visit_annotations(loc, args, returns)?;
                self.enter_block(*name, BlockKind::Function, loc)?;
                self.visit_arguments(args)?;
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                self.exit_block()?;
                if !type_params.is_empty() {
                    self.exit_block()?;
                }
            }
            Stmt::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.add_def(*name, DEF_LOCAL, loc)?;
                for default in &args.defaults {
                    self.visit_expr(default)?;
                }
                for default in args.kw_defaults.iter().flatten() {
                    self.visit_expr(default)?;
                }
                for decorator in decorator_list {
                    self.visit_expr(decorator)?;
                }
                if !type_params.is_empty() {
                    self.enter_type_param_block(
                        *name,
                        // `args->defaults != NULL` in CPython: the sequence is
                        // always allocated by the parser.
                        true,
                        args.kw_defaults.iter().any(Option::is_some),
                        DefKind::AsyncFunctionDef,
                        loc,
                    )?;
                    for type_param in type_params {
                        self.visit_type_param(type_param)?;
                    }
                }
                self.visit_annotations(loc, args, returns)?;
                self.enter_block(*name, BlockKind::Function, loc)?;
                let ste = self.cur();
                self.stes[ste].coroutine = true;
                self.visit_arguments(args)?;
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                self.exit_block()?;
                if !type_params.is_empty() {
                    self.exit_block()?;
                }
            }
            Stmt::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.add_def(*name, DEF_LOCAL, loc)?;
                for decorator in decorator_list {
                    self.visit_expr(decorator)?;
                }
                let outer_private = self.private;
                if !type_params.is_empty() {
                    self.enter_type_param_block(*name, false, false, DefKind::ClassDef, loc)?;
                    self.private = Some(*name);
                    let ste = self.cur();
                    self.stes[ste].mangled_names = Some(HashSet::new());
                    for type_param in type_params {
                        self.visit_type_param(type_param)?;
                    }
                }
                for base in bases {
                    self.visit_expr(base)?;
                }
                for keyword in keywords {
                    self.visit_keyword(keyword)?;
                }
                self.enter_block(*name, BlockKind::Class, loc)?;
                self.private = Some(*name);
                if !type_params.is_empty() {
                    self.add_def(self.names.type_params, DEF_LOCAL, loc)?;
                    self.add_def(self.names.dot_type_params, USE, loc)?;
                }
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                self.exit_block()?;
                if !type_params.is_empty() {
                    self.exit_block()?;
                }
                self.private = outer_private;
            }
            Stmt::TypeAlias {
                name,
                type_params,
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.visit_expr(name)?;
                let alias_name = match &**name {
                    Expr::Name { id, .. } => *id,
                    _ => unreachable!("type alias target is a Name"),
                };
                let is_in_class = self.stes[self.cur()].kind == BlockKind::Class;
                let is_generic = !type_params.is_empty();
                if is_generic {
                    self.enter_type_param_block(alias_name, false, false, DefKind::TypeAlias, loc)?;
                    for type_param in type_params {
                        self.visit_type_param(type_param)?;
                    }
                }
                self.enter_block(alias_name, BlockKind::TypeAlias, loc)?;
                let ste = self.cur();
                self.stes[ste].can_see_class_scope = is_in_class;
                if is_in_class {
                    let value_loc = expr_loc(value);
                    self.add_def(self.names.classdict, USE, value_loc)?;
                }
                self.visit_expr(value)?;
                self.exit_block()?;
                if is_generic {
                    self.exit_block()?;
                }
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.visit_expr(value)?;
                }
            }
            Stmt::Delete { targets, .. } => {
                for target in targets {
                    self.visit_expr(target)?;
                }
            }
            Stmt::Assign { targets, value, .. } => {
                for target in targets {
                    self.visit_expr(target)?;
                }
                self.visit_expr(value)?;
            }
            Stmt::AugAssign { target, value, .. } => {
                self.visit_expr(target)?;
                self.visit_expr(value)?;
            }
            Stmt::AnnAssign {
                target,
                annotation,
                value,
                simple,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                let target = &**target;
                if let Expr::Name { id, .. } = target {
                    let target_loc = expr_loc(target);
                    let cur = self.lookup(*id)?;
                    if cur & (DEF_GLOBAL | DEF_NONLOCAL) != 0
                        && self.cur() != self.top
                        && *simple != 0
                    {
                        let message = if cur & DEF_GLOBAL != 0 {
                            format!("annotated name '{}' can't be global", self.name_text(*id))
                        } else {
                            format!("annotated name '{}' can't be nonlocal", self.name_text(*id))
                        };
                        return Err(self.ranged_error(message, loc));
                    }
                    if *simple != 0 {
                        self.add_def(*id, DEF_ANNOT | DEF_LOCAL, target_loc)?;
                    } else if value.is_some() {
                        self.add_def(*id, DEF_LOCAL, target_loc)?;
                    }
                } else {
                    self.visit_expr(target)?;
                }
                self.visit_annotation(annotation)?;
                if let Some(value) = value {
                    self.visit_expr(value)?;
                }
            }
            Stmt::For {
                target,
                iter,
                body,
                orelse,
                ..
            } => {
                self.visit_expr(target)?;
                self.visit_expr(iter)?;
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                for stmt in orelse {
                    self.visit_stmt(stmt)?;
                }
            }
            Stmt::AsyncFor {
                target,
                iter,
                body,
                orelse,
                ..
            } => {
                self.visit_expr(target)?;
                self.visit_expr(iter)?;
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                for stmt in orelse {
                    self.visit_stmt(stmt)?;
                }
            }
            Stmt::While {
                test, body, orelse, ..
            } => {
                self.visit_expr(test)?;
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                for stmt in orelse {
                    self.visit_stmt(stmt)?;
                }
            }
            Stmt::If {
                test, body, orelse, ..
            } => {
                self.visit_expr(test)?;
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                for stmt in orelse {
                    self.visit_stmt(stmt)?;
                }
            }
            Stmt::Match { subject, cases, .. } => {
                self.visit_expr(subject)?;
                for case in cases {
                    self.visit_match_case(case)?;
                }
            }
            Stmt::Raise { exc, cause, .. } => {
                if let Some(exc) = exc {
                    self.visit_expr(exc)?;
                    if let Some(cause) = cause {
                        self.visit_expr(cause)?;
                    }
                }
            }
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
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
                for stmt in orelse {
                    self.visit_stmt(stmt)?;
                }
                for handler in handlers {
                    self.visit_excepthandler(handler)?;
                }
                for stmt in finalbody {
                    self.visit_stmt(stmt)?;
                }
            }
            Stmt::Assert { test, msg, .. } => {
                self.visit_expr(test)?;
                if let Some(msg) = msg {
                    self.visit_expr(msg)?;
                }
            }
            Stmt::Import { names, .. } => {
                for alias in names {
                    self.visit_alias(alias)?;
                }
            }
            Stmt::ImportFrom { names, .. } => {
                for alias in names {
                    self.visit_alias(alias)?;
                }
            }
            Stmt::Global {
                names,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                for name in names {
                    let cur = self.lookup(*name)?;
                    if cur & (DEF_PARAM | DEF_LOCAL | USE | DEF_ANNOT) != 0 {
                        let message = if cur & DEF_PARAM != 0 {
                            format!("name '{}' is parameter and global", self.name_text(*name))
                        } else if cur & USE != 0 {
                            format!(
                                "name '{}' is used prior to global declaration",
                                self.name_text(*name)
                            )
                        } else if cur & DEF_ANNOT != 0 {
                            format!("annotated name '{}' can't be global", self.name_text(*name))
                        } else {
                            format!(
                                "name '{}' is assigned to before global declaration",
                                self.name_text(*name)
                            )
                        };
                        return Err(self.ranged_error(message, loc));
                    }
                    self.add_def(*name, DEF_GLOBAL, loc)?;
                    self.record_directive(*name, loc)?;
                }
            }
            Stmt::Nonlocal {
                names,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                for name in names {
                    let cur = self.lookup(*name)?;
                    if cur & (DEF_PARAM | DEF_LOCAL | USE | DEF_ANNOT) != 0 {
                        let message = if cur & DEF_PARAM != 0 {
                            format!("name '{}' is parameter and nonlocal", self.name_text(*name))
                        } else if cur & USE != 0 {
                            format!(
                                "name '{}' is used prior to nonlocal declaration",
                                self.name_text(*name)
                            )
                        } else if cur & DEF_ANNOT != 0 {
                            format!(
                                "annotated name '{}' can't be nonlocal",
                                self.name_text(*name)
                            )
                        } else {
                            format!(
                                "name '{}' is assigned to before nonlocal declaration",
                                self.name_text(*name)
                            )
                        };
                        return Err(self.ranged_error(message, loc));
                    }
                    self.add_def(*name, DEF_NONLOCAL, loc)?;
                    self.record_directive(*name, loc)?;
                }
            }
            Stmt::Expr { value, .. } => self.visit_expr(value)?,
            Stmt::Pass { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
            Stmt::With { items, body, .. } | Stmt::AsyncWith { items, body, .. } => {
                for item in items {
                    self.visit_withitem(item)?;
                }
                for stmt in body {
                    self.visit_stmt(stmt)?;
                }
            }
        }
        Ok(())
    }

    fn visit_expr(&mut self, e: &Expr) -> Result<(), SymtableError> {
        match e {
            Expr::NamedExpr {
                target,
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.raise_if_annotation_block("named expression", loc)?;
                self.handle_namedexpr(target, value, loc)?;
            }
            Expr::BoolOp { values, .. } => {
                for value in values {
                    self.visit_expr(value)?;
                }
            }
            Expr::BinOp { left, right, .. } => {
                self.visit_expr(left)?;
                self.visit_expr(right)?;
            }
            Expr::UnaryOp { operand, .. } => self.visit_expr(operand)?,
            Expr::Lambda {
                args,
                body,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                if self.stes[self.cur()].can_see_class_scope {
                    // gh-109118
                    return Err(self.ranged_error(
                        "Cannot use lambda in annotation scope within class scope".to_string(),
                        loc,
                    ));
                }
                for default in &args.defaults {
                    self.visit_expr(default)?;
                }
                for default in args.kw_defaults.iter().flatten() {
                    self.visit_expr(default)?;
                }
                self.enter_block(self.names.lambda, BlockKind::Function, loc)?;
                self.visit_arguments(args)?;
                self.visit_expr(body)?;
                self.exit_block()?;
            }
            Expr::IfExp {
                test, body, orelse, ..
            } => {
                self.visit_expr(test)?;
                self.visit_expr(body)?;
                self.visit_expr(orelse)?;
            }
            Expr::Dict { keys, values, .. } => {
                for key in keys.iter().flatten() {
                    self.visit_expr(key)?;
                }
                for value in values {
                    self.visit_expr(value)?;
                }
            }
            Expr::Set { elts, .. } => {
                for elt in elts {
                    self.visit_expr(elt)?;
                }
            }
            Expr::ListComp {
                elt, generators, ..
            } => {
                let loc = expr_loc(e);
                self.handle_comprehension(
                    loc,
                    self.names.listcomp,
                    ComprehensionKind::List,
                    generators,
                    elt,
                    None,
                )?;
            }
            Expr::SetComp {
                elt, generators, ..
            } => {
                let loc = expr_loc(e);
                self.handle_comprehension(
                    loc,
                    self.names.setcomp,
                    ComprehensionKind::Set,
                    generators,
                    elt,
                    None,
                )?;
            }
            Expr::DictComp {
                key,
                value,
                generators,
                ..
            } => {
                let loc = expr_loc(e);
                self.handle_comprehension(
                    loc,
                    self.names.dictcomp,
                    ComprehensionKind::Dict,
                    generators,
                    key,
                    Some(value),
                )?;
            }
            Expr::GeneratorExp {
                elt, generators, ..
            } => {
                let loc = expr_loc(e);
                self.handle_comprehension(
                    loc,
                    self.names.genexpr,
                    ComprehensionKind::Generator,
                    generators,
                    elt,
                    None,
                )?;
            }
            Expr::Await {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.raise_if_annotation_block("await expression", loc)?;
                self.visit_expr(value)?;
                let ste = self.cur();
                self.stes[ste].coroutine = true;
            }
            Expr::Yield {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.raise_if_annotation_block("yield expression", loc)?;
                if let Some(value) = value {
                    self.visit_expr(value)?;
                }
                let ste = self.cur();
                self.stes[ste].generator = true;
                if self.stes[ste].comprehension != ComprehensionKind::None {
                    return Err(self.raise_if_comprehension_block(loc));
                }
            }
            Expr::YieldFrom {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.raise_if_annotation_block("yield expression", loc)?;
                self.visit_expr(value)?;
                let ste = self.cur();
                self.stes[ste].generator = true;
                if self.stes[ste].comprehension != ComprehensionKind::None {
                    return Err(self.raise_if_comprehension_block(loc));
                }
            }
            Expr::Compare {
                left, comparators, ..
            } => {
                self.visit_expr(left)?;
                for comparator in comparators {
                    self.visit_expr(comparator)?;
                }
            }
            Expr::Call {
                func,
                args,
                keywords,
                ..
            } => {
                self.visit_expr(func)?;
                for arg in args {
                    self.visit_expr(arg)?;
                }
                for keyword in keywords {
                    self.visit_keyword(keyword)?;
                }
            }
            Expr::FormattedValue {
                value, format_spec, ..
            } => {
                self.visit_expr(value)?;
                if let Some(format_spec) = format_spec {
                    self.visit_expr(format_spec)?;
                }
            }
            Expr::JoinedStr { values, .. } => {
                for value in values {
                    self.visit_expr(value)?;
                }
            }
            Expr::Constant { .. } => {}
            Expr::Attribute { value, .. } => self.visit_expr(value)?,
            Expr::Subscript { value, slice, .. } => {
                self.visit_expr(value)?;
                self.visit_expr(slice)?;
            }
            Expr::Starred { value, .. } => self.visit_expr(value)?,
            Expr::Slice {
                lower, upper, step, ..
            } => {
                if let Some(lower) = lower {
                    self.visit_expr(lower)?;
                }
                if let Some(upper) = upper {
                    self.visit_expr(upper)?;
                }
                if let Some(step) = step {
                    self.visit_expr(step)?;
                }
            }
            Expr::Name {
                id,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => {
                let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
                let flag = if matches!(**ctx, ExprContext::Load) {
                    USE
                } else {
                    DEF_LOCAL
                };
                self.add_def(*id, flag, loc)?;
                // Special-case super: it counts as a use of __class__.
                if matches!(**ctx, ExprContext::Load)
                    && self.is_function_like(self.cur())
                    && *id == self.names.super_
                {
                    self.add_def(self.names.class, USE, loc)?;
                }
            }
            Expr::List { elts, .. } => {
                for elt in elts {
                    self.visit_expr(elt)?;
                }
            }
            Expr::Tuple { elts, .. } => {
                for elt in elts {
                    self.visit_expr(elt)?;
                }
            }
        }
        Ok(())
    }

    fn visit_keyword(&mut self, k: &Keyword) -> Result<(), SymtableError> {
        self.visit_expr(&k.value)
    }

    fn visit_withitem(&mut self, item: &Withitem) -> Result<(), SymtableError> {
        self.visit_expr(&item.context_expr)?;
        if let Some(optional_vars) = &item.optional_vars {
            self.visit_expr(optional_vars)?;
        }
        Ok(())
    }

    fn visit_excepthandler(&mut self, eh: &Excepthandler) -> Result<(), SymtableError> {
        let Excepthandler::ExceptHandler {
            r#type,
            name,
            body,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
        } = eh;
        if let Some(exc_type) = r#type {
            self.visit_expr(exc_type)?;
        }
        if let Some(name) = name {
            let loc = Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset);
            self.add_def(*name, DEF_LOCAL, loc)?;
        }
        for stmt in body {
            self.visit_stmt(stmt)?;
        }
        Ok(())
    }

    fn visit_match_case(&mut self, m: &MatchCase) -> Result<(), SymtableError> {
        self.visit_pattern(&m.pattern)?;
        if let Some(guard) = &m.guard {
            self.visit_expr(guard)?;
        }
        for stmt in &m.body {
            self.visit_stmt(stmt)?;
        }
        Ok(())
    }

    fn visit_pattern(&mut self, p: &Pattern) -> Result<(), SymtableError> {
        match p {
            Pattern::MatchValue { value, .. } => self.visit_expr(value)?,
            Pattern::MatchSingleton { .. } => {}
            Pattern::MatchSequence { patterns, .. } => {
                for pattern in patterns {
                    self.visit_pattern(pattern)?;
                }
            }
            Pattern::MatchStar {
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                if let Some(name) = name {
                    let loc = Loc::new(
                        *lineno,
                        *col_offset,
                        Some(*end_lineno),
                        Some(*end_col_offset),
                    );
                    self.add_def(*name, DEF_LOCAL, loc)?;
                }
            }
            Pattern::MatchMapping {
                keys,
                patterns,
                rest,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                for key in keys {
                    self.visit_expr(key)?;
                }
                for pattern in patterns {
                    self.visit_pattern(pattern)?;
                }
                if let Some(rest) = rest {
                    let loc = Loc::new(
                        *lineno,
                        *col_offset,
                        Some(*end_lineno),
                        Some(*end_col_offset),
                    );
                    self.add_def(*rest, DEF_LOCAL, loc)?;
                }
            }
            Pattern::MatchClass {
                cls,
                patterns,
                kwd_patterns,
                ..
            } => {
                self.visit_expr(cls)?;
                for pattern in patterns {
                    self.visit_pattern(pattern)?;
                }
                for pattern in kwd_patterns {
                    self.visit_pattern(pattern)?;
                }
            }
            Pattern::MatchAs {
                pattern,
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                if let Some(pattern) = pattern {
                    self.visit_pattern(pattern)?;
                }
                if let Some(name) = name {
                    let loc = Loc::new(
                        *lineno,
                        *col_offset,
                        Some(*end_lineno),
                        Some(*end_col_offset),
                    );
                    self.add_def(*name, DEF_LOCAL, loc)?;
                }
            }
            Pattern::MatchOr { patterns, .. } => {
                for pattern in patterns {
                    self.visit_pattern(pattern)?;
                }
            }
        }
        Ok(())
    }

    fn visit_type_param(&mut self, tp: &TypeParam) -> Result<(), SymtableError> {
        let (name, lineno, col_offset, end_lineno, end_col_offset, bound) = match tp {
            TypeParam::TypeVar {
                name,
                bound,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => (
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                bound.as_deref(),
            ),
            TypeParam::ParamSpec {
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => (name, lineno, col_offset, end_lineno, end_col_offset, None),
            TypeParam::TypeVarTuple {
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => (name, lineno, col_offset, end_lineno, end_col_offset, None),
        };
        let loc = Loc::new(
            *lineno,
            *col_offset,
            Some(*end_lineno),
            Some(*end_col_offset),
        );
        if *name == self.names.classdict {
            return Err(self.ranged_error(
                "reserved name '__classdict__' cannot be used for type parameter".to_string(),
                loc,
            ));
        }
        self.add_def(*name, DEF_TYPE_PARAM | DEF_LOCAL, loc)?;
        if let Some(bound) = bound {
            let ste = self.cur();
            let is_in_class = self.stes[ste].can_see_class_scope;
            self.enter_block(*name, BlockKind::TypeVarBound, loc)?;
            let ste = self.cur();
            self.stes[ste].can_see_class_scope = is_in_class;
            if is_in_class {
                let bound_loc = expr_loc(bound);
                self.add_def(self.names.classdict, USE, bound_loc)?;
            }
            self.visit_expr(bound)?;
            self.exit_block()?;
        }
        Ok(())
    }

    fn visit_arguments(&mut self, a: &Arguments) -> Result<(), SymtableError> {
        // Default arguments are skipped inside the function block.
        self.visit_params(&a.posonlyargs)?;
        self.visit_params(&a.args)?;
        self.visit_params(&a.kwonlyargs)?;
        if let Some(vararg) = &a.vararg {
            self.add_def(vararg.arg, DEF_PARAM, arg_loc(vararg))?;
        }
        if let Some(kwarg) = &a.kwarg {
            self.add_def(kwarg.arg, DEF_PARAM, arg_loc(kwarg))?;
        }
        Ok(())
    }

    fn visit_params(&mut self, args: &[Arg]) -> Result<(), SymtableError> {
        for arg in args {
            self.add_def(arg.arg, DEF_PARAM, arg_loc(arg))?;
        }
        Ok(())
    }

    fn visit_annotations(
        &mut self,
        def_loc: Loc,
        a: &Arguments,
        returns: &Option<Box<Expr>>,
    ) -> Result<(), SymtableError> {
        let future_annotations = self.future_annotations;
        if future_annotations {
            self.enter_block(self.names.annotation, BlockKind::Annotation, def_loc)?;
        }
        for arg in a.posonlyargs.iter().chain(&a.args) {
            if let Some(annotation) = &arg.annotation {
                self.visit_expr(annotation)?;
            }
        }
        if let Some(vararg) = &a.vararg
            && let Some(annotation) = &vararg.annotation
        {
            self.visit_expr(annotation)?;
        }
        if let Some(kwarg) = &a.kwarg
            && let Some(annotation) = &kwarg.annotation
        {
            self.visit_expr(annotation)?;
        }
        for arg in &a.kwonlyargs {
            if let Some(annotation) = &arg.annotation {
                self.visit_expr(annotation)?;
            }
        }
        if future_annotations {
            self.exit_block()?;
        }
        if let Some(returns) = returns {
            self.visit_annotation(returns)?;
        }
        Ok(())
    }

    fn visit_annotation(&mut self, annotation: &Expr) -> Result<(), SymtableError> {
        let future_annotations = self.future_annotations;
        if future_annotations {
            let loc = expr_loc(annotation);
            self.enter_block(self.names.annotation, BlockKind::Annotation, loc)?;
        }
        self.visit_expr(annotation)?;
        if future_annotations {
            self.exit_block()?;
        }
        Ok(())
    }

    fn visit_alias(&mut self, a: &Alias) -> Result<(), SymtableError> {
        // The name actually bound is the asname, or the top package of a
        // dotted name.
        let name = a.asname.unwrap_or(a.name);
        let store_name = match self.interner.resolve(name).iter().position(|&cp| cp == DOT) {
            Some(dot) => {
                let prefix = self.interner.resolve(name)[..dot].to_vec();
                self.interner.intern_code_points(prefix)
            }
            None => name,
        };
        if name == self.names.star {
            if self.stes[self.cur()].kind != BlockKind::Module {
                let loc = Loc::new(a.lineno, a.col_offset, a.end_lineno, a.end_col_offset);
                return Err(
                    self.ranged_error("import * only allowed at module level".to_string(), loc)
                );
            }
            return Ok(());
        }
        let loc = Loc::new(a.lineno, a.col_offset, a.end_lineno, a.end_col_offset);
        self.add_def(store_name, DEF_IMPORT, loc)
    }

    // --- named expressions -------------------------------------------------

    fn handle_namedexpr(
        &mut self,
        target: &Expr,
        value: &Expr,
        loc: Loc,
    ) -> Result<(), SymtableError> {
        if self.stes[self.cur()].comp_iter_expr > 0 {
            // Assignment isn't allowed in a comprehension iterable expression.
            return Err(self.ranged_error(
                "assignment expression cannot be used in a comprehension iterable expression"
                    .to_string(),
                loc,
            ));
        }
        if self.stes[self.cur()].comprehension != ComprehensionKind::None {
            // Inside a comprehension body, so find the right target scope.
            self.extend_namedexpr_scope(target)?;
        }
        self.visit_expr(value)?;
        self.visit_expr(target)
    }

    fn extend_namedexpr_scope(&mut self, target: &Expr) -> Result<(), SymtableError> {
        let target_name = match target {
            Expr::Name { id, .. } => *id,
            _ => unreachable!("named expression target is a Name"),
        };
        // CPython passes the target Name here, so every error and directive
        // ranges over the target, not the whole NamedExpr.
        let loc = expr_loc(target);
        // Find the nearest adequate scope, walking the stack in reverse.
        for index in (0..self.stack.len()).rev() {
            let ste = self.stack[index];
            if self.stes[ste].comprehension != ComprehensionKind::None {
                let target_in_scope = self.lookup_entry(ste, target_name)?;
                if target_in_scope & DEF_COMP_ITER != 0 && target_in_scope & DEF_LOCAL != 0 {
                    let message = format!(
                        "assignment expression cannot rebind comprehension iteration variable '{}'",
                        self.name_text(target_name)
                    );
                    return Err(self.ranged_error(message, loc));
                }
                continue;
            }
            match self.stes[ste].kind {
                BlockKind::Function => {
                    let target_in_scope = self.lookup_entry(ste, target_name)?;
                    if target_in_scope & DEF_GLOBAL != 0 {
                        self.add_def(target_name, DEF_GLOBAL, loc)?;
                    } else {
                        self.add_def(target_name, DEF_NONLOCAL, loc)?;
                    }
                    self.record_directive(target_name, loc)?;
                    return self.add_def_helper(target_name, DEF_LOCAL, ste, loc);
                }
                BlockKind::Module => {
                    self.add_def(target_name, DEF_GLOBAL, loc)?;
                    self.record_directive(target_name, loc)?;
                    return self.add_def_helper(target_name, DEF_GLOBAL, ste, loc);
                }
                // Disallowed usage in class and type scopes.
                BlockKind::Class => {
                    return Err(self.ranged_error(
                        "assignment expression within a comprehension cannot be used in a class body".to_string(),
                        loc,
                    ));
                }
                BlockKind::TypeParam => {
                    return Err(self.ranged_error(
                        "assignment expression within a comprehension cannot be used within the definition of a generic".to_string(),
                        loc,
                    ));
                }
                BlockKind::TypeAlias => {
                    return Err(self.ranged_error(
                        "assignment expression within a comprehension cannot be used in a type alias".to_string(),
                        loc,
                    ));
                }
                BlockKind::TypeVarBound => {
                    return Err(self.ranged_error(
                        "assignment expression within a comprehension cannot be used in a TypeVar bound".to_string(),
                        loc,
                    ));
                }
                BlockKind::Annotation => {}
            }
        }
        unreachable!("named expression outside any module or function scope")
    }

    // --- comprehensions -----------------------------------------------------

    fn visit_comprehension(&mut self, lc: &Comprehension) -> Result<(), SymtableError> {
        let ste = self.cur();
        self.stes[ste].comp_iter_target = true;
        let visited = self.visit_expr(&lc.target);
        self.stes[ste].comp_iter_target = false;
        visited?;
        self.stes[ste].comp_iter_expr += 1;
        let visited = self.visit_expr(&lc.iter);
        self.stes[ste].comp_iter_expr -= 1;
        visited?;
        for guard in &lc.ifs {
            self.visit_expr(guard)?;
        }
        if lc.is_async != 0 {
            self.stes[ste].coroutine = true;
        }
        Ok(())
    }

    fn handle_comprehension(
        &mut self,
        loc: Loc,
        scope_name: Sym,
        kind: ComprehensionKind,
        generators: &[Comprehension],
        elt: &Expr,
        value: Option<&Expr>,
    ) -> Result<(), SymtableError> {
        if self.stes[self.cur()].can_see_class_scope {
            // gh-109118
            return Err(self.ranged_error(
                "Cannot use comprehension in annotation scope within class scope".to_string(),
                loc,
            ));
        }
        let is_generator = kind == ComprehensionKind::Generator;
        let outermost = generators.first().expect("comprehension generator");
        // The outermost iterator is evaluated in the current scope.
        let ste = self.cur();
        self.stes[ste].comp_iter_expr += 1;
        let visited = self.visit_expr(&outermost.iter);
        self.stes[ste].comp_iter_expr -= 1;
        visited?;
        // The rest goes into a comprehension scope of its own.
        self.enter_block(scope_name, BlockKind::Function, loc)?;
        let ste = self.cur();
        self.stes[ste].comprehension = kind;
        if outermost.is_async != 0 {
            self.stes[ste].coroutine = true;
        }
        // The outermost iter is received as an argument.
        let ste_loc = self.ste_loc(ste);
        self.add_def(self.names.dot0, DEF_PARAM, ste_loc)?;
        self.stes[ste].comp_iter_target = true;
        let visited = self.visit_expr(&outermost.target);
        self.stes[ste].comp_iter_target = false;
        visited?;
        for guard in &outermost.ifs {
            self.visit_expr(guard)?;
        }
        for generator in &generators[1..] {
            self.visit_comprehension(generator)?;
        }
        if let Some(value) = value {
            self.visit_expr(value)?;
        }
        self.visit_expr(elt)?;
        self.stes[ste].generator = is_generator;
        let is_async = self.stes[ste].coroutine && !is_generator;
        self.exit_block()?;
        if is_async {
            let ste = self.cur();
            self.stes[ste].coroutine = true;
        }
        Ok(())
    }

    fn raise_if_annotation_block(&mut self, name: &str, loc: Loc) -> Result<(), SymtableError> {
        let message = match self.stes[self.cur()].kind {
            BlockKind::Annotation => format!("{name} cannot be used within an annotation"),
            BlockKind::TypeVarBound => format!("{name} cannot be used within a TypeVar bound"),
            BlockKind::TypeAlias => format!("{name} cannot be used within a type alias"),
            BlockKind::TypeParam => {
                format!("{name} cannot be used within the definition of a generic")
            }
            _ => return Ok(()),
        };
        Err(self.ranged_error(message, loc))
    }

    fn raise_if_comprehension_block(&self, loc: Loc) -> SymtableError {
        let message = match self.stes[self.cur()].comprehension {
            ComprehensionKind::List => "'yield' inside list comprehension",
            ComprehensionKind::Set => "'yield' inside set comprehension",
            ComprehensionKind::Dict => "'yield' inside dict comprehension",
            _ => "'yield' inside generator expression",
        };
        self.ranged_error(message.to_string(), loc)
    }

    // --- second pass: analysis ----------------------------------------------

    fn analyze(&mut self) -> Result<(), SymtableError> {
        let mut free = HashSet::new();
        let mut global = HashSet::new();
        let mut type_params = HashSet::new();
        self.analyze_block(
            self.top,
            None,
            &mut free,
            &mut global,
            &mut type_params,
            None,
        )
    }

    fn analyze_child_block(
        &mut self,
        entry: usize,
        bound: Option<&HashSet<Sym>>,
        free: &HashSet<Sym>,
        global: &HashSet<Sym>,
        type_params: &HashSet<Sym>,
        class_entry: Option<usize>,
    ) -> Result<HashSet<Sym>, SymtableError> {
        let mut temp_bound = bound.cloned().unwrap_or_default();
        let mut temp_free = free.clone();
        let mut temp_global = global.clone();
        let mut temp_type_params = type_params.clone();
        self.analyze_block(
            entry,
            Some(&mut temp_bound),
            &mut temp_free,
            &mut temp_global,
            &mut temp_type_params,
            class_entry,
        )?;
        Ok(temp_free)
    }

    fn analyze_block(
        &mut self,
        ste: usize,
        mut bound: Option<&mut HashSet<Sym>>,
        free: &mut HashSet<Sym>,
        global: &mut HashSet<Sym>,
        type_params: &mut HashSet<Sym>,
        class_entry: Option<usize>,
    ) -> Result<(), SymtableError> {
        let mut local: HashSet<Sym> = HashSet::new();
        let mut scopes = SymMap::default();
        let mut newglobal: HashSet<Sym> = HashSet::new();
        let mut newfree: HashSet<Sym> = HashSet::new();
        let mut newbound: HashSet<Sym> = HashSet::new();
        let mut inlined_cells: HashSet<Sym> = HashSet::new();

        let is_class = self.stes[ste].kind == BlockKind::Class;

        // The class namespace has no effect on names visible in nested
        // functions, so populate the global and bound sets for children
        // before analyzing this block's own names.
        if is_class {
            newglobal.extend(global.iter().copied());
            if let Some(bound) = bound.as_deref() {
                newbound.extend(bound.iter().copied());
            }
        }

        let mut state = AnalysisState {
            scopes: &mut scopes,
            bound: bound.as_deref_mut(),
            local: &mut local,
            free,
            global,
            type_params,
            class_entry,
        };
        let symbols: Vec<(Sym, i64)> = self.stes[ste].symbols.iter().collect();
        for (name, flags) in symbols {
            self.analyze_name(ste, &mut state, name, flags)?;
        }

        if !is_class {
            if self.is_function_like(ste) {
                newbound.extend(local.iter().copied());
            }
            if let Some(bound) = bound.as_deref() {
                newbound.extend(bound.iter().copied());
            }
            newglobal.extend(global.iter().copied());
        } else {
            // Special-case __class__ and __classdict__.
            newbound.insert(self.names.class);
            newbound.insert(self.names.classdict);
        }

        let children: Vec<usize> = self.stes[ste].children.clone();
        for child in children {
            let new_class_entry = if self.stes[child].can_see_class_scope {
                if is_class { Some(ste) } else { class_entry }
            } else {
                None
            };
            // All non-generator-expression comprehensions are inlined.
            let inline_comp = self.stes[child].comprehension != ComprehensionKind::None
                && !self.stes[child].generator;
            let mut child_free = self.analyze_child_block(
                child,
                Some(&newbound),
                &newfree,
                &newglobal,
                type_params,
                new_class_entry,
            )?;
            if inline_comp {
                self.inline_comprehension(
                    ste,
                    child,
                    &mut scopes,
                    &mut child_free,
                    &mut inlined_cells,
                )?;
                self.stes[child].comp_inlined = true;
            }
            newfree.extend(child_free.iter().copied());
            if self.stes[child].free || self.stes[child].child_free {
                self.stes[ste].child_free = true;
            }
        }

        // Splice the children of inlined comprehensions into our children.
        let mut index = self.stes[ste].children.len();
        while index > 0 {
            index -= 1;
            let child = self.stes[ste].children[index];
            if self.stes[child].comp_inlined {
                let grandchildren = std::mem::take(&mut self.stes[child].children);
                self.stes[ste].children.splice(index..=index, grandchildren);
            }
        }

        // Check if any local variables must be converted to cell variables.
        if self.is_function_like(ste) {
            analyze_cells(&mut scopes, &mut newfree, &inlined_cells);
        } else if is_class {
            // __class__ and __classdict__ never escape a class scope as
            // free variables; needs_class_closure/needs_classdict, which
            // CPython sets here, are compiler outputs this table doesn't
            // report.
            newfree.remove(&self.names.class);
            newfree.remove(&self.names.classdict);
        }
        let classflag = is_class || self.stes[ste].can_see_class_scope;
        let symbols = &mut self.stes[ste].symbols;
        update_symbols(
            symbols,
            &scopes,
            bound.as_deref(),
            &newfree,
            &inlined_cells,
            classflag,
        );
        free.extend(newfree.iter().copied());
        Ok(())
    }

    fn analyze_name(
        &mut self,
        ste: usize,
        state: &mut AnalysisState<'_>,
        name: Sym,
        flags: i64,
    ) -> Result<(), SymtableError> {
        if flags & DEF_GLOBAL != 0 {
            if flags & DEF_NONLOCAL != 0 {
                let message = format!("name '{}' is nonlocal and global", self.name_text(name));
                return Err(self.directive_error(ste, name, message));
            }
            state.scopes.insert(name, GLOBAL_EXPLICIT);
            state.global.insert(name);
            if let Some(bound) = state.bound.as_deref_mut() {
                bound.remove(&name);
            }
            return Ok(());
        }
        if flags & DEF_NONLOCAL != 0 {
            let Some(bound) = state.bound.as_deref() else {
                return Err(self.directive_error(
                    ste,
                    name,
                    "nonlocal declaration not allowed at module level".to_string(),
                ));
            };
            if !bound.contains(&name) {
                let message = format!("no binding for nonlocal '{}' found", self.name_text(name));
                return Err(self.directive_error(ste, name, message));
            }
            if state.type_params.contains(&name) {
                let message = format!(
                    "nonlocal binding not allowed for type parameter '{}'",
                    self.name_text(name)
                );
                return Err(self.directive_error(ste, name, message));
            }
            state.scopes.insert(name, FREE);
            self.stes[ste].free = true;
            state.free.insert(name);
            return Ok(());
        }
        if flags & DEF_BOUND != 0 {
            state.scopes.insert(name, LOCAL);
            state.local.insert(name);
            state.global.remove(&name);
            if flags & DEF_TYPE_PARAM != 0 {
                state.type_params.insert(name);
            } else {
                state.type_params.remove(&name);
            }
            return Ok(());
        }
        // In a scope that can see a class scope, a name bound (or declared
        // global) by the class namespace resolves against the class and
        // globals alone.
        if let Some(class_entry) = state.class_entry {
            let class_flags = self.stes[class_entry].symbols.get(name).unwrap_or(0);
            if class_flags & DEF_GLOBAL != 0 {
                state.scopes.insert(name, GLOBAL_EXPLICIT);
                return Ok(());
            } else if class_flags & DEF_BOUND != 0 && class_flags & DEF_NONLOCAL == 0 {
                state.scopes.insert(name, GLOBAL_IMPLICIT);
                return Ok(());
            }
        }
        // If an enclosing block has a binding for this name, it is a free
        // variable rather than a global variable.
        if let Some(bound) = state.bound.as_deref()
            && bound.contains(&name)
        {
            state.scopes.insert(name, FREE);
            self.stes[ste].free = true;
            state.free.insert(name);
            return Ok(());
        }
        // If a parent has a global statement, then call it global implicit.
        if state.global.contains(&name) {
            state.scopes.insert(name, GLOBAL_IMPLICIT);
            return Ok(());
        }
        if self.stes[ste].nested {
            self.stes[ste].free = true;
        }
        state.scopes.insert(name, GLOBAL_IMPLICIT);
        Ok(())
    }

    fn is_free_in_any_child(&self, ste: usize, name: Sym) -> bool {
        self.stes[ste].children.iter().any(|&child| {
            let flags = self.stes[child].symbols.get(name).unwrap_or(0);
            (flags >> SCOPE_OFFSET) & SCOPE_MASK == FREE
        })
    }

    fn inline_comprehension(
        &mut self,
        ste: usize,
        comp: usize,
        scopes: &mut SymMap,
        comp_free: &mut HashSet<Sym>,
        inlined_cells: &mut HashSet<Sym>,
    ) -> Result<(), SymtableError> {
        let mut remove_dunder_class = false;
        let entries: Vec<(Sym, i64)> = self.stes[comp].symbols.iter().collect();
        for (name, comp_flags) in entries {
            // Skip the comprehension parameter.
            if comp_flags & DEF_PARAM != 0 {
                continue;
            }
            let mut scope = (comp_flags >> SCOPE_OFFSET) & SCOPE_MASK;
            let only_flags = comp_flags & ((1 << SCOPE_OFFSET) - 1);
            if scope == CELL || only_flags & DEF_COMP_CELL != 0 {
                inlined_cells.insert(name);
            }
            // A local without DEF_NONLOCAL is a comprehension `for` target:
            // its outer binding is saved while the comprehension runs.
            if only_flags & DEF_LOCAL != 0 && only_flags & DEF_NONLOCAL == 0 {
                let bound_names = &mut self.stes[ste].comp_bound;
                if !bound_names.contains(&name) {
                    bound_names.push(name);
                }
            }
            let existing = self.stes[ste].symbols.get(name);
            // __class__ is never allowed to be free through a class scope
            // (see drop_class_free).
            if scope == FREE && self.stes[ste].kind == BlockKind::Class && name == self.names.class
            {
                scope = GLOBAL_IMPLICIT;
                comp_free.remove(&name);
                remove_dunder_class = true;
            }
            match existing {
                None => {
                    self.stes[ste].symbols.insert(name, only_flags);
                    scopes.insert(name, scope);
                }
                Some(existing_flags) => {
                    // Free vars in the comprehension that are locals in the
                    // outer scope can now simply be locals, unless they are
                    // free in the comprehension's children or the outer
                    // scope is a class block.
                    if existing_flags & DEF_BOUND != 0
                        && !self.is_free_in_any_child(comp, name)
                        && self.stes[ste].kind != BlockKind::Class
                    {
                        comp_free.remove(&name);
                    }
                }
            }
        }
        self.stes[comp].free = !comp_free.is_empty();
        if remove_dunder_class {
            self.stes[comp].symbols.remove(self.names.class);
        }
        Ok(())
    }

    fn finalize(&self, ste: usize, inlined_comps: Vec<((i32, i32), InlinedComp)>) -> SymbolTable {
        let entry = &self.stes[ste];
        SymbolTable {
            name: entry.name,
            kind: entry.kind,
            lineno: entry.lineno,
            is_nested: entry.nested,
            generator: entry.generator,
            coroutine: entry.coroutine,
            symbols: entry.symbols.iter().collect(),
            comp_bound: entry.comp_bound.clone(),
            children: entry
                .children
                .iter()
                .map(|&child| self.finalize(child, Vec::new()))
                .collect(),
            inlined_comps,
        }
    }
}

/// The name-resolution state a block's analysis threads through
/// `analyze_name` — CPython's `analyze_name` parameters beyond the block and
/// the name itself.
struct AnalysisState<'a> {
    scopes: &'a mut SymMap,
    bound: Option<&'a mut HashSet<Sym>>,
    local: &'a mut HashSet<Sym>,
    free: &'a mut HashSet<Sym>,
    global: &'a mut HashSet<Sym>,
    type_params: &'a mut HashSet<Sym>,
    class_entry: Option<usize>,
}

/// Converts LOCAL scopes that provide bindings for free (or inlined-cell)
/// names into CELL scopes.
fn analyze_cells(scopes: &mut SymMap, free: &mut HashSet<Sym>, inlined_cells: &HashSet<Sym>) {
    for (name, scope) in scopes.entries_mut() {
        if *scope != LOCAL {
            continue;
        }
        if free.contains(name) || inlined_cells.contains(name) {
            *scope = CELL;
            free.remove(name);
        }
    }
}

/// Records the final scopes in the flag words, then propagates unresolved
/// free variables up the lexical stack.
fn update_symbols(
    symbols: &mut SymMap,
    scopes: &SymMap,
    bound: Option<&HashSet<Sym>>,
    free: &HashSet<Sym>,
    inlined_cells: &HashSet<Sym>,
    classflag: bool,
) {
    for (name, mut flags) in symbols.iter().collect::<Vec<_>>() {
        if inlined_cells.contains(&name) {
            flags |= DEF_COMP_CELL;
        }
        let scope = scopes.get(name).expect("every analyzed symbol has a scope");
        flags |= scope << SCOPE_OFFSET;
        symbols.insert(name, flags);
    }
    for name in free {
        match symbols.get(*name) {
            // A free variable in a method of the class that has the same
            // name as a local or global in the class scope.
            Some(existing) => {
                if classflag {
                    symbols.insert(*name, existing | DEF_FREE_CLASS);
                }
            }
            None => {
                if let Some(bound) = bound
                    && !bound.contains(name)
                {
                    continue; // it's a global
                }
                symbols.insert(*name, FREE << SCOPE_OFFSET);
            }
        }
    }
}

fn arg_loc(arg: &Arg) -> Loc {
    Loc::new(
        arg.lineno,
        arg.col_offset,
        arg.end_lineno,
        arg.end_col_offset,
    )
}

/// Every `Expr` variant carries the four span fields directly.
fn expr_loc(e: &Expr) -> Loc {
    match e {
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
        } => Loc::new(*lineno, *col_offset, *end_lineno, *end_col_offset),
    }
}

/// The future features the symbol table consumes: `_PyFuture_FromAST` for
/// `CO_FUTURE_ANNOTATIONS`, including its errors for undefined features.
fn future_annotations(module: &Mod, interner: &mut Interner) -> Result<bool, SymtableError> {
    /// The features 3.12 accepts; everything unknown but these is an error.
    const KNOWN: [&str; 10] = [
        "nested_scopes",
        "generators",
        "division",
        "absolute_import",
        "with_statement",
        "print_function",
        "unicode_literals",
        "barry_as_FLUFL",
        "generator_stop",
        "annotations",
    ];

    fn sym_eq(interner: &Interner, sym: Sym, text: &str) -> bool {
        interner
            .resolve(sym)
            .iter()
            .copied()
            .eq(text.chars().map(u32::from))
    }

    // %.100s truncates at 100 bytes, never mid-character in practice.
    fn truncate100(text: &str) -> &str {
        if text.len() <= 100 {
            return text;
        }
        let mut end = 100;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    let body = match module {
        Mod::Module { body, .. } | Mod::Interactive { body } => body,
        _ => return Ok(false),
    };
    // Only a docstring and other future statements may precede one.
    let docstring = matches!(
        body.first(),
        Some(Stmt::Expr { value, .. })
            if matches!(**value, Expr::Constant { value: Constant::Str(_), .. })
    );
    let mut annotations = false;
    for stmt in &body[usize::from(docstring)..] {
        let Stmt::ImportFrom {
            module: Some(whence),
            names,
            ..
        } = stmt
        else {
            break;
        };
        if !sym_eq(interner, *whence, "__future__") {
            break;
        }
        for alias in names {
            let feature = interner
                .resolve(alias.name)
                .iter()
                .map(|&cp| char::from_u32(cp).unwrap_or('\u{FFFD}'))
                .collect::<String>();
            if feature == "annotations" {
                annotations = true;
            } else if feature == "braces" {
                let loc = Loc::new(
                    alias.lineno,
                    alias.col_offset,
                    alias.end_lineno,
                    alias.end_col_offset,
                );
                let (_, offset, end_lineno, end_offset) = loc.ranged();
                return Err(SymtableError {
                    kind: ErrorKind::Syntax,
                    message: "not a chance".to_string(),
                    lineno: Some(loc.lineno),
                    offset,
                    end_lineno,
                    end_offset,
                });
            } else if !KNOWN.contains(&feature.as_str()) {
                let loc = Loc::new(
                    alias.lineno,
                    alias.col_offset,
                    alias.end_lineno,
                    alias.end_col_offset,
                );
                let (_, offset, end_lineno, end_offset) = loc.ranged();
                return Err(SymtableError {
                    kind: ErrorKind::Syntax,
                    message: format!("future feature {} is not defined", truncate100(&feature)),
                    lineno: Some(loc.lineno),
                    offset,
                    end_lineno,
                    end_offset,
                });
            }
        }
    }
    Ok(annotations)
}

impl SymMap {
    /// A mutable view over the entries, in insertion order.
    fn entries_mut(&mut self) -> impl Iterator<Item = (&mut Sym, &mut i64)> {
        self.entries
            .iter_mut()
            .map(|entry| (&mut entry.0, &mut entry.1))
    }
}

#[cfg(test)]
mod tests {
    use crate::dump::dump_symtable;
    use crate::parser::parse_module;

    fn build(source: &str) -> String {
        let (module, mut interner) = parse_module(source.as_bytes(), None).unwrap();
        let table = super::symtable(&module, &mut interner).unwrap();
        dump_symtable(&table, &interner)
    }

    fn build_error(source: &str) -> super::SymtableError {
        let (module, mut interner) = parse_module(source.as_bytes(), None).unwrap();
        match super::symtable(&module, &mut interner) {
            Err(error) => error,
            Ok(_) => panic!("expected a symbol-table error"),
        }
    }

    #[test]
    fn closure_marks_free_variable_and_enclosing_cell() {
        let source =
            "def outer(x):\n    y = x + 1\n    def inner():\n        return y\n    return inner\n";
        assert_eq!(
            build(source),
            r#"{"name":"top","type":"module","lineno":0,"is_optimized":false,"is_nested":false,"symbols":{"outer":["is_assigned","is_global","is_local","is_namespace"]},"children":[{"name":"outer","type":"function","lineno":1,"is_optimized":true,"is_nested":false,"symbols":{"inner":["is_assigned","is_local","is_namespace","is_referenced"],"x":["is_local","is_parameter","is_referenced"],"y":["is_assigned","is_local"]},"children":[{"name":"inner","type":"function","lineno":3,"is_optimized":true,"is_nested":true,"symbols":{"y":["is_free","is_referenced"]},"children":[]}]}]}"#
        );
    }

    #[test]
    fn class_mangles_private_names_and_sees_class_cell() {
        let source =
            "class C:\n    __v = 3\n    def m(self):\n        return super().__init__, __v\n";
        assert_eq!(
            build(source),
            r#"{"name":"top","type":"module","lineno":0,"is_optimized":false,"is_nested":false,"symbols":{"C":["is_assigned","is_global","is_local","is_namespace"]},"children":[{"name":"C","type":"class","lineno":1,"is_optimized":false,"is_nested":false,"symbols":{"_C__v":["is_assigned","is_local"],"m":["is_assigned","is_local","is_namespace"]},"children":[{"name":"m","type":"function","lineno":3,"is_optimized":true,"is_nested":false,"symbols":{"_C__v":["is_global","is_referenced"],"__class__":["is_free","is_referenced"],"self":["is_local","is_parameter"],"super":["is_global","is_referenced"]},"children":[]}]}]}"#
        );
    }

    #[test]
    fn list_comprehension_is_inlined_and_genexp_keeps_scope() {
        let source =
            "lst = [i + j for i in range(3) for j in range(i) if i]\ngen = (k for k in lst)\n";
        assert_eq!(
            build(source),
            r#"{"name":"top","type":"module","lineno":0,"is_optimized":false,"is_nested":false,"symbols":{"gen":["is_assigned","is_global","is_local"],"i":["is_assigned","is_global","is_local","is_referenced"],"j":["is_assigned","is_global","is_local","is_referenced"],"lst":["is_assigned","is_global","is_local","is_referenced"],"range":["is_global","is_referenced"]},"children":[{"name":"genexpr","type":"function","lineno":2,"is_optimized":true,"is_nested":false,"symbols":{".0":["is_local","is_parameter"],"k":["is_assigned","is_local","is_referenced"]},"children":[]}]}"#
        );
    }

    #[test]
    fn global_and_nonlocal_resolution_errors() {
        let error = build_error(
            "a = 1\ndef f():\n    global a\n    a = 2\n    global b\n    b = 3\ndef g():\n    nonlocal missing\n",
        );
        assert_eq!(error.kind.name(), "SyntaxError");
        assert_eq!(error.message, "no binding for nonlocal 'missing' found");
        assert_eq!(error.lineno, Some(8));
        assert_eq!(error.offset, Some(5));
        assert_eq!(error.end_lineno, Some(8));
        assert_eq!(error.end_offset, Some(21));

        let error = build_error("def f():\n    print(a)\n    global a\n");
        assert_eq!(
            error.message,
            "name 'a' is used prior to global declaration"
        );
        assert_eq!(error.lineno, Some(3));
        assert_eq!(error.offset, Some(5));
    }

    #[test]
    fn future_annotations_hide_annotation_scope() {
        let source = "\"\"\"doc\"\"\"\nfrom __future__ import annotations\n\ndef f(x: int) -> \"D\": return x\nclass D: pass\n";
        assert_eq!(
            build(source),
            r#"{"name":"top","type":"module","lineno":0,"is_optimized":false,"is_nested":false,"symbols":{"D":["is_assigned","is_global","is_local","is_namespace"],"annotations":["is_global","is_imported","is_local"],"f":["is_assigned","is_global","is_local","is_namespace"]},"children":[{"name":"f","type":"function","lineno":4,"is_optimized":true,"is_nested":false,"symbols":{"x":["is_local","is_parameter","is_referenced"]},"children":[]},{"name":"D","type":"class","lineno":5,"is_optimized":false,"is_nested":false,"symbols":{},"children":[]}]}"#
        );
    }

    #[test]
    fn generic_def_gets_type_parameter_scope() {
        let source = "def f[T: int, *Ts, U](x: T) -> U:\n    v: U = x\n    return v\n";
        assert_eq!(
            build(source),
            r#"{"name":"top","type":"module","lineno":0,"is_optimized":false,"is_nested":false,"symbols":{"f":["is_assigned","is_global","is_local","is_namespace"]},"children":[{"name":"f","type":"type parameter","lineno":1,"is_optimized":false,"is_nested":false,"symbols":{".defaults":["is_local","is_parameter"],"T":["is_assigned","is_local","is_namespace","is_referenced"],"Ts":["is_assigned","is_local"],"U":["is_assigned","is_local","is_referenced"]},"children":[{"name":"T","type":"TypeVar bound","lineno":1,"is_optimized":false,"is_nested":true,"symbols":{"int":["is_global","is_referenced"]},"children":[]},{"name":"f","type":"function","lineno":1,"is_optimized":true,"is_nested":true,"symbols":{"U":["is_free","is_referenced"],"v":["is_annotated","is_assigned","is_local","is_referenced"],"x":["is_local","is_parameter","is_referenced"]},"children":[]}]}]}"#
        );
    }

    #[test]
    fn named_expression_errors_range_the_target() {
        let error = build_error("x = [(j := 1) for j in range(3) if (j := 2)]\n");
        assert_eq!(
            error.message,
            "assignment expression cannot rebind comprehension iteration variable 'j'"
        );
        assert_eq!(error.lineno, Some(1));
        assert_eq!(error.offset, Some(37));
        assert_eq!(error.end_lineno, Some(1));
        assert_eq!(error.end_offset, Some(38));

        let error = build_error("class x:\n    [(e := f) for f in rng]\n");
        assert_eq!(
            error.message,
            "assignment expression within a comprehension cannot be used in a class body"
        );
        assert_eq!(error.lineno, Some(2));
        assert_eq!(error.offset, Some(7));
        assert_eq!(error.end_lineno, Some(2));
        assert_eq!(error.end_offset, Some(8));
    }

    #[test]
    fn annotation_visit_order_reports_vararg_and_kwarg_first() {
        let error = build_error(
            "from __future__ import annotations\ndef f(*args: await x, y: await z): pass\n",
        );
        assert_eq!(
            error.message,
            "await expression cannot be used within an annotation"
        );
        assert_eq!(error.lineno, Some(2));
        assert_eq!(error.offset, Some(14));
        assert_eq!(error.end_lineno, Some(2));
        assert_eq!(error.end_offset, Some(21));

        let error = build_error(
            "from __future__ import annotations\ndef f(*, y: await z, **kw: await w): pass\n",
        );
        assert_eq!(error.offset, Some(28));
        assert_eq!(error.end_offset, Some(35));
    }

    #[test]
    fn star_import_is_module_only() {
        let error = build_error("from os import *\ndef f():\n    from sys import *\n");
        assert_eq!(error.message, "import * only allowed at module level");
        assert_eq!(error.lineno, Some(3));
        assert_eq!(error.offset, Some(21));
        assert_eq!(error.end_lineno, Some(3));
        assert_eq!(error.end_offset, Some(22));
    }
}

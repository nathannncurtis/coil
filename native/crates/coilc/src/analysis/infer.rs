//! Whole-program, flow-sensitive type inference with per-function
//! specialisation on argument-type tuples (spec section 7): Agesen's
//! Cartesian Product Algorithm with bounded cloning over the lattice of
//! spec section 5. One clone per distinct argument-type tuple up to
//! [`CLONE_LIMIT`], then one `Top` version that everything further routes
//! to, so every reachable function has a version whose analysis covers the
//! inputs nobody pinned down. The fixpoint walks the call graph's components
//! in their callees-first order and re-runs a function whenever a quantity
//! it read grows: argument tuples, callee returns, attribute types per
//! class, module-global types and closure-cell types only grow, so the
//! rounds converge.
//!
//! The sources are exactly section 7's list: literals, `isinstance` and
//! `type(x) is T` guards narrowing on the true edge, default values,
//! plain-class-name annotations (a starting point; where one narrows a
//! `Top` parameter a guard is recorded for `specialize` to emit, never a
//! semantic change), return types of the specialised callees, attribute
//! types per shape, container-literal element types and the hand-written
//! signatures. `Int64` means "an int whose value provably fits in 64 bits":
//! an operation that may overflow types its result `BigInt`, and the
//! overflow branch of an `*Ovf` instruction narrows its `into` value to
//! `BigInt`. What inference cannot pin down stays `Top` for the boxed path.
//!
//! The analysis runs on its own module-qualified view of the lattice
//! ([`ATy`]): the IR's `Type` indexes class tables per module, so a type
//! crossing a function boundary (a routed argument tuple, an attribute type)
//! carries its [`ClassRef`] along and narrows back to the module-local
//! `Type` only when written into that module's `Function::types`. A class of
//! another module has no point in the target module's lattice and reads as
//! `Top` there.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use coil_ir::ir::{BinOp, Const, ConstPool, Instr, Module, Terminator, UnaryOp};
use coil_ir::types::{BlockId, ConstIdx, Sym, Type, Value};

use crate::analysis::callgraph::{self, CallGraph, Hierarchy};
use crate::analysis::shapes::Shapes;
use crate::analysis::signatures::{self, Ty as SigTy};
use crate::build::relative_base;
use crate::world::reach::TopDef;
use crate::world::{ClassRef, FnRef, Program};

/// Distinct argument-type tuples a function specialises on before falling
/// back to one `Top` version (spec section 7).
const CLONE_LIMIT: usize = 8;

/// Union members past this many collapse to `Top`: a wider union boxes
/// anyway, so the precision is not worth the lattice height.
const UNION_LIMIT: usize = 4;

/// The analysis's module-qualified type. Mirrors the IR lattice with
/// cross-module references carried explicitly, and one extra point: a value
/// that is one known function ([`FnRef`]), which call routing uses and the
/// IR expresses as `Top`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum ATy {
    Top,
    Union(Box<[ATy]>),
    Inst {
        cls: ClassRef,
        exact: bool,
    },
    Int64,
    Float64,
    Bool,
    None,
    BigInt,
    Str,
    Bytes,
    Tuple(Box<[ATy]>),
    List,
    Dict,
    Set,
    Func(FnRef),
    /// A program module by index into `Program::modules`.
    Module(u32),
    Class(ClassRef),
    /// An extension-defined type by name (spec section 9); its own regime.
    Ext(String),
    Bottom,
}

impl ATy {
    /// The module-local IR type. `Inst` and `Class` of another module have
    /// no point in `module`'s lattice and read as `Top`; function objects
    /// and extension names have no IR point yet.
    pub fn to_type(&self, module: u32) -> Type {
        match self {
            ATy::Top | ATy::Func(_) | ATy::Module(_) | ATy::Ext(_) => Type::Top,
            ATy::Union(parts) => Type::Union(parts.iter().map(|p| p.to_type(module)).collect()),
            ATy::Inst { cls, exact } => {
                if cls.module == module {
                    Type::Inst {
                        cls: cls.class,
                        exact: *exact,
                    }
                } else {
                    Type::Top
                }
            }
            ATy::Int64 => Type::Int64,
            ATy::Float64 => Type::Float64,
            ATy::Bool => Type::Bool,
            ATy::None => Type::None,
            ATy::BigInt => Type::BigInt,
            ATy::Str => Type::Str,
            ATy::Bytes => Type::Bytes,
            ATy::Tuple(parts) => Type::Tuple(parts.iter().map(|p| p.to_type(module)).collect()),
            ATy::List => Type::List,
            ATy::Dict => Type::Dict,
            ATy::Set => Type::Set,
            ATy::Class(cls) if cls.module == module => Type::Class(cls.class),
            ATy::Class(_) => Type::Top,
            ATy::Bottom => Type::Bottom,
        }
    }
}

/// The lattice's bottom element: an empty map entry awaiting its first
/// fact, the identity of `join`.
impl Default for ATy {
    fn default() -> Self {
        ATy::Bottom
    }
}

impl ATy {
    fn is_top(&self) -> bool {
        matches!(self, ATy::Top)
    }
}

/// `Bottom` means "no fact survived"; the honest materialised type of a
/// live value is `Top`, inside structure as well as at the top.
fn materialize(t: &ATy) -> ATy {
    match t {
        ATy::Bottom => ATy::Top,
        ATy::Union(parts) => ATy::Union(parts.iter().map(materialize).collect::<Vec<_>>().into()),
        ATy::Tuple(parts) => ATy::Tuple(parts.iter().map(materialize).collect::<Vec<_>>().into()),
        other => other.clone(),
    }
}

/// The IR type back into the analysis lattice, for the types the earlier
/// passes embed in instructions (`TypeGuard`, `Phi` incomings).
fn from_type(ty: &Type, module: u32, pool: &ConstPool) -> ATy {
    match ty {
        Type::Top => ATy::Top,
        Type::Union(parts) => {
            ATy::Union(parts.iter().map(|p| from_type(p, module, pool)).collect())
        }
        Type::Inst { cls, exact } => ATy::Inst {
            cls: ClassRef {
                module,
                class: *cls,
            },
            exact: *exact,
        },
        Type::Int64 => ATy::Int64,
        Type::Float64 => ATy::Float64,
        Type::Bool => ATy::Bool,
        Type::None => ATy::None,
        Type::BigInt => ATy::BigInt,
        Type::Str => ATy::Str,
        Type::Bytes => ATy::Bytes,
        Type::Tuple(parts) => {
            ATy::Tuple(parts.iter().map(|p| from_type(p, module, pool)).collect())
        }
        Type::List => ATy::List,
        Type::Dict => ATy::Dict,
        Type::Set => ATy::Set,
        Type::Func(_) | Type::Module(_) => ATy::Top,
        Type::Class(cls) => ATy::Class(ClassRef {
            module,
            class: *cls,
        }),
        Type::Ext(name) => ATy::Ext(pool.sym_str(name.0).to_owned()),
        Type::Bottom => ATy::Bottom,
    }
}

/// The least type covering both: `Bottom` is the identity, `Top` absorbs,
/// the int chain `Bool` < `Int64` < `BigInt` collapses upward (a widened
/// unboxed int reads as its boxed form), and anything else forms a
/// normalised union that collapses to `Top` past [`UNION_LIMIT`] members.
fn join(a: &ATy, b: &ATy) -> ATy {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (ATy::Top, _) | (_, ATy::Top) => ATy::Top,
        (ATy::Bottom, t) | (t, ATy::Bottom) => t.clone(),
        _ => {
            let mut members: Vec<ATy> = Vec::new();
            flatten_into(a, &mut members);
            flatten_into(b, &mut members);
            normalize_members(&mut members);
            if members.len() > UNION_LIMIT {
                ATy::Top
            } else if members.len() == 1 {
                members.pop().expect("one member")
            } else {
                ATy::Union(members.into())
            }
        }
    }
}

/// Join `t` into `slot`, returning whether it grew.
fn join_into(slot: &mut ATy, t: &ATy) -> bool {
    if subsumes(slot, t) {
        return false;
    }
    *slot = join(slot, t);
    true
}

/// Whether `sup` already covers `t`.
fn subsumes(sup: &ATy, t: &ATy) -> bool {
    *sup == join(sup, t)
}

fn flatten_into(t: &ATy, out: &mut Vec<ATy>) {
    match t {
        ATy::Union(parts) => {
            for p in parts {
                flatten_into(p, out);
            }
        }
        _ => out.push(t.clone()),
    }
}

/// Sort, deduplicate and collapse the subtypes the lattice chains: `Bool`
/// and `Int64` under `BigInt`, `Bool` under `Int64`, an exact instance under
/// the inexact one of the same class.
fn normalize_members(members: &mut Vec<ATy>) {
    members.retain(|m| !matches!(m, ATy::Bottom));
    members.sort();
    members.dedup();
    if members.contains(&ATy::BigInt) {
        members.retain(|m| !matches!(m, ATy::Bool | ATy::Int64));
    } else if members.contains(&ATy::Int64) {
        members.retain(|m| !matches!(m, ATy::Bool));
    }
    let inexact: Vec<ClassRef> = members
        .iter()
        .filter_map(|m| match m {
            ATy::Inst { cls, exact: false } => Some(*cls),
            _ => None,
        })
        .collect();
    if !inexact.is_empty() {
        members.retain(|m| !matches!(m, ATy::Inst { cls, exact: true } if inexact.contains(cls)));
    }
    members.dedup();
}

/// The greatest type inside both, for guard narrowing and annotation seeds.
/// A subclass stays inside its superclass's type; the int chain keeps its
/// smaller point.
fn meet(a: &ATy, b: &ATy, hierarchy: &Hierarchy) -> ATy {
    if a == b {
        return a.clone();
    }
    match (a, b) {
        (ATy::Top, t) | (t, ATy::Top) => t.clone(),
        (ATy::Bottom, _) | (_, ATy::Bottom) => ATy::Bottom,
        (ATy::Union(parts), t) | (t, ATy::Union(parts)) => {
            let mut out = ATy::Bottom;
            for p in parts {
                out = join(&out, &meet(p, t, hierarchy));
            }
            out
        }
        (ATy::Inst { cls: c1, exact: e1 }, ATy::Inst { cls: c2, exact: e2 }) => {
            if c1 == c2 {
                ATy::Inst {
                    cls: *c1,
                    exact: *e1 && *e2,
                }
            } else if mro_contains(hierarchy, *c1, *c2) {
                ATy::Inst {
                    cls: *c1,
                    exact: *e1,
                }
            } else if mro_contains(hierarchy, *c2, *c1) {
                ATy::Inst {
                    cls: *c2,
                    exact: *e2,
                }
            } else {
                ATy::Bottom
            }
        }
        (ATy::Int64, ATy::BigInt) | (ATy::BigInt, ATy::Int64) => ATy::Int64,
        (ATy::Bool, ATy::BigInt) | (ATy::BigInt, ATy::Bool) => ATy::Bool,
        (ATy::Bool, ATy::Int64) | (ATy::Int64, ATy::Bool) => ATy::Bool,
        (ATy::Tuple(p1), ATy::Tuple(p2)) if p1.len() == p2.len() => ATy::Tuple(
            p1.iter()
                .zip(p2.iter())
                .map(|(x, y)| meet(x, y, hierarchy))
                .collect(),
        ),
        _ => ATy::Bottom,
    }
}

/// Whether `sup` is in `sub`'s linearisation: `sub` is a subclass of `sup`.
fn mro_contains(hierarchy: &Hierarchy, sub: ClassRef, sup: ClassRef) -> bool {
    hierarchy.mro(sub).is_some_and(|mro| mro.contains(&sup))
}

/// The signature table's view of a type: what a hand-written entry can
/// match. Structured values and unknowns read as `Any`.
fn sig_of(t: &ATy) -> SigTy {
    match t {
        ATy::Int64 => SigTy::Int64,
        ATy::BigInt => SigTy::BigInt,
        ATy::Float64 => SigTy::Float64,
        ATy::Bool => SigTy::Bool,
        ATy::None => SigTy::None,
        ATy::Str => SigTy::Str,
        ATy::Bytes => SigTy::Bytes,
        ATy::List => SigTy::List,
        ATy::Dict => SigTy::Dict,
        ATy::Set => SigTy::Set,
        ATy::Tuple(_) => SigTy::Tuple,
        _ => SigTy::Any,
    }
}

/// A signature's type in the analysis lattice.
fn aty_of_sig(t: SigTy) -> ATy {
    match t {
        SigTy::Any | SigTy::Tuple => ATy::Top,
        SigTy::Bool => ATy::Bool,
        SigTy::None => ATy::None,
        SigTy::Int64 => ATy::Int64,
        SigTy::BigInt => ATy::BigInt,
        SigTy::Float64 => ATy::Float64,
        SigTy::Str => ATy::Str,
        SigTy::Bytes => ATy::Bytes,
        SigTy::List => ATy::List,
        SigTy::Dict => ATy::Dict,
        SigTy::Set => ATy::Set,
        SigTy::Bottom => ATy::Bottom,
    }
}

/// A signature lookup with the overflow rule applied: an `Int64` result the
/// operation may have to box reads as `BigInt`.
fn sig_result(key: &str, args: &[ATy]) -> Option<ATy> {
    let sig_args: Vec<SigTy> = args.iter().map(sig_of).collect();
    let call = signatures::call(key, &sig_args)?;
    let mut result = aty_of_sig(call.result);
    if call.may_overflow && result == ATy::Int64 {
        result = ATy::BigInt;
    }
    Some(result)
}

/// The builtin type names as guard and annotation types: `int` covers every
/// int point, so it reads as `BigInt` (the widest int).
fn builtin_class_ty(name: &str) -> Option<ATy> {
    match name {
        "int" => Some(ATy::BigInt),
        "bool" => Some(ATy::Bool),
        "float" => Some(ATy::Float64),
        "str" => Some(ATy::Str),
        "bytes" => Some(ATy::Bytes),
        "list" => Some(ATy::List),
        "dict" => Some(ATy::Dict),
        "set" | "frozenset" => Some(ATy::Set),
        _ => None,
    }
}

/// One clone of a function: what the specialisation pass materialises.
#[derive(Clone, Debug)]
pub struct CloneInfo {
    /// The argument-type tuple this clone was specialised on, one per
    /// parameter.
    pub args: Vec<ATy>,
    /// The fallback version every further distinct tuple routes to.
    pub top: bool,
    /// The join of the clone's return values.
    pub returns: ATy,
    /// The clones of the callers that route here: `(caller, clone index)`.
    pub callers: BTreeSet<(FnRef, usize)>,
}

/// A narrowing inference recorded for `specialize` to emit as a `TypeGuard`
/// with an explicit slow path (spec section 7): never a semantic change.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Guard {
    pub at: FnRef,
    pub clone: usize,
    /// The block where the narrowed type holds.
    pub block: u32,
    pub value: u32,
    pub ty: ATy,
    /// An entry seed from a plain-class-name annotation rather than a branch.
    pub entry: bool,
}

/// What inference found: the specialisation table, the guards to insert and
/// the attribute types per class (spec section 8.3's starting point).
#[derive(Clone, Debug, Default)]
pub struct Inference {
    pub table: BTreeMap<FnRef, Vec<CloneInfo>>,
    pub guards: Vec<Guard>,
    pub attrs: BTreeMap<(ClassRef, String), ATy>,
}

impl Inference {
    /// The text form the corpus tests assert on: one block per function
    /// (its clones, their callers), the guards, the attribute types.
    pub fn dump(&self, program: &Program) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (fr, clones) in &self.table {
            let _ = writeln!(out, "{}", fn_label(program, *fr));
            for (i, clone) in clones.iter().enumerate() {
                let tag = if clone.top { " top" } else { "" };
                let _ = writeln!(
                    out,
                    "  clone {i} args [{}] returns {}{tag}",
                    show_list(program, &clone.args),
                    show(program, &clone.returns)
                );
                for (caller, index) in &clone.callers {
                    let _ = writeln!(
                        out,
                        "    called by {} clone {index}",
                        fn_label(program, *caller)
                    );
                }
            }
        }
        if !self.guards.is_empty() {
            out.push_str("guards:\n");
            for guard in &self.guards {
                let kind = if guard.entry { "entry" } else { "branch" };
                let _ = writeln!(
                    out,
                    "  {} clone {} v{} -> {} ({kind})",
                    fn_label(program, guard.at),
                    guard.clone,
                    guard.value,
                    show(program, &guard.ty)
                );
            }
        }
        if !self.attrs.is_empty() {
            out.push_str("attrs:\n");
            for ((cr, name), t) in &self.attrs {
                let _ = writeln!(
                    out,
                    "  {}.{} = {}",
                    class_label(program, *cr),
                    name,
                    show(program, t)
                );
            }
        }
        out
    }
}

fn fn_label(program: &Program, fr: FnRef) -> String {
    let state = &program.modules[fr.module as usize];
    let ir = state.ir.as_ref().expect("reachable functions are compiled");
    let qualname = ir
        .consts
        .sym_str(ir.functions[fr.function.0 as usize].qualname);
    format!("{}.{}", state.name, qualname)
}

fn class_label(program: &Program, cr: ClassRef) -> String {
    let state = &program.modules[cr.module as usize];
    let ir = state
        .ir
        .as_ref()
        .expect("classes only exist in compiled modules");
    let name = ir.consts.sym_str(ir.classes[cr.class.0 as usize].name);
    format!("{}.{}", state.name, name)
}

fn show(program: &Program, t: &ATy) -> String {
    match t {
        ATy::Top => "Top".to_owned(),
        ATy::Union(parts) => {
            let inner: Vec<String> = parts.iter().map(|p| show(program, p)).collect();
            format!("Union[{}]", inner.join(", "))
        }
        ATy::Inst { cls, exact } => format!(
            "{}{}",
            class_label(program, *cls),
            if *exact { "!" } else { "" }
        ),
        ATy::Int64 => "Int64".to_owned(),
        ATy::Float64 => "Float64".to_owned(),
        ATy::Bool => "Bool".to_owned(),
        ATy::None => "None".to_owned(),
        ATy::BigInt => "BigInt".to_owned(),
        ATy::Str => "Str".to_owned(),
        ATy::Bytes => "Bytes".to_owned(),
        ATy::Tuple(parts) => {
            let inner: Vec<String> = parts.iter().map(|p| show(program, p)).collect();
            format!("Tuple[{}]", inner.join(", "))
        }
        ATy::List => "List".to_owned(),
        ATy::Dict => "Dict".to_owned(),
        ATy::Set => "Set".to_owned(),
        ATy::Func(fr) => format!("Func({})", fn_label(program, *fr)),
        ATy::Module(m) => program.modules[*m as usize].name.clone(),
        ATy::Class(cr) => format!("Class({})", class_label(program, *cr)),
        ATy::Ext(name) => format!("Ext({name})"),
        ATy::Bottom => "Bottom".to_owned(),
    }
}

fn show_list(program: &Program, parts: &[ATy]) -> String {
    let shown: Vec<String> = parts.iter().map(|p| show(program, p)).collect();
    shown.join(", ")
}

/// One function's clones and the tables its analysis reads and writes.
struct FnState {
    params: Vec<String>,
    n_pos: usize,
    vararg_idx: Option<usize>,
    kwonly_start: usize,
    kwarg_idx: Option<usize>,
    arity: usize,
    /// Constant default types, aligned to the last `defaults.len()`
    /// positional parameters.
    defaults: Vec<ATy>,
    /// Keyword-only parameters' constant default types.
    kwdefaults: BTreeMap<String, ATy>,
    /// Plain-class-name annotations by parameter name.
    annots: BTreeMap<String, String>,
    n_values: usize,
    entry: BlockId,
    rpo: Vec<BlockId>,
    /// Blocks no path from the entry reaches, in block order.
    tail: Vec<BlockId>,
    preds: Vec<Vec<BlockId>>,
    def_site: HashMap<Value, (BlockId, usize)>,
    clones: Vec<CloneRec>,
    dirty: BTreeSet<usize>,
}

struct CloneRec {
    args: Vec<ATy>,
    top: bool,
    returns: ATy,
    /// Per-value types of the last analysis; the join over clones is what
    /// lands in `Function::types`.
    types: Vec<ATy>,
    /// Narrowed types by `(block, value)`, the guard-consuming passes' map.
    narrow: BTreeMap<(u32, u32), ATy>,
    routers: BTreeSet<(FnRef, usize)>,
}

/// What one clone's analysis read and wrote outside itself.
#[derive(Default)]
struct FlowOut {
    base: Vec<ATy>,
    narrow: BTreeMap<(u32, u32), ATy>,
    returns: ATy,
    routes: Vec<(FnRef, Vec<ATy>)>,
    attr_writes: BTreeMap<(ClassRef, String), ATy>,
    attr_reads: BTreeSet<(ClassRef, String)>,
    global_writes: BTreeMap<(u32, String), ATy>,
    global_reads: BTreeSet<(u32, String)>,
    cell_writes: BTreeMap<(FnRef, u32), ATy>,
    cell_reads: BTreeSet<(FnRef, u32)>,
    guards: Vec<(u32, Value, ATy)>,
}

/// The whole-program fixpoint state.
struct Driver<'a> {
    program: &'a Program,
    shapes: &'a Shapes,
    order: Vec<Vec<FnRef>>,
    hierarchy: Hierarchy,
    mod_by_name: BTreeMap<String, u32>,
    fns: BTreeMap<FnRef, FnState>,
    attrs: BTreeMap<(ClassRef, String), ATy>,
    attr_readers: BTreeMap<(ClassRef, String), BTreeSet<(FnRef, usize)>>,
    globals: BTreeMap<(u32, String), ATy>,
    global_readers: BTreeMap<(u32, String), BTreeSet<(FnRef, usize)>>,
    cells: BTreeMap<(FnRef, u32), ATy>,
    cell_readers: BTreeMap<(FnRef, u32), BTreeSet<(FnRef, usize)>>,
    /// Callers to re-run when a function gains a clone or a return type
    /// grows.
    call_readers: BTreeMap<FnRef, BTreeSet<(FnRef, usize)>>,
    guards: BTreeSet<Guard>,
}

/// Run inference over the program, filling every `Function::types` with the
/// join over its clones and returning the specialisation table, the guards
/// and the attribute types for the later passes (spec sections 7 and 8.3).
pub fn analyze(program: &mut Program, shapes: &Shapes, graph: &CallGraph) -> Inference {
    let mut driver = Driver::new(program, shapes, graph);
    driver.seed();
    driver.solve();
    let (inference, writes) = driver.finish();
    for (fr, types) in writes {
        if let Some(ir) = program.modules[fr.module as usize].ir.as_mut()
            && let Some(f) = ir.functions.get_mut(fr.function.0 as usize)
        {
            f.types = types;
        }
    }
    inference
}

impl<'a> Driver<'a> {
    fn new(program: &'a Program, shapes: &'a Shapes, graph: &CallGraph) -> Driver<'a> {
        let mut fns = BTreeMap::new();
        for &fr in &program.functions {
            if let Some(state) = FnState::of(program, fr) {
                fns.insert(fr, state);
            }
        }
        let prescan = Prescan::run(program);
        for (fr, found) in prescan.found {
            if let Some(state) = fns.get_mut(&fr) {
                state.defaults = found.defaults;
                state.kwdefaults = found.kwdefaults;
                state.annots = found.annots;
            }
        }
        let mod_by_name = program
            .modules
            .iter()
            .enumerate()
            .map(|(i, state)| (state.name.clone(), i as u32))
            .collect();
        Driver {
            program,
            shapes,
            order: graph.sccs().to_vec(),
            hierarchy: callgraph::hierarchy(program),
            mod_by_name,
            fns,
            attrs: BTreeMap::new(),
            attr_readers: BTreeMap::new(),
            globals: BTreeMap::new(),
            global_readers: BTreeMap::new(),
            cells: BTreeMap::new(),
            cell_readers: BTreeMap::new(),
            call_readers: BTreeMap::new(),
            guards: BTreeSet::new(),
        }
    }

    /// Every module body starts as an exact empty-tuple clone: an import
    /// runs it once with no arguments.
    fn seed(&mut self) {
        let seeds: Vec<FnRef> = self
            .program
            .modules
            .iter()
            .enumerate()
            .filter_map(|(i, state)| {
                state.ir.as_ref().map(|ir| FnRef {
                    module: i as u32,
                    function: ir.body,
                })
            })
            .collect();
        for fr in seeds {
            let args = vec![ATy::Top; self.fns.get(&fr).map_or(0, |s| s.arity)];
            self.push_clone(fr, args, false, None);
        }
    }

    /// Rounds over the components in callees-first order until a whole pass
    /// analyses nothing. Everything the rounds read only grows, so the loop
    /// terminates.
    fn solve(&mut self) {
        let order = self.order.clone();
        loop {
            let mut any = false;
            for component in &order {
                loop {
                    let mut work: Vec<(FnRef, usize)> = Vec::new();
                    for fr in component {
                        if let Some(state) = self.fns.get(fr) {
                            let idxs: Vec<usize> = state.dirty.iter().copied().collect();
                            for idx in idxs {
                                work.push((*fr, idx));
                            }
                        }
                    }
                    if work.is_empty() {
                        break;
                    }
                    any = true;
                    for (fr, idx) in work {
                        self.fns
                            .get_mut(&fr)
                            .expect("work comes from a live function")
                            .dirty
                            .remove(&idx);
                        let effects = Flow::run(self, fr, idx);
                        self.apply(fr, idx, effects);
                    }
                }
            }
            if !any {
                break;
            }
        }
    }

    /// Give every function never called with a known tuple its `Top`
    /// fallback version, re-run, then write the joined types into the
    /// program and collect the table.
    /// The joined types per function, the specialisation table and the
    /// guards. The writes are applied by [`analyze`] once this driver's
    /// borrow of the program has ended.
    fn finish(&mut self) -> (Inference, Vec<(FnRef, Vec<Type>)>) {
        let missing: Vec<FnRef> = self
            .fns
            .iter()
            .filter(|(_, state)| state.clones.is_empty())
            .map(|(fr, _)| *fr)
            .collect();
        if !missing.is_empty() {
            for fr in missing {
                let arity = self.fns.get(&fr).map_or(0, |s| s.arity);
                self.push_clone(fr, vec![ATy::Top; arity], true, None);
            }
            self.solve();
        }
        let writes: Vec<(FnRef, Vec<Type>)> = self
            .fns
            .iter()
            .map(|(fr, state)| {
                let mut types = vec![ATy::Bottom; state.n_values];
                for clone in &state.clones {
                    for (t, c) in types.iter_mut().zip(&clone.types) {
                        join_into(t, c);
                    }
                }
                (
                    *fr,
                    types
                        .iter()
                        .map(materialize)
                        .map(|t| t.to_type(fr.module))
                        .collect(),
                )
            })
            .collect();
        let mut table = BTreeMap::new();
        for (fr, state) in &self.fns {
            if state.clones.is_empty() {
                continue;
            }
            table.insert(
                *fr,
                state
                    .clones
                    .iter()
                    .map(|c| CloneInfo {
                        args: c.args.iter().map(materialize).collect(),
                        top: c.top,
                        returns: materialize(&c.returns),
                        callers: c.routers.clone(),
                    })
                    .collect(),
            );
        }
        (
            Inference {
                table,
                guards: self.guards.iter().cloned().collect(),
                attrs: self.attrs.clone(),
            },
            writes,
        )
    }

    /// Create a clone of `callee` and register `from` as its caller. Exact
    /// tuples match first; past [`CLONE_LIMIT`] distinct ones the call
    /// routes to the `Top` version. Argument tuples never widen once
    /// created, so a caller re-routed by a grown argument type simply gets
    /// a new clone.
    fn route(&mut self, callee: FnRef, args: &[ATy], from: (FnRef, usize)) {
        let exact = self
            .fns
            .get(&callee)
            .expect("routed callees are program functions")
            .clones
            .iter()
            .position(|c| !c.top && c.args.as_slice() == args);
        let idx = match exact {
            Some(idx) => idx,
            None => {
                let exacts = self
                    .fns
                    .get(&callee)
                    .expect("routed callees are program functions")
                    .clones
                    .iter()
                    .filter(|c| !c.top)
                    .count();
                let top = self.fns[&callee].clones.iter().position(|c| c.top);
                match (exacts < CLONE_LIMIT, top) {
                    (true, _) => self.push_clone(callee, args.to_vec(), false, Some(from)),
                    (false, Some(idx)) => idx,
                    (false, None) => {
                        let arity = self.fns[&callee].arity;
                        self.push_clone(callee, vec![ATy::Top; arity], true, Some(from))
                    }
                }
            }
        };
        let state = self
            .fns
            .get_mut(&callee)
            .expect("routed callees are program functions");
        state.clones[idx].routers.insert(from);
        self.call_readers.entry(callee).or_default().insert(from);
    }

    /// Append a fresh clone, mark it dirty, notify the callers watching the
    /// function, and seed the `Top` version's entry guards from the
    /// annotations.
    fn push_clone(
        &mut self,
        callee: FnRef,
        args: Vec<ATy>,
        top: bool,
        from: Option<(FnRef, usize)>,
    ) -> usize {
        let narrowed: Vec<Option<ATy>> = if top {
            let arity = args.len();
            (0..arity).map(|i| self.annot_ty(callee, i)).collect()
        } else {
            Vec::new()
        };
        let state = self
            .fns
            .get_mut(&callee)
            .expect("routed callees are program functions");
        state.clones.push(CloneRec {
            args,
            top,
            returns: ATy::Bottom,
            types: vec![ATy::Bottom; state.n_values],
            narrow: BTreeMap::new(),
            routers: BTreeSet::new(),
        });
        let idx = state.clones.len() - 1;
        state.dirty.insert(idx);
        for (i, ty) in narrowed.into_iter().enumerate() {
            if let Some(ty) = ty {
                self.guards.insert(Guard {
                    at: callee,
                    clone: idx,
                    block: state.entry.0,
                    value: i as u32,
                    ty,
                    entry: true,
                });
            }
        }
        if let Some(from) = from {
            let state = self
                .fns
                .get_mut(&callee)
                .expect("routed callees are program functions");
            state.clones[idx].routers.insert(from);
            self.call_readers.entry(callee).or_default().insert(from);
        }
        let watchers = self
            .watchers_of(callee)
            .into_iter()
            .chain(from)
            .collect::<Vec<_>>();
        for (fr, idx) in watchers {
            if let Some(state) = self.fns.get_mut(&fr) {
                state.dirty.insert(idx);
            }
        }
        idx
    }

    fn watchers_of(&self, callee: FnRef) -> BTreeSet<(FnRef, usize)> {
        let mut out: BTreeSet<(FnRef, usize)> =
            self.call_readers.get(&callee).cloned().unwrap_or_default();
        out.extend(
            self.fns
                .get(&callee)
                .map(|s| {
                    s.clones
                        .iter()
                        .flat_map(|c| c.routers.iter().copied())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
        out
    }

    /// The plain-class-name annotation of parameter `i` as a type: a
    /// program class of the function's module, or a builtin type name.
    fn annot_ty(&self, fr: FnRef, i: usize) -> Option<ATy> {
        let state = self.fns.get(&fr)?;
        let name = state.params.get(i)?;
        let annot = state.annots.get(name)?;
        if let Some(ty) = builtin_class_ty(annot) {
            return Some(ty);
        }
        match self.program.namespaces[fr.module as usize]
            .defs
            .get(annot)
            .map(std::ops::Deref::deref)
        {
            Some([TopDef::Class(cr)]) => Some(ATy::Inst {
                cls: *cr,
                exact: false,
            }),
            _ => None,
        }
    }

    /// The join of the return types of every clone whose argument tuple
    /// covers `args`.
    fn returns_of(&self, callee: FnRef, args: &[ATy]) -> ATy {
        let state = self
            .fns
            .get(&callee)
            .expect("routed callees are program functions");
        let mut out = ATy::Bottom;
        for clone in &state.clones {
            if clone
                .args
                .iter()
                .zip(args.iter())
                .all(|(param, arg)| subsumes(param, arg))
            {
                join_into(&mut out, &clone.returns);
            }
        }
        out
    }

    /// Fold one clone's analysis into the shared tables and its own state,
    /// dirtying whoever read a quantity that grew.
    fn apply(&mut self, fr: FnRef, idx: usize, effects: FlowOut) {
        let FlowOut {
            base,
            narrow,
            returns,
            routes,
            attr_writes,
            attr_reads,
            global_writes,
            global_reads,
            cell_writes,
            cell_reads,
            guards,
        } = effects;
        for (callee, args) in routes {
            self.route(callee, &args, (fr, idx));
        }
        for (key, ty) in attr_writes {
            let grown = join_into(self.attrs.entry(key.clone()).or_default(), &ty);
            if grown {
                self.dirty_attr_readers(&key);
            }
        }
        for key in attr_reads {
            self.attr_readers.entry(key).or_default().insert((fr, idx));
        }
        for (key, ty) in global_writes {
            let grown = join_into(self.globals.entry(key.clone()).or_default(), &ty);
            if grown {
                self.dirty_global_readers(&key);
            }
        }
        for key in global_reads {
            self.global_readers
                .entry(key)
                .or_default()
                .insert((fr, idx));
        }
        for (key, ty) in cell_writes {
            if join_into(self.cells.entry(key).or_default(), &ty) {
                self.dirty_cell_readers(&key);
            }
        }
        for key in cell_reads {
            self.cell_readers.entry(key).or_default().insert((fr, idx));
        }
        for (block, value, ty) in guards {
            self.guards.insert(Guard {
                at: fr,
                clone: idx,
                block,
                value: value.0,
                ty,
                entry: false,
            });
        }
        let state = self.fns.get_mut(&fr).expect("analysed functions are live");
        let grew = {
            let clone = &mut state.clones[idx];
            for (t, b) in clone.types.iter_mut().zip(&base) {
                join_into(t, b);
            }
            clone.narrow = narrow;
            join_into(&mut clone.returns, &returns)
        };
        if grew {
            let readers = self.call_readers.get(&fr).cloned().unwrap_or_default();
            for (reader, ridx) in readers {
                if let Some(state) = self.fns.get_mut(&reader) {
                    state.dirty.insert(ridx);
                }
            }
        }
    }

    fn dirty_attr_readers(&mut self, key: &(ClassRef, String)) {
        let readers = self.attr_readers.get(key).cloned().unwrap_or_default();
        for (fr, idx) in readers {
            if let Some(state) = self.fns.get_mut(&fr) {
                state.dirty.insert(idx);
            }
        }
    }

    fn dirty_global_readers(&mut self, key: &(u32, String)) {
        let readers = self.global_readers.get(key).cloned().unwrap_or_default();
        for (fr, idx) in readers {
            if let Some(state) = self.fns.get_mut(&fr) {
                state.dirty.insert(idx);
            }
        }
    }

    fn dirty_cell_readers(&mut self, key: &(FnRef, u32)) {
        let readers = self.cell_readers.get(key).cloned().unwrap_or_default();
        for (fr, idx) in readers {
            if let Some(state) = self.fns.get_mut(&fr) {
                state.dirty.insert(idx);
            }
        }
    }

    /// Bind a call's argument types to a callee's parameters: the
    /// positional parameters in order (extras into the vararg tuple),
    /// keywords by name, and the holes left over taking the constant
    /// defaults or `Top`.
    fn tuple_for(&self, callee: FnRef, pos: &[ATy], kw: &[(String, ATy)]) -> Vec<ATy> {
        let state = match self.fns.get(&callee) {
            Some(state) => state,
            None => return Vec::new(),
        };
        let mut bound = vec![ATy::Top; state.arity];
        let supplied = pos.len().min(state.n_pos);
        for (i, ty) in pos.iter().take(state.n_pos).enumerate() {
            bound[i] = ty.clone();
        }
        if let Some(v) = state.vararg_idx {
            bound[v] = if pos.len() > state.n_pos {
                ATy::Tuple(pos[state.n_pos..].to_vec().into())
            } else {
                ATy::Tuple(Vec::new().into())
            };
        }
        // Positional holes take the constant defaults, which align to the
        // trailing positional parameters.
        for (i, slot) in bound
            .iter_mut()
            .enumerate()
            .take(state.n_pos)
            .skip(supplied)
        {
            let hole = state.n_pos - i;
            *slot = if hole <= state.defaults.len() {
                state.defaults[state.defaults.len() - hole].clone()
            } else {
                ATy::Top
            };
        }
        let kwonly_end = state.kwarg_idx.unwrap_or(state.arity);
        for (name, ty) in kw {
            if let Some(i) = state.params[..state.n_pos].iter().position(|p| p == name) {
                if i >= supplied {
                    bound[i] = ty.clone();
                }
            } else if let Some(idx) =
                (state.kwonly_start..kwonly_end).find(|idx| state.params[*idx] == *name)
            {
                bound[idx] = ty.clone();
            }
        }
        for (idx, slot) in bound
            .iter_mut()
            .enumerate()
            .take(kwonly_end.min(state.arity))
            .skip(state.kwonly_start)
        {
            if slot.is_top() {
                *slot = state
                    .kwdefaults
                    .get(&state.params[idx])
                    .cloned()
                    .unwrap_or(ATy::Top);
            }
        }
        if let Some(k) = state.kwarg_idx {
            bound[k] = ATy::Dict;
        }
        bound
    }
}

impl FnState {
    fn of(program: &Program, fr: FnRef) -> Option<FnState> {
        let ir = program.modules[fr.module as usize].ir.as_ref()?;
        let function = &ir.functions[fr.function.0 as usize];
        let params: Vec<String> = function
            .params
            .names()
            .iter()
            .map(|s| ir.consts.sym_str(*s).to_owned())
            .collect();
        let n_pos = function.params.positional.len();
        let vararg_idx = function.params.vararg.is_some().then_some(n_pos);
        let kwonly_start = n_pos + usize::from(vararg_idx.is_some());
        let kwarg_idx = function
            .params
            .kwarg
            .is_some()
            .then_some(kwonly_start + function.params.kwonly.len());
        let arity = function.params.arity();
        let (rpo, tail, preds) = cfg(function);
        let mut def_site = HashMap::new();
        for (bi, block) in function.blocks.iter().enumerate() {
            for (ii, instr) in block.instrs.iter().enumerate() {
                for value in instr_results(instr) {
                    def_site.insert(value, (BlockId(bi as u32), ii));
                }
            }
        }
        Some(FnState {
            params,
            n_pos,
            vararg_idx,
            kwonly_start,
            kwarg_idx,
            arity,
            defaults: Vec::new(),
            kwdefaults: BTreeMap::new(),
            annots: BTreeMap::new(),
            n_values: function.types.len(),
            entry: function.entry,
            rpo,
            tail,
            preds,
            def_site,
            clones: Vec::new(),
            dirty: BTreeSet::new(),
        })
    }
}

/// The results an instruction defines, in field order.
fn instr_results(instr: &Instr) -> Vec<Value> {
    match instr {
        Instr::Const { into, .. }
        | Instr::Copy { into, .. }
        | Instr::IAdd { into, .. }
        | Instr::ISub { into, .. }
        | Instr::IMul { into, .. }
        | Instr::IDiv { into, .. }
        | Instr::IMod { into, .. }
        | Instr::INeg { into, .. }
        | Instr::Shl { into, .. }
        | Instr::Shr { into, .. }
        | Instr::BitAnd { into, .. }
        | Instr::BitOr { into, .. }
        | Instr::BitXor { into, .. }
        | Instr::BitNot { into, .. }
        | Instr::ICmp { into, .. }
        | Instr::FAdd { into, .. }
        | Instr::FSub { into, .. }
        | Instr::FMul { into, .. }
        | Instr::FDiv { into, .. }
        | Instr::FMod { into, .. }
        | Instr::FNeg { into, .. }
        | Instr::FCmp { into, .. }
        | Instr::Box { into, .. }
        | Instr::Unbox { into, .. }
        | Instr::BinOp { into, .. }
        | Instr::UnaryOp { into, .. }
        | Instr::Compare { into, .. }
        | Instr::GetAttr { into, .. }
        | Instr::GetItem { into, .. }
        | Instr::GetIter { into, .. }
        | Instr::Next { into, .. }
        | Instr::Call { into, .. }
        | Instr::CallEx { into, .. }
        | Instr::CallMethod { into, .. }
        | Instr::Truth { into, .. }
        | Instr::Len { into, .. }
        | Instr::Contains { into, .. }
        | Instr::Format { into, .. }
        | Instr::BuildList { into, .. }
        | Instr::BuildTuple { into, .. }
        | Instr::BuildDict { into, .. }
        | Instr::BuildSet { into, .. }
        | Instr::BuildStr { into, .. }
        | Instr::BuildSlice { into, .. }
        | Instr::Import { into, .. }
        | Instr::MakeFunction { into, .. }
        | Instr::MakeClass { into, .. }
        | Instr::MakeCell { into, .. }
        | Instr::LoadClosure { into, .. }
        | Instr::LoadCell { into, .. }
        | Instr::LoadGlobal { into, .. }
        | Instr::LoadName { into, .. }
        | Instr::CallDirect { into, .. }
        | Instr::LoadField { into, .. }
        | Instr::AllocInst { into, .. }
        | Instr::AllocTuple { into, .. }
        | Instr::StackAlloc { into, .. }
        | Instr::ListGetUnchecked { into, .. }
        | Instr::TupleGetUnchecked { into, .. }
        | Instr::Yield { into, .. }
        | Instr::YieldFrom { into, .. }
        | Instr::Await { into, .. }
        | Instr::PushExcInfo { into, .. }
        | Instr::FinallyEnter { into, .. }
        | Instr::PrepReraiseStar { into, .. } => vec![*into],
        Instr::IAddOvf { into, ovf, .. }
        | Instr::ISubOvf { into, ovf, .. }
        | Instr::IMulOvf { into, ovf, .. }
        | Instr::IDivOvf { into, ovf, .. }
        | Instr::INegOvf { into, ovf, .. }
        | Instr::ShlOvf { into, ovf, .. } => vec![*into, *ovf],
        Instr::Unpack { into, .. } => into.to_vec(),
        Instr::TypeGuard { ok, fail, .. } | Instr::ShapeGuard { ok, fail, .. } => {
            vec![*ok, *fail]
        }
        Instr::CatchBegin { matched, exc, .. } => vec![*matched, *exc],
        Instr::CatchStarBegin {
            matched, exc, rest, ..
        } => vec![*matched, *exc, *rest],
        Instr::BoundsCheck { into, .. } => vec![*into],
        Instr::SetAttr { .. }
        | Instr::DelAttr { .. }
        | Instr::SetItem { .. }
        | Instr::DelItem { .. }
        | Instr::ImportStar { .. }
        | Instr::StoreCell { .. }
        | Instr::StoreGlobal { .. }
        | Instr::StoreName { .. }
        | Instr::StoreField { .. }
        | Instr::Safepoint
        | Instr::CatchEnd
        | Instr::PopExcInfo
        | Instr::FinallyExit { .. } => Vec::new(),
    }
}

/// The reachable blocks in reverse postorder, the unreachable tail, and the
/// predecessor lists.
fn cfg(function: &coil_ir::ir::Function) -> (Vec<BlockId>, Vec<BlockId>, Vec<Vec<BlockId>>) {
    let n = function.blocks.len();
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); n];
    for (bi, block) in function.blocks.iter().enumerate() {
        for succ in term_succs(&block.term) {
            preds[succ.0 as usize].push(BlockId(bi as u32));
        }
    }
    let mut post: Vec<BlockId> = Vec::new();
    let mut seen = vec![false; n];
    let mut stack: Vec<BlockId> = vec![function.entry];
    seen[function.entry.0 as usize] = true;
    while let Some(top) = stack.pop() {
        let mut pushed = false;
        for succ in term_succs(&function.blocks[top.0 as usize].term) {
            if !seen[succ.0 as usize] {
                seen[succ.0 as usize] = true;
                stack.push(top);
                stack.push(succ);
                pushed = true;
                break;
            }
        }
        if !pushed {
            post.push(top);
        }
    }
    let mut rpo: Vec<BlockId> = post;
    rpo.reverse();
    let tail: Vec<BlockId> = (0..n as u32)
        .filter(|b| !seen[*b as usize])
        .map(BlockId)
        .collect();
    (rpo, tail, preds)
}

fn term_succs(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Jump { to } => vec![*to],
        Terminator::Branch {
            then_to, else_to, ..
        } => {
            if then_to == else_to {
                vec![*then_to]
            } else {
                vec![*then_to, *else_to]
            }
        }
        Terminator::Switch { cases, default, .. } => {
            let mut out = vec![*default];
            out.extend(cases.iter().map(|(_, b)| *b));
            out
        }
        _ => Vec::new(),
    }
}

/// What the flow knows about a value beyond its type: the constant it
/// holds, the name it was loaded by (a signature key), the bound method it
/// is, and the closure cell it reads or writes.
#[derive(Default)]
struct Know {
    consts: HashMap<Value, ConstIdx>,
    name: HashMap<Value, String>,
    /// Bound-method knowledge from `GetAttr` and `CallMethod`.
    methods: HashMap<Value, MethodCall>,
    cell_key: HashMap<Value, (FnRef, u32)>,
    /// A `MakeCell`'s initial content, until the `MakeFunction` binding it.
    cell_init: HashMap<Value, ATy>,
    /// Stores of a cell never bound into a function.
    anon: HashMap<Value, ATy>,
    /// This function's own name stores, in store order.
    defs: HashMap<String, Vec<Value>>,
}

#[derive(Clone, Debug)]
struct MethodCall {
    recv: Value,
    name: String,
}

/// The flow-sensitive analysis of one clone: a least-fixpoint pass over
/// the blocks for the base types and element types, then a dominator walk
/// applying the guards. Reads of the shared tables (attributes, globals,
/// cells, callee returns) are recorded for `apply` to register.
struct Flow<'a, 'b> {
    d: &'b Driver<'a>,
    fr: FnRef,
    ir: &'a Module,
    function: &'a coil_ir::ir::Function,
    top: bool,
    arity: usize,
    entry: BlockId,
    rpo: Vec<BlockId>,
    tail: Vec<BlockId>,
    preds: Vec<Vec<BlockId>>,
    def_site: HashMap<Value, (BlockId, usize)>,
    base: Vec<ATy>,
    elems: Vec<ATy>,
    know: Know,
    /// An `import *` ran: names can no longer be resolved.
    poison: bool,
    out: FlowOut,
}

impl<'a, 'b> Flow<'a, 'b> {
    fn run(d: &'b Driver<'a>, fr: FnRef, idx: usize) -> FlowOut {
        let ir = d.program.modules[fr.module as usize]
            .ir
            .as_ref()
            .expect("reachable functions are compiled");
        let function = &ir.functions[fr.function.0 as usize];
        let state = &d.fns[&fr];
        let clone = &state.clones[idx];
        let mut base = vec![ATy::Bottom; state.n_values];
        for (i, slot) in base.iter_mut().enumerate().take(state.arity) {
            // A `Top` clone's parameters start at the plain-class-name
            // annotation where one narrows them; the entry guard recorded
            // at the clone's creation is what makes that sound.
            *slot = if clone.top {
                d.annot_ty(fr, i).unwrap_or_else(|| clone.args[i].clone())
            } else {
                clone.args[i].clone()
            };
        }
        let mut flow = Flow {
            d,
            fr,
            ir,
            function,
            top: clone.top,
            arity: state.arity,
            entry: state.entry,
            rpo: state.rpo.clone(),
            tail: state.tail.clone(),
            preds: state.preds.clone(),
            def_site: state.def_site.clone(),
            base,
            elems: vec![ATy::Bottom; state.n_values],
            know: Know::default(),
            poison: false,
            out: FlowOut::default(),
        };
        flow.base_pass();
        flow.narrow_pass();
        flow.out.base = flow.base;
        flow.out
    }

    fn set(&mut self, v: Value, t: ATy) -> bool {
        join_into(&mut self.base[v.idx()], &t)
    }

    fn set_elem(&mut self, v: Value, t: ATy) -> bool {
        join_into(&mut self.elems[v.idx()], &t)
    }

    /// The element type a value's iteration produces, from its own type.
    fn elem_ty(&self, v: Value) -> ATy {
        match &self.base[v.idx()] {
            ATy::Tuple(parts) => parts.iter().fold(ATy::Bottom, |a, b| join(&a, b)),
            ATy::List | ATy::Set => self.elems[v.idx()].clone(),
            // An opaque iterable a builtin produced (`range`, `iter`): the
            // element type the call recorded is everything known about it.
            ATy::Top | ATy::Bottom => {
                let e = self.elems[v.idx()].clone();
                if e == ATy::Bottom {
                    self.base[v.idx()].clone()
                } else {
                    e
                }
            }
            ATy::Str => ATy::Str,
            _ => ATy::Top,
        }
    }

    fn base_pass(&mut self) {
        let order: Vec<BlockId> = self.rpo.iter().chain(self.tail.iter()).copied().collect();
        loop {
            let mut changed = false;
            for &b in &order {
                let block = &self.function.blocks[b.0 as usize];
                for phi in &block.phis {
                    let mut t = ATy::Bottom;
                    let mut e = ATy::Bottom;
                    for (v, _) in &phi.incomings {
                        t = join(&t, &self.base[v.idx()]);
                        e = join(&e, &self.elems[v.idx()]);
                    }
                    changed |= self.set(phi.into, t);
                    changed |= self.set_elem(phi.into, e);
                }
                for instr in &block.instrs {
                    changed |= self.instr(instr);
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn instr(&mut self, instr: &Instr) -> bool {
        match instr {
            Instr::Const { idx, into } => {
                self.know.consts.insert(*into, *idx);
                self.set(*into, const_aty(self.ir, *idx))
            }
            Instr::Copy { src, into } => {
                if let Some(idx) = self.know.consts.get(src) {
                    self.know.consts.insert(*into, *idx);
                }
                if let Some(name) = self.know.name.get(src).cloned() {
                    self.know.name.insert(*into, name);
                }
                if let Some(mc) = self.know.methods.get(src).cloned() {
                    self.know.methods.insert(*into, mc);
                }
                let t = self.base[src.idx()].clone();
                let e = self.elems[src.idx()].clone();
                let changed = self.set(*into, t);
                self.set_elem(*into, e);
                changed
            }
            Instr::IAddOvf { into, ovf, .. }
            | Instr::ISubOvf { into, ovf, .. }
            | Instr::IMulOvf { into, ovf, .. }
            | Instr::IDivOvf { into, ovf, .. }
            | Instr::INegOvf { into, ovf, .. }
            | Instr::ShlOvf { into, ovf, .. } => {
                self.set(*into, ATy::Int64);
                self.set(*ovf, ATy::Bool)
            }
            Instr::IAdd { into, .. }
            | Instr::ISub { into, .. }
            | Instr::IMul { into, .. }
            | Instr::IDiv { into, .. }
            | Instr::IMod { into, .. }
            | Instr::INeg { into, .. }
            | Instr::Shl { into, .. }
            | Instr::Shr { into, .. }
            | Instr::BitAnd { into, .. }
            | Instr::BitOr { into, .. }
            | Instr::BitXor { into, .. }
            | Instr::BitNot { into, .. } => self.set(*into, ATy::Int64),
            Instr::ICmp { into, .. } | Instr::FCmp { into, .. } => self.set(*into, ATy::Bool),
            Instr::FAdd { into, .. }
            | Instr::FSub { into, .. }
            | Instr::FMul { into, .. }
            | Instr::FDiv { into, .. }
            | Instr::FMod { into, .. }
            | Instr::FNeg { into, .. } => self.set(*into, ATy::Float64),
            Instr::Box { scalar, into, .. } => self.set(
                *into,
                match scalar {
                    coil_ir::ir::Scalar::Int64 => ATy::BigInt,
                    coil_ir::ir::Scalar::Float64 => ATy::Float64,
                    coil_ir::ir::Scalar::Bool => ATy::Bool,
                },
            ),
            Instr::Unbox { scalar, into, .. } => self.set(
                *into,
                match scalar {
                    coil_ir::ir::Scalar::Int64 => ATy::Int64,
                    coil_ir::ir::Scalar::Float64 => ATy::Float64,
                    coil_ir::ir::Scalar::Bool => ATy::Bool,
                },
            ),
            Instr::BinOp { op, lhs, rhs, into } => self.binop(*op, *lhs, *rhs, *into),
            Instr::UnaryOp { op, operand, into } => self.unaryop(*op, *operand, *into),
            Instr::Compare { into, .. } => self.set(*into, ATy::Bool),
            Instr::Truth { into, .. } => self.set(*into, ATy::Bool),
            Instr::Contains { into, .. } => self.set(*into, ATy::Bool),
            Instr::Len { into, .. } => self.set(*into, ATy::Int64),
            Instr::Format { into, .. } => self.set(*into, ATy::Str),
            Instr::BuildList { items, into } => {
                let elem = items
                    .iter()
                    .fold(ATy::Bottom, |a, v| join(&a, &self.base[v.idx()]));
                let changed = self.set(*into, ATy::List);
                self.set_elem(*into, elem);
                changed
            }
            Instr::BuildTuple { items, into } => {
                let t = ATy::Tuple(items.iter().map(|v| self.base[v.idx()].clone()).collect());
                self.set(*into, t)
            }
            Instr::BuildDict { into, .. } => self.set(*into, ATy::Dict),
            Instr::BuildSet { items, into } => {
                let elem = items
                    .iter()
                    .fold(ATy::Bottom, |a, v| join(&a, &self.base[v.idx()]));
                let changed = self.set(*into, ATy::Set);
                self.set_elem(*into, elem);
                changed
            }
            Instr::BuildStr { into, .. } => self.set(*into, ATy::Str),
            Instr::BuildSlice { into, .. } => self.set(*into, ATy::Top),
            Instr::GetItem { obj, key, into } => self.getitem(*obj, *key, *into),
            Instr::SetItem { obj, value, .. } => {
                // A store into a list grows the list's element type.
                if members(&self.base[obj.idx()]).contains(&ATy::List) {
                    return self.set_elem(*obj, self.base[value.idx()].clone());
                }
                false
            }
            Instr::DelItem { .. } | Instr::DelAttr { .. } => false,
            Instr::GetIter { obj, into } => {
                let elem = self.elem_ty(*obj);
                let changed = self.set(*into, ATy::Top);
                changed | self.set_elem(*into, elem)
            }
            Instr::Next { iter, into } => {
                let t = self.elems[iter.idx()].clone();
                self.set(*into, t)
            }
            Instr::Call {
                callee,
                args,
                kwnames,
                into,
            } => {
                let pos = args.to_vec();
                let kw = kwnames.to_vec();
                self.eval_call(*callee, &pos, &kw, *into)
            }
            Instr::CallEx {
                callee, args, into, ..
            } => {
                let pos = match &self.base[args.idx()] {
                    ATy::Tuple(parts) => Some(parts.to_vec()),
                    ATy::Bottom => Some(Vec::new()),
                    _ => None,
                };
                self.eval_call_ex(*callee, pos.as_deref(), *into)
            }
            Instr::CallMethod {
                name,
                recv,
                args,
                kwnames,
                into,
            } => {
                let mc = MethodCall {
                    recv: *recv,
                    name: self.ir.consts.sym_str(*name).to_owned(),
                };
                let pos = args.to_vec();
                let kw = kwnames.to_vec();
                self.eval_method(&mc, &pos, &kw, *into)
            }
            Instr::CallDirect { func, args, into } => {
                let callee = FnRef {
                    module: self.fr.module,
                    function: *func,
                };
                let pos: Vec<ATy> = args.iter().map(|v| self.base[v.idx()].clone()).collect();
                let tuple = self.d.tuple_for(callee, &pos, &[]);
                self.out.routes.push((callee, tuple.clone()));
                let t = self.d.returns_of(callee, &tuple);
                self.set(*into, t)
            }
            Instr::Import {
                name,
                fromlist,
                level,
                into,
            } => self.import(*name, *fromlist, *level, *into),
            Instr::ImportStar { .. } => {
                self.poison = true;
                false
            }
            Instr::MakeFunction {
                func, cells, into, ..
            } => {
                let callee = FnRef {
                    module: self.fr.module,
                    function: *func,
                };
                self.bind_cells(callee, cells);
                self.set(*into, ATy::Func(callee))
            }
            Instr::MakeClass {
                desc, cells, into, ..
            } => {
                let cls = ClassRef {
                    module: self.fr.module,
                    class: *desc,
                };
                let body = FnRef {
                    module: self.fr.module,
                    function: self.ir.classes[desc.0 as usize].body,
                };
                self.bind_cells(body, cells);
                self.out.routes.push((body, Vec::new()));
                self.set(*into, ATy::Class(cls))
            }
            Instr::MakeCell { initial, into } => {
                let init = match initial {
                    Some(v) => self.base[v.idx()].clone(),
                    None => ATy::Bottom,
                };
                self.know.cell_init.insert(*into, init);
                self.set(*into, ATy::Top)
            }
            Instr::LoadClosure { index, into } => {
                let key = (self.fr, *index);
                self.out.cell_reads.insert(key);
                let t = self.d.cells.get(&key).cloned().unwrap_or(ATy::Bottom);
                self.set(*into, t)
            }
            Instr::LoadCell { cell, into } => {
                if let Some(&key) = self.know.cell_key.get(cell) {
                    self.out.cell_reads.insert(key);
                    let t = self.d.cells.get(&key).cloned().unwrap_or(ATy::Bottom);
                    self.set(*into, t)
                } else {
                    let t = self.know.anon.get(cell).cloned().unwrap_or(ATy::Top);
                    self.set(*into, t)
                }
            }
            Instr::StoreCell { cell, value } => {
                let t = self.base[value.idx()].clone();
                if let Some(&key) = self.know.cell_key.get(cell) {
                    let slot = self.out.cell_writes.entry(key).or_default();
                    join_into(slot, &t);
                } else {
                    self.know.anon.insert(*cell, t);
                }
                false
            }
            Instr::LoadGlobal { name, into } | Instr::LoadName { name, into } => {
                self.load_name(*name, *into)
            }
            Instr::StoreGlobal { name, value } | Instr::StoreName { name, value } => {
                let name = self.ir.consts.sym_str(*name).to_owned();
                self.know.defs.entry(name.clone()).or_default().push(*value);
                // A store in a module body binds a global; a `global`
                // declaration lowers to `StoreGlobal` anywhere.
                let module_body = self.fr.function == self.ir.body;
                if matches!(instr, Instr::StoreGlobal { .. }) || module_body {
                    let key = (self.fr.module, name);
                    let t = self.base[value.idx()].clone();
                    let slot = self.out.global_writes.entry(key).or_default();
                    join_into(slot, &t);
                }
                false
            }
            Instr::GetAttr { obj, name, into } => self.get_attr(*obj, *name, *into),
            Instr::SetAttr { obj, name, value } => {
                let name = self.ir.consts.sym_str(*name).to_owned();
                let t = self.base[value.idx()].clone();
                for m in members(&self.base[obj.idx()]) {
                    match m {
                        ATy::Inst { cls, .. } => {
                            let slot = self.out.attr_writes.entry((cls, name.clone())).or_default();
                            join_into(slot, &t);
                        }
                        ATy::Class(cr) => {
                            let slot = self.out.attr_writes.entry((cr, name.clone())).or_default();
                            join_into(slot, &t);
                        }
                        _ => {}
                    }
                }
                false
            }
            Instr::LoadField {
                shape, slot, into, ..
            } => {
                let Some(key) = self.field_key(*shape, *slot) else {
                    return self.set(*into, ATy::Top);
                };
                self.out.attr_reads.insert(key.clone());
                let t = self.d.attrs.get(&key).cloned().unwrap_or(ATy::Bottom);
                self.set(*into, t)
            }
            Instr::StoreField {
                shape, slot, value, ..
            } => {
                let Some(key) = self.field_key(*shape, *slot) else {
                    return false;
                };
                let t = self.base[value.idx()].clone();
                let slot = self.out.attr_writes.entry(key).or_default();
                join_into(slot, &t);
                false
            }
            Instr::AllocInst { cls, into, .. } | Instr::StackAlloc { cls, into, .. } => {
                let t = ATy::Inst {
                    cls: ClassRef {
                        module: self.fr.module,
                        class: *cls,
                    },
                    exact: true,
                };
                self.set(*into, t)
            }
            Instr::AllocTuple { items, into } => {
                let t = ATy::Tuple(items.iter().map(|v| self.base[v.idx()].clone()).collect());
                self.set(*into, t)
            }
            Instr::TypeGuard {
                value,
                ty,
                ok,
                fail,
                ..
            } => {
                let t = from_type(ty, self.fr.module, &self.ir.consts);
                self.set(*value, t.clone());
                self.set(*ok, t);
                self.set(*fail, ATy::Bool)
            }
            Instr::ShapeGuard {
                obj,
                shape,
                ok,
                fail,
            } => {
                let cls = self.d.shapes.shape(*shape).cls;
                let t = ATy::Inst { cls, exact: false };
                self.set(*obj, t.clone());
                self.set(*ok, t);
                self.set(*fail, ATy::Bool)
            }
            Instr::BoundsCheck { into, .. } => self.set(*into, ATy::Bool),
            Instr::ListGetUnchecked { list, into, .. } => {
                let t = self.elems[list.idx()].clone();
                self.set(*into, t)
            }
            Instr::TupleGetUnchecked {
                tuple, index, into, ..
            } => {
                let t = match &self.base[tuple.idx()] {
                    ATy::Tuple(parts) => parts.get(*index as usize).cloned().unwrap_or(ATy::Top),
                    _ => ATy::Top,
                };
                self.set(*into, t)
            }
            Instr::Unpack { src, n, star, into } => self.unpack(*src, *n, *star, into),
            Instr::Yield { into, .. }
            | Instr::YieldFrom { into, .. }
            | Instr::Await { into, .. } => self.set(*into, ATy::Top),
            Instr::Safepoint | Instr::CatchEnd | Instr::PopExcInfo => false,
            Instr::PushExcInfo { into } => self.set(*into, ATy::Top),
            Instr::FinallyEnter { into } => self.set(*into, ATy::None),
            Instr::FinallyExit { .. } => false,
            Instr::CatchBegin { matched, exc, .. } => {
                self.set(*matched, ATy::Bool);
                self.set(*exc, ATy::Top)
            }
            Instr::CatchStarBegin {
                matched, exc, rest, ..
            } => {
                self.set(*matched, ATy::Bool);
                self.set(*exc, ATy::Top);
                self.set(*rest, ATy::Top)
            }
            Instr::PrepReraiseStar { into, .. } => self.set(*into, ATy::Top),
        }
    }

    fn bind_cells(&mut self, callee: FnRef, cells: &[Value]) {
        for (i, cell) in cells.iter().enumerate() {
            self.know.cell_key.insert(*cell, (callee, i as u32));
            if let Some(init) = self.know.cell_init.get(cell) {
                let slot = self.out.cell_writes.entry((callee, i as u32)).or_default();
                join_into(slot, init);
            }
        }
    }

    fn field_key(&self, shape: coil_ir::types::ShapeId, slot: u32) -> Option<(ClassRef, String)> {
        let s = self.d.shapes.shape(shape);
        let name = s.slots.get(slot as usize)?.clone();
        Some((s.cls, name))
    }

    fn const_int(&self, v: Value) -> Option<i64> {
        let idx = self.know.consts.get(&v)?;
        match &self.ir.consts.consts[idx.0 as usize] {
            Const::Int(i) => Some(*i),
            _ => None,
        }
    }

    fn binop(&mut self, op: BinOp, lhs: Value, rhs: Value, into: Value) -> bool {
        if let Some(t) = self.fold_ints(op, lhs, rhs) {
            return self.set(into, t);
        }
        let mut t = ATy::Bottom;
        for a in members(&self.base[lhs.idx()]) {
            for b in members(&self.base[rhs.idx()]) {
                t = join(&t, &binop_pair(op, &a, &b));
            }
        }
        let changed = self.set(into, t);
        // Container arithmetic carries the element type along.
        let left = &self.base[lhs.idx()];
        let right = &self.base[rhs.idx()];
        let elem = match op {
            BinOp::Add
                if has_word(left, "list")
                    && has_word(right, "list")
                    && self.elems[lhs.idx()] != ATy::Bottom =>
            {
                Some(join(&self.elems[lhs.idx()], &self.elems[rhs.idx()]))
            }
            BinOp::Mul if has_word(left, "list") && is_int_atom(right) => {
                Some(self.elems[lhs.idx()].clone())
            }
            BinOp::Mul if has_word(right, "list") && is_int_atom(left) => {
                Some(self.elems[rhs.idx()].clone())
            }
            _ => None,
        };
        if let Some(elem) = elem {
            self.set_elem(into, elem);
        }
        changed
    }

    /// Fold an integer operation on two constants in 128 bits: the result
    /// keeps `Int64` when it provably still fits.
    fn fold_ints(&mut self, op: BinOp, lhs: Value, rhs: Value) -> Option<ATy> {
        let a = self.const_int(lhs)?;
        let b = self.const_int(rhs)?;
        let wide = |v: i128| {
            if v >= i64::MIN as i128 && v <= i64::MAX as i128 {
                ATy::Int64
            } else {
                ATy::BigInt
            }
        };
        let folded = match op {
            BinOp::Add => i128::from(a).checked_add(i128::from(b))?,
            BinOp::Sub => i128::from(a).checked_sub(i128::from(b))?,
            BinOp::Mul => i128::from(a).checked_mul(i128::from(b))?,
            BinOp::Pow => {
                if !(0..=63).contains(&b) {
                    return None;
                }
                (a as i128).checked_pow(b as u32)?
            }
            BinOp::LShift => {
                if !(0..=62).contains(&b) {
                    return None;
                }
                (a as i128).checked_shl(b as u32)?
            }
            _ => return None,
        };
        Some(wide(folded))
    }

    fn unaryop(&mut self, op: UnaryOp, operand: Value, into: Value) -> bool {
        let mut t = ATy::Bottom;
        for m in members(&self.base[operand.idx()]) {
            let r = match op {
                UnaryOp::Not => ATy::Bool,
                UnaryOp::Neg if is_int_atom(&m) => ATy::BigInt,
                UnaryOp::Neg if m == ATy::Float64 => ATy::Float64,
                UnaryOp::Pos if matches!(m, ATy::Int64 | ATy::Bool) => ATy::Int64,
                UnaryOp::Pos if m == ATy::BigInt => ATy::BigInt,
                UnaryOp::Pos if m == ATy::Float64 => ATy::Float64,
                UnaryOp::Invert if is_int_atom(&m) => ATy::Int64,
                _ if m.is_top() => ATy::Top,
                _ => ATy::Bottom,
            };
            t = join(&t, &r);
        }
        self.set(into, t)
    }

    fn getitem(&mut self, obj: Value, key: Value, into: Value) -> bool {
        let index = self.const_int(key);
        let mut t = ATy::Bottom;
        for m in members(&self.base[obj.idx()]) {
            match &m {
                ATy::Tuple(parts) => {
                    if let Some(i) = index {
                        let at = if i < 0 { parts.len() as i64 + i } else { i };
                        if at >= 0 && (at as usize) < parts.len() {
                            t = join(&t, &parts[at as usize]);
                        }
                    } else {
                        t = join(&t, &parts.iter().fold(ATy::Bottom, |a, b| join(&a, b)));
                    }
                }
                ATy::List => t = join(&t, &self.elems[obj.idx()]),
                ATy::Dict | ATy::Top => t = join(&t, &ATy::Top),
                ATy::Str => t = join(&t, &ATy::Str),
                ATy::Bytes => t = join(&t, &ATy::Int64),
                _ => {}
            }
        }
        self.set(into, t)
    }

    fn unpack(&mut self, src: Value, n: u32, star: Option<u32>, into: &[Value]) -> bool {
        let n = n as usize;
        let mut results = vec![ATy::Bottom; n];
        let mut star_elem = ATy::Bottom;

        let elem = self.elem_ty(src);
        for m in members(&self.base[src.idx()]) {
            match &m {
                ATy::Tuple(parts) => match star {
                    None => {
                        for i in 0..n.min(parts.len()) {
                            results[i] = join(&results[i], &parts[i]);
                        }
                    }
                    Some(k) => {
                        let k = k as usize;
                        let tail = n - k - 1;
                        if parts.len() >= n {
                            for (i, p) in parts[..k].iter().enumerate() {
                                results[i] = join(&results[i], p);
                            }
                            for p in &parts[k..parts.len() - tail] {
                                star_elem = join(&star_elem, p);
                            }
                            for (j, p) in parts[parts.len() - tail..].iter().enumerate() {
                                results[k + 1 + j] = join(&results[k + 1 + j], p);
                            }
                        }
                    }
                },
                ATy::List | ATy::Set => {
                    for r in &mut results {
                        *r = join(r, &elem);
                    }
                    if star.is_some() {
                        star_elem = join(&star_elem, &elem);
                    }
                }
                ATy::Top => {
                    for r in &mut results {
                        *r = join(r, &ATy::Top);
                    }
                    if star.is_some() {
                        star_elem = join(&star_elem, &ATy::Top);
                    }
                }
                _ => {}
            }
        }
        let mut changed = false;
        for (i, v) in into.iter().enumerate() {
            if star == Some(i as u32) {
                changed |= self.set(*v, ATy::List);
                self.set_elem(*v, star_elem.clone());
            } else if i < n {
                changed |= self.set(*v, results[i].clone());
            }
        }
        changed
    }

    fn import(&mut self, name: Sym, fromlist: Value, level: Value, into: Value) -> bool {
        let name = self.ir.consts.sym_str(name).to_owned();
        let level = match self.const_int(level) {
            Some(l) => l,
            None => return self.set(into, ATy::Top),
        };
        let fromlist = match self.know.consts.get(&fromlist) {
            Some(idx) => match &self.ir.consts.consts[idx.0 as usize] {
                // `None` is a plain import; a tuple names the from-list.
                Const::None => false,
                Const::Tuple(_) => true,
                _ => return self.set(into, ATy::Top),
            },
            None => return self.set(into, ATy::Top),
        };
        let state = &self.d.program.modules[self.fr.module as usize];
        let target = if level == 0 {
            name
        } else {
            match relative_base(&state.name, state.is_package, level as i32) {
                Some(base) if name.is_empty() => base,
                Some(base) => format!("{base}.{name}"),
                None => return self.set(into, ATy::Top),
            }
        };
        let bound = if fromlist {
            self.d.mod_by_name.get(&target).copied()
        } else {
            target
                .split('.')
                .next()
                .and_then(|top| self.d.mod_by_name.get(top))
                .copied()
        };
        let t = bound.map(ATy::Module).unwrap_or(ATy::Top);
        self.set(into, t)
    }

    fn load_name(&mut self, name: Sym, into: Value) -> bool {
        let name = self.ir.consts.sym_str(name).to_owned();
        self.know.name.insert(into, name.clone());
        // A store this function performed is the local truth for the name.
        if let Some(defs) = self.know.defs.get(&name) {
            let mut t = ATy::Bottom;
            for v in defs {
                t = join(&t, &self.base[v.idx()]);
            }
            return self.set(into, t);
        }
        let key = (self.fr.module, name.clone());
        self.out.global_reads.insert(key.clone());
        let mut t = self.d.globals.get(&key).cloned().unwrap_or(ATy::Bottom);
        if !self.poison
            && let Some(defs) = self.d.program.namespaces[self.fr.module as usize]
                .defs
                .get(&name)
        {
            for def in defs {
                let d = match def {
                    TopDef::Function(f) => ATy::Func(*f),
                    TopDef::Class(c) => ATy::Class(*c),
                    TopDef::Module(m) => ATy::Module(*m),
                };
                t = join(&t, &d);
            }
        }
        if t == ATy::Bottom
            && let Some(s) = signatures::singleton(&name)
        {
            t = aty_of_sig(s);
        }
        // Still no fact: the globals table grows across rounds, so the
        // value stays `Bottom` rather than committing to `Top`.
        self.set(into, t)
    }

    fn get_attr(&mut self, obj: Value, name: Sym, into: Value) -> bool {
        let name = self.ir.consts.sym_str(name).to_owned();
        self.know.name.insert(into, name.clone());
        let mut t = ATy::Bottom;
        for m in members(&self.base[obj.idx()]) {
            match &m {
                ATy::Module(midx) => {
                    let full = format!("{}.{}", self.d.program.modules[*midx as usize].name, name);
                    if let Some(&sub) = self.d.mod_by_name.get(&full) {
                        t = join(&t, &ATy::Module(sub));
                    } else if let Some(defs) =
                        self.d.program.namespaces[*midx as usize].defs.get(&name)
                    {
                        for def in defs {
                            let d = match def {
                                TopDef::Function(f) => ATy::Func(*f),
                                TopDef::Class(c) => ATy::Class(*c),
                                TopDef::Module(sub) => ATy::Module(*sub),
                            };
                            t = join(&t, &d);
                        }
                    } else if let Some(s) = signatures::singleton(&full) {
                        t = join(&t, &aty_of_sig(s));
                    } else {
                        t = join(&t, &ATy::Top);
                    }
                }
                ATy::Inst { cls, .. } => {
                    let mro: Vec<ClassRef> = self
                        .d
                        .hierarchy
                        .mro(*cls)
                        .map(|m| m.to_vec())
                        .unwrap_or_else(|| vec![*cls]);
                    let mut found = ATy::Bottom;
                    for cr in &mro {
                        let key = (*cr, name.clone());
                        self.out.attr_reads.insert(key.clone());
                        if let Some(ty) = self.d.attrs.get(&key) {
                            found = ty.clone();
                            break;
                        }
                    }
                    if found != ATy::Bottom {
                        t = join(&t, &found);
                    } else {
                        // No attribute fact yet: the tables only grow, so
                        // the value stays `Bottom` and a later round may
                        // still pin it. A bound method always binds the
                        // receiver as the first argument, sealed or not.
                        self.know.methods.insert(
                            into,
                            MethodCall {
                                recv: obj,
                                name: name.clone(),
                            },
                        );
                        if self.d.shapes.is_sealed(*cls)
                            && let Some(f) = self.d.hierarchy.method(self.d.program, *cls, &name)
                        {
                            t = join(&t, &ATy::Func(f));
                        }
                    }
                }
                ATy::Class(cr) => {
                    if let Some(f) = self.d.hierarchy.method(self.d.program, *cr, &name) {
                        t = join(&t, &ATy::Func(f));
                    } else {
                        let key = (*cr, name.clone());
                        self.out.attr_reads.insert(key.clone());
                        let a = self.d.attrs.get(&key).cloned().unwrap_or(ATy::Bottom);
                        t = join(&t, &a);
                    }
                }
                m if word_of(m).is_some() => {
                    self.know.methods.insert(
                        into,
                        MethodCall {
                            recv: obj,
                            name: name.clone(),
                        },
                    );
                    t = join(&t, &ATy::Top);
                }
                ATy::Top => t = join(&t, &ATy::Top),
                _ => t = join(&t, &ATy::Top),
            }
        }
        self.set(into, t)
    }
}

impl<'a, 'b> Flow<'a, 'b> {
    fn eval_call(
        &mut self,
        callee: Value,
        pos: &[Value],
        kw: &[(Sym, Value)],
        into: Value,
    ) -> bool {
        if let Some(mc) = self.know.methods.get(&callee).cloned() {
            return self.eval_method(&mc, pos, kw, into);
        }
        let pos_tys: Vec<ATy> = pos.iter().map(|v| self.base[v.idx()].clone()).collect();
        let kw_pairs: Vec<(String, ATy)> = kw
            .iter()
            .map(|(s, v)| {
                (
                    self.ir.consts.sym_str(*s).to_owned(),
                    self.base[v.idx()].clone(),
                )
            })
            .collect();
        let name = self.know.name.get(&callee).cloned();
        let mut t = ATy::Bottom;
        for m in members(&self.base[callee.idx()]) {
            match &m {
                ATy::Func(f) => {
                    let tuple = self.d.tuple_for(*f, &pos_tys, &kw_pairs);
                    self.out.routes.push((*f, tuple.clone()));
                    let r = self.d.returns_of(*f, &tuple);
                    t = join(&t, &r);
                }
                ATy::Class(cr) => {
                    let sealed = self.d.shapes.is_sealed(*cr);
                    t = join(
                        &t,
                        &ATy::Inst {
                            cls: *cr,
                            exact: sealed,
                        },
                    );
                    // A sealed class's constructor runs the `__new__` and
                    // `__init__` its MRO names; the runtime's own
                    // `object.__new__` is outside the program.
                    if sealed {
                        if let Some(init) = self.d.hierarchy.method(self.d.program, *cr, "__init__")
                        {
                            let mut args = vec![ATy::Inst {
                                cls: *cr,
                                exact: false,
                            }];
                            args.extend(pos_tys.iter().cloned());
                            let tuple = self.d.tuple_for(init, &args, &kw_pairs);
                            self.out.routes.push((init, tuple));
                        }
                        if let Some(new) = self.d.hierarchy.method(self.d.program, *cr, "__new__") {
                            let mut args = vec![ATy::Class(*cr)];
                            args.extend(pos_tys.iter().cloned());
                            let tuple = self.d.tuple_for(new, &args, &kw_pairs);
                            self.out.routes.push((new, tuple));
                        }
                    }
                }
                // Calling a module always fails: no result, no route.
                ATy::Module(_) => {}
                _ => match &name {
                    Some(n) => {
                        if let Some(r) = sig_result(n, &pos_tys) {
                            t = join(&t, &r);
                        } else {
                            t = join(&t, &ATy::Top);
                        }
                        self.call_elem(n, pos, into);
                    }
                    None => t = join(&t, &ATy::Top),
                },
            }
        }
        // A pending callee (`Bottom`: its global is still unresolved) with
        // a builtin name answers from the signature table now; a later
        // round that resolves it to a program function joins over this.
        if t == ATy::Bottom
            && self.base[callee.idx()] == ATy::Bottom
            && let Some(n) = &name
        {
            if let Some(r) = sig_result(n, &pos_tys) {
                t = join(&t, &r);
            }
            self.call_elem(n, pos, into);
        }
        self.set(into, t)
    }

    /// A call whose positional list was built at runtime: the callee is
    /// known only when the argument tuple's type is.
    fn eval_call_ex(&mut self, callee: Value, pos: Option<&[ATy]>, into: Value) -> bool {
        let pos_tys: Vec<ATy> = pos.map(|p| p.to_vec()).unwrap_or_default();
        let mut t = ATy::Bottom;
        for m in members(&self.base[callee.idx()]) {
            match &m {
                ATy::Func(f) => match pos {
                    Some(_) => {
                        let tuple = self.d.tuple_for(*f, &pos_tys, &[]);
                        self.out.routes.push((*f, tuple.clone()));
                        let r = self.d.returns_of(*f, &tuple);
                        t = join(&t, &r);
                    }
                    None => t = join(&t, &ATy::Top),
                },
                ATy::Class(cr) => {
                    let sealed = self.d.shapes.is_sealed(*cr);
                    t = join(
                        &t,
                        &ATy::Inst {
                            cls: *cr,
                            exact: sealed,
                        },
                    );
                }
                ATy::Module(_) => {}
                _ => t = join(&t, &ATy::Top),
            }
        }
        if t == ATy::Bottom && matches!(self.base[callee.idx()], ATy::Top) {
            t = ATy::Top;
        }
        self.set(into, t)
    }

    fn eval_method(
        &mut self,
        mc: &MethodCall,
        pos: &[Value],
        kw: &[(Sym, Value)],
        into: Value,
    ) -> bool {
        let pos_tys: Vec<ATy> = pos.iter().map(|v| self.base[v.idx()].clone()).collect();
        let kw_pairs: Vec<(String, ATy)> = kw
            .iter()
            .map(|(s, v)| {
                (
                    self.ir.consts.sym_str(*s).to_owned(),
                    self.base[v.idx()].clone(),
                )
            })
            .collect();
        let mut t = ATy::Bottom;
        for m in members(&self.base[mc.recv.idx()]) {
            match &m {
                // A method call on an instance binds the receiver as the
                // first argument; a sealed class's override set is closed,
                // so the MRO's first definition is the one that runs.
                ATy::Inst { cls, .. } => {
                    if self.d.shapes.is_sealed(*cls)
                        && let Some(f) = self.d.hierarchy.method(self.d.program, *cls, &mc.name)
                    {
                        let mut args = vec![ATy::Inst {
                            cls: *cls,
                            exact: false,
                        }];
                        args.extend(pos_tys.iter().cloned());
                        let tuple = self.d.tuple_for(f, &args, &kw_pairs);
                        self.out.routes.push((f, tuple.clone()));
                        t = join(&t, &self.d.returns_of(f, &tuple));
                    } else {
                        t = join(&t, &ATy::Top);
                    }
                }
                // A class attribute access names its definition exactly.
                ATy::Class(cr) => {
                    if let Some(f) = self.d.hierarchy.method(self.d.program, *cr, &mc.name) {
                        let tuple = self.d.tuple_for(f, &pos_tys, &kw_pairs);
                        self.out.routes.push((f, tuple.clone()));
                        t = join(&t, &self.d.returns_of(f, &tuple));
                    } else {
                        t = join(&t, &ATy::Top);
                    }
                }
                ATy::Module(midx) => {
                    let defs = self.d.program.namespaces[*midx as usize]
                        .defs
                        .get(&mc.name)
                        .cloned()
                        .unwrap_or_default();
                    let mut resolved = false;
                    for def in &defs {
                        match def {
                            TopDef::Function(f) => {
                                let tuple = self.d.tuple_for(*f, &pos_tys, &kw_pairs);
                                self.out.routes.push((*f, tuple.clone()));
                                t = join(&t, &self.d.returns_of(*f, &tuple));
                                resolved = true;
                            }
                            TopDef::Class(cr) => {
                                let sealed = self.d.shapes.is_sealed(*cr);
                                t = join(
                                    &t,
                                    &ATy::Inst {
                                        cls: *cr,
                                        exact: sealed,
                                    },
                                );
                                resolved = true;
                            }
                            TopDef::Module(_) => {}
                        }
                    }
                    if !resolved {
                        let full = format!(
                            "{}.{}",
                            self.d.program.modules[*midx as usize].name, mc.name
                        );
                        if let Some(s) = signatures::singleton(&full) {
                            t = join(&t, &aty_of_sig(s));
                        } else {
                            t = join(&t, &ATy::Top);
                        }
                    }
                }
                other => {
                    if let Some(w) = word_of(other) {
                        let key = format!("{w}.{}", mc.name);
                        let mut args = vec![other.clone()];
                        args.extend(pos_tys.iter().cloned());
                        if let Some(r) = sig_result(&key, &args) {
                            t = join(&t, &r);
                        } else {
                            t = join(&t, &ATy::Top);
                        }
                        self.method_elem(w, &mc.name, mc.recv, into);
                    } else if let Some(r) = sig_result(&mc.name, &pos_tys) {
                        t = join(&t, &r);
                    } else {
                        t = join(&t, &ATy::Top);
                    }
                }
            }
        }
        self.set(into, t)
    }

    /// The element type of the builtin iterables' results, from the
    /// signatures' own vocabulary.
    fn call_elem(&mut self, name: &str, pos: &[Value], into: Value) {
        match name {
            "range" => {
                self.set_elem(into, ATy::Int64);
            }
            "iter" | "reversed" | "list" | "set" | "sorted" if !pos.is_empty() => {
                let e = self.elem_ty(pos[0]);
                self.set_elem(into, e);
            }
            "enumerate" if !pos.is_empty() => {
                let e = self.elem_ty(pos[0]);
                self.set_elem(into, ATy::Tuple(vec![ATy::Int64, e].into()));
            }
            "zip" => {
                let parts: Vec<ATy> = pos.iter().map(|v| self.elem_ty(*v)).collect();
                self.set_elem(into, ATy::Tuple(parts.into()));
            }
            _ => {}
        }
    }

    fn method_elem(&mut self, word: &str, name: &str, recv: Value, into: Value) {
        let splits = matches!(name, "split" | "rsplit" | "splitlines");
        if word == "str" && splits {
            self.set_elem(into, ATy::Str);
        } else if word == "bytes" && splits {
            self.set_elem(into, ATy::Bytes);
        } else if word == "list" && name == "copy" {
            let e = self.elems[recv.idx()].clone();
            self.set_elem(into, e);
        }
    }

    /// The dominator walk: each block starts from its immediate dominator's
    /// narrowed map, plus the guard of the single branch edge into it, then
    /// computes return types with the narrowed values in force.
    fn narrow_pass(&mut self) {
        let idom = dominators(self.entry, &self.preds, &self.rpo);
        let mut narrows: Vec<HashMap<u32, ATy>> = vec![HashMap::new(); self.function.blocks.len()];
        for &b in &self.rpo {
            let mut m: HashMap<u32, ATy> = match idom[b.0 as usize] {
                Some(p) => narrows[p.0 as usize].clone(),
                None => HashMap::new(),
            };
            if let Some((cond, positive)) = self.branch_into(b)
                && let Some((value, ty, record)) = self.recognize(cond, positive)
            {
                let cur = self.base[value.idx()].clone();
                let n = meet(&cur, &ty, &self.d.hierarchy);
                if cur != ATy::Bottom && n != ATy::Bottom && n != cur {
                    m.insert(value.0, n.clone());
                    if record {
                        self.out.guards.push((b.0, value, n.clone()));
                    }
                    self.out.narrow.insert((b.0, value.0), n);
                }
            }
            narrows[b.0 as usize] = m;
        }
        // The `Top` version's annotation seeds.
        if self.top {
            for i in 0..self.arity {
                if let Some(t) = self.d.annot_ty(self.fr, i) {
                    self.out.narrow.insert((self.entry.0, i as u32), t);
                }
            }
        }
        let mut returns = ATy::Bottom;
        let mut saw_return = false;
        for (bi, block) in self.function.blocks.iter().enumerate() {
            if let Terminator::Return { value } = &block.term {
                saw_return = true;
                let t = narrows[bi]
                    .get(&value.0)
                    .cloned()
                    .unwrap_or_else(|| self.base[value.idx()].clone());
                returns = join(&returns, &t);
            }
        }
        if !saw_return {
            // Falling off the end returns `None`.
            returns = ATy::None;
        }
        self.out.returns = returns;
    }

    /// The branch feeding exactly one of this block's edges, with whether
    /// this block is the true target.
    fn branch_into(&self, b: BlockId) -> Option<(Value, bool)> {
        for &p in &self.preds[b.0 as usize] {
            let term = &self.function.blocks[p.0 as usize].term;
            if let Terminator::Branch {
                cond,
                then_to,
                else_to,
            } = term
            {
                if *then_to == b && *else_to != b {
                    return Some((*cond, true));
                }
                if *else_to == b && *then_to != b {
                    return Some((*cond, false));
                }
            }
        }
        None
    }

    /// The narrowing a branch condition implies on its true (or false)
    /// edge, with whether a guard should be recorded for it.
    fn recognize(&self, cond: Value, positive: bool) -> Option<(Value, ATy, bool)> {
        let mut v = cond;
        let mut pos = positive;
        loop {
            let &(block, ii) = self.def_site.get(&v)?;
            let instr = &self.function.blocks[block.0 as usize].instrs[ii];
            match instr {
                Instr::Truth { value: w, .. } | Instr::Copy { src: w, .. } => {
                    v = *w;
                }
                Instr::Box {
                    src: w,
                    scalar: coil_ir::ir::Scalar::Bool,
                    ..
                } => {
                    v = *w;
                }
                Instr::UnaryOp {
                    op: UnaryOp::Not,
                    operand: w,
                    ..
                } => {
                    v = *w;
                    pos = !pos;
                }
                Instr::Compare { op, lhs, rhs, .. }
                    if matches!(op, coil_ir::ir::CmpOp::Is | coil_ir::ir::CmpOp::IsNot) =>
                {
                    if (*op == coil_ir::ir::CmpOp::Is) != pos {
                        return None;
                    }
                    if let Some(x) = self.type_arg(*lhs) {
                        return self.class_guard(*rhs, x, true);
                    }
                    if let Some(x) = self.type_arg(*rhs) {
                        return self.class_guard(*lhs, x, true);
                    }
                    return None;
                }
                Instr::Call { callee, args, .. }
                    if pos && args.len() == 2 && self.is_builtin(*callee, "isinstance") =>
                {
                    let ty = self.guard_ty(args[1], false)?;
                    return Some((args[0], ty, true));
                }
                Instr::IAddOvf { into, ovf, .. }
                | Instr::ISubOvf { into, ovf, .. }
                | Instr::IMulOvf { into, ovf, .. }
                | Instr::IDivOvf { into, ovf, .. }
                | Instr::INegOvf { into, ovf, .. }
                | Instr::ShlOvf { into, ovf, .. }
                    if v == *ovf && pos =>
                {
                    return Some((*into, ATy::BigInt, false));
                }
                _ => return None,
            }
        }
    }

    /// `type(x)`'s subject, when the value is that call.
    fn type_arg(&self, v: Value) -> Option<Value> {
        let &(block, ii) = self.def_site.get(&v)?;
        let Instr::Call { callee, args, .. } = &self.function.blocks[block.0 as usize].instrs[ii]
        else {
            return None;
        };
        (args.len() == 1 && self.is_builtin(*callee, "type")).then_some(args[0])
    }

    /// `type(x) is <class operand>` narrows `x` to the exact class.
    fn class_guard(
        &self,
        operand: Value,
        subject: Value,
        exact: bool,
    ) -> Option<(Value, ATy, bool)> {
        if let ATy::Class(cr) = &self.base[operand.idx()] {
            return Some((subject, ATy::Inst { cls: *cr, exact }, true));
        }
        if let Some(name) = self.know.name.get(&operand)
            && !matches!(
                self.base[operand.idx()],
                ATy::Func(_) | ATy::Class(_) | ATy::Module(_)
            )
            && let Some(ty) = builtin_class_ty(name)
        {
            return Some((subject, ty, true));
        }
        None
    }

    /// The type an `isinstance` second argument narrows to: a class value,
    /// a builtin type name, or the join over a tuple of them.
    fn guard_ty(&self, operand: Value, exact: bool) -> Option<ATy> {
        if let ATy::Class(cr) = &self.base[operand.idx()] {
            return Some(ATy::Inst { cls: *cr, exact });
        }
        if let Some(&(block, ii)) = self.def_site.get(&operand)
            && let Instr::BuildTuple { items, .. } =
                &self.function.blocks[block.0 as usize].instrs[ii]
        {
            let mut t = ATy::Bottom;
            for item in items {
                t = join(&t, &self.guard_ty(*item, exact)?);
            }
            return Some(t);
        }
        if let Some(name) = self.know.name.get(&operand)
            && !matches!(
                self.base[operand.idx()],
                ATy::Func(_) | ATy::Class(_) | ATy::Module(_)
            )
        {
            return builtin_class_ty(name);
        }
        None
    }

    /// Whether a value is the builtin of `name`: it carries the name and is
    /// no program object.
    fn is_builtin(&self, v: Value, name: &str) -> bool {
        self.know.name.get(&v).map(String::as_str) == Some(name)
            && !matches!(
                self.base[v.idx()],
                ATy::Func(_) | ATy::Class(_) | ATy::Module(_)
            )
    }
}

/// The members a type contributes to a pairwise operation: a union's
/// parts, nothing for `Bottom`.
fn members(t: &ATy) -> Vec<ATy> {
    match t {
        ATy::Bottom => Vec::new(),
        ATy::Union(parts) => parts
            .iter()
            .filter(|p| !matches!(p, ATy::Bottom))
            .cloned()
            .collect(),
        _ => vec![t.clone()],
    }
}

fn is_int_atom(t: &ATy) -> bool {
    matches!(t, ATy::Int64 | ATy::BigInt | ATy::Bool)
}

/// The signature table's word for a builtin receiver.
fn word_of(t: &ATy) -> Option<&'static str> {
    match t {
        ATy::Str => Some("str"),
        ATy::Bytes => Some("bytes"),
        ATy::List => Some("list"),
        ATy::Dict => Some("dict"),
        ATy::Set => Some("set"),
        ATy::Tuple(_) => Some("tuple"),
        ATy::Int64 | ATy::BigInt | ATy::Bool => Some("int"),
        ATy::Float64 => Some("float"),
        _ => None,
    }
}

fn has_word(t: &ATy, word: &str) -> bool {
    word_of(t) == Some(word)
}

fn const_aty(ir: &Module, idx: ConstIdx) -> ATy {
    match &ir.consts.consts[idx.0 as usize] {
        Const::None => ATy::None,
        Const::Bool(_) => ATy::Bool,
        Const::Int(_) => ATy::Int64,
        Const::BigInt(_) => ATy::BigInt,
        Const::Float(_) => ATy::Float64,
        Const::Str(_) => ATy::Str,
        Const::Bytes(_) => ATy::Bytes,
        Const::Tuple(items) => ATy::Tuple(items.iter().map(|i| const_aty(ir, *i)).collect()),
        Const::Ellipsis => ATy::Top,
    }
}

/// The result of a binary operation on one pair of member types: an int
/// operation that may overflow types `BigInt`, one provably bounded stays
/// `Int64`, a float on either side is contagious, and the container
/// algebra follows Python's.
fn binop_pair(op: BinOp, a: &ATy, b: &ATy) -> ATy {
    if a.is_top() || b.is_top() {
        return ATy::Top;
    }
    if is_int_atom(a) && is_int_atom(b) {
        return match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Pow | BinOp::LShift | BinOp::FloorDiv => {
                ATy::BigInt
            }
            BinOp::Mod | BinOp::RShift | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => ATy::Int64,
            BinOp::Div => ATy::Float64,
            BinOp::MatMul => ATy::Bottom,
        };
    }
    let numeric = (is_int_atom(a) || *a == ATy::Float64) && (is_int_atom(b) || *b == ATy::Float64);
    if numeric {
        return match op {
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::FloorDiv
            | BinOp::Mod
            | BinOp::Pow => ATy::Float64,
            _ => ATy::Bottom,
        };
    }
    match (a, b) {
        (ATy::Str, ATy::Str) if op == BinOp::Add => ATy::Str,
        (ATy::Str, _) if op == BinOp::Mod => ATy::Str,
        (ATy::Str, t) | (t, ATy::Str) if op == BinOp::Mul && is_int_atom(t) => ATy::Str,
        (ATy::Bytes, ATy::Bytes) if op == BinOp::Add => ATy::Bytes,
        (ATy::Bytes, _) if op == BinOp::Mod => ATy::Bytes,
        (ATy::Bytes, t) | (t, ATy::Bytes) if op == BinOp::Mul && is_int_atom(t) => ATy::Bytes,
        (ATy::List, ATy::List) if op == BinOp::Add => ATy::List,
        (ATy::List, t) | (t, ATy::List) if op == BinOp::Mul && is_int_atom(t) => ATy::List,
        (ATy::Tuple(p1), ATy::Tuple(p2)) if op == BinOp::Add => {
            ATy::Tuple(p1.iter().chain(p2.iter()).cloned().collect())
        }
        (ATy::Set, ATy::Set)
            if matches!(
                op,
                BinOp::BitOr | BinOp::BitAnd | BinOp::BitXor | BinOp::Sub
            ) =>
        {
            ATy::Set
        }
        (ATy::Dict, ATy::Dict) if op == BinOp::BitOr => ATy::Dict,
        _ => ATy::Bottom,
    }
}

/// The immediate dominators of the reachable blocks, by the iterative
/// algorithm over the reverse postorder.
fn dominators(entry: BlockId, preds: &[Vec<BlockId>], rpo: &[BlockId]) -> Vec<Option<BlockId>> {
    let n = preds.len();
    let mut idom: Vec<Option<BlockId>> = vec![None; n];
    let mut rpo_index = vec![usize::MAX; n];
    for (i, b) in rpo.iter().enumerate() {
        rpo_index[b.0 as usize] = i;
    }
    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo {
            if b == entry {
                continue;
            }
            let mut new: Option<BlockId> = None;
            for &p in &preds[b.0 as usize] {
                if idom[p.0 as usize].is_none() && p != entry {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(cur) => intersect(cur, p, &idom, &rpo_index),
                });
            }
            if new != idom[b.0 as usize] {
                idom[b.0 as usize] = new;
                changed = true;
            }
        }
    }
    idom
}

fn intersect(
    mut a: BlockId,
    mut b: BlockId,
    idom: &[Option<BlockId>],
    rpo_index: &[usize],
) -> BlockId {
    while a != b {
        while rpo_index[a.0 as usize] > rpo_index[b.0 as usize] {
            a = idom[a.0 as usize].unwrap_or(b);
        }
        while rpo_index[b.0 as usize] > rpo_index[a.0 as usize] {
            b = idom[b.0 as usize].unwrap_or(a);
        }
    }
    a
}

/// What the prescan found for one function.
#[derive(Default)]
struct Found {
    defaults: Vec<ATy>,
    kwdefaults: BTreeMap<String, ATy>,
    annots: BTreeMap<String, String>,
}

struct Prescan {
    found: BTreeMap<FnRef, Found>,
}

impl Prescan {
    /// Recover each function's constant defaults and plain-class-name
    /// annotations from the enclosing code that creates it: lowering emits
    /// `MakeFunction` with the default values and a `__annotations__` dict
    /// store on the function object. This reads the IR cache as lowered;
    /// no source is re-parsed.
    fn run(program: &Program) -> Prescan {
        let mut found: BTreeMap<FnRef, Found> = BTreeMap::new();
        for &fr in &program.functions {
            let Some(ir) = program.modules[fr.module as usize].ir.as_ref() else {
                continue;
            };
            let mut consts: HashMap<Value, ConstIdx> = HashMap::new();
            let mut loadname: HashMap<Value, String> = HashMap::new();
            let mut mfs: HashMap<Value, FnRef> = HashMap::new();
            let mut tuples: HashMap<Value, Vec<ATy>> = HashMap::new();
            let mut annot_dicts: HashMap<Value, Vec<(String, String)>> = HashMap::new();
            let mut kwdef_dicts: HashMap<Value, Vec<(String, ATy)>> = HashMap::new();
            for block in &ir.functions[fr.function.0 as usize].blocks {
                for instr in &block.instrs {
                    match instr {
                        Instr::Const { idx, into } => {
                            consts.insert(*into, *idx);
                        }
                        Instr::Copy { src, into } => {
                            if let Some(&c) = consts.get(src) {
                                consts.insert(*into, c);
                            }
                            if let Some(n) = loadname.get(src).cloned() {
                                loadname.insert(*into, n);
                            }
                        }
                        Instr::LoadName { name, into } | Instr::LoadGlobal { name, into } => {
                            loadname.insert(*into, ir.consts.sym_str(*name).to_owned());
                        }
                        Instr::BuildTuple { items, into } => {
                            let mut tys = Vec::with_capacity(items.len());
                            let mut all = true;
                            for item in items {
                                match consts.get(item) {
                                    Some(&c) => tys.push(const_aty(ir, c)),
                                    None => {
                                        all = false;
                                        break;
                                    }
                                }
                            }
                            if all {
                                tuples.insert(*into, tys);
                            }
                        }
                        Instr::BuildDict { items, into } => {
                            let mut annots = Vec::new();
                            let mut kwdefs = Vec::new();
                            let mut annot_ok = true;
                            let mut kw_ok = true;
                            for (k, v) in items {
                                let key = match consts.get(k) {
                                    Some(&c) => match &ir.consts.consts[c.0 as usize] {
                                        Const::Str(sym) => ir.consts.sym_str(*sym).to_owned(),
                                        _ => {
                                            annot_ok = false;
                                            kw_ok = false;
                                            break;
                                        }
                                    },
                                    None => {
                                        annot_ok = false;
                                        kw_ok = false;
                                        break;
                                    }
                                };
                                match annotation_word(ir, &consts, &loadname, *v) {
                                    Some(word) => annots.push((key.clone(), word)),
                                    None => annot_ok = false,
                                }
                                match consts.get(v) {
                                    Some(&c) => kwdefs.push((key, const_aty(ir, c))),
                                    None => kw_ok = false,
                                }
                            }
                            if annot_ok && !annots.is_empty() {
                                annot_dicts.insert(*into, annots);
                            }
                            if kw_ok && !kwdefs.is_empty() {
                                kwdef_dicts.insert(*into, kwdefs);
                            }
                        }
                        Instr::MakeFunction {
                            func,
                            into,
                            defaults,
                            ..
                        } => {
                            let callee = FnRef {
                                module: fr.module,
                                function: *func,
                            };
                            mfs.insert(*into, callee);
                            let entry = found.entry(callee).or_default();
                            if defaults.len() == 2 {
                                if let Some(tys) = tuples.get(&defaults[0]) {
                                    entry.defaults = tys.clone();
                                }
                                if let Some(pairs) = kwdef_dicts.get(&defaults[1]) {
                                    for (k, v) in pairs {
                                        entry.kwdefaults.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                        }
                        Instr::SetAttr { obj, name, value }
                            if ir.consts.sym_str(*name) == "__annotations__" =>
                        {
                            if let (Some(&callee), Some(pairs)) =
                                (mfs.get(obj), annot_dicts.get(value))
                            {
                                let entry = found.entry(callee).or_default();
                                for (k, v) in pairs {
                                    entry.annots.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Prescan { found }
    }
}

/// A parameter annotation's word: a plain class name loaded by name, or a
/// string constant (a forward reference).
fn annotation_word(
    ir: &Module,
    consts: &HashMap<Value, ConstIdx>,
    loadname: &HashMap<Value, String>,
    v: Value,
) -> Option<String> {
    if let Some(name) = loadname.get(&v) {
        return Some(name.clone());
    }
    match consts.get(&v) {
        Some(&c) => match &ir.consts.consts[c.0 as usize] {
            Const::Str(sym) => Some(ir.consts.sym_str(*sym).to_owned()),
            _ => None,
        },
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_collapses_the_int_chain_upward() {
        assert_eq!(join(&ATy::Int64, &ATy::BigInt), ATy::BigInt);
        assert_eq!(join(&ATy::Bool, &ATy::Int64), ATy::Int64);
        assert_eq!(join(&ATy::Bool, &ATy::BigInt), ATy::BigInt);
        assert_eq!(join(&ATy::Bottom, &ATy::Str), ATy::Str);
        assert_eq!(join(&ATy::Str, &ATy::Top), ATy::Top);
    }

    #[test]
    fn join_builds_normalised_unions() {
        assert_eq!(
            join(&ATy::Int64, &ATy::Str),
            ATy::Union(vec![ATy::Int64, ATy::Str].into())
        );
        // Members sort, deduplicate and collapse.
        assert_eq!(
            join(&ATy::Str, &ATy::Int64),
            ATy::Union(vec![ATy::Int64, ATy::Str].into())
        );
        assert_eq!(
            join(&ATy::Union(vec![ATy::Int64, ATy::Str].into()), &ATy::Int64),
            ATy::Union(vec![ATy::Int64, ATy::Str].into())
        );
    }

    #[test]
    fn wide_unions_collapse_to_top() {
        let wide = [ATy::Int64, ATy::Str, ATy::Float64, ATy::None, ATy::Bytes]
            .into_iter()
            .fold(ATy::Bottom, |a, b| join(&a, &b));
        assert_eq!(wide, ATy::Top);
    }

    #[test]
    fn subsumes_is_coverage() {
        assert!(subsumes(&ATy::Top, &ATy::Int64));
        assert!(subsumes(&ATy::BigInt, &ATy::Int64));
        assert!(subsumes(
            &ATy::Union(vec![ATy::Int64, ATy::Str].into()),
            &ATy::Str
        ));
        assert!(!subsumes(&ATy::Int64, &ATy::Str));
    }

    #[test]
    fn int_operations_overflow_or_not() {
        let big = [
            BinOp::Add,
            BinOp::Sub,
            BinOp::Mul,
            BinOp::Pow,
            BinOp::LShift,
            BinOp::FloorDiv,
        ];
        for op in big {
            assert_eq!(binop_pair(op, &ATy::Int64, &ATy::Int64), ATy::BigInt);
        }
        let small = [
            BinOp::Mod,
            BinOp::RShift,
            BinOp::BitAnd,
            BinOp::BitOr,
            BinOp::BitXor,
        ];
        for op in small {
            assert_eq!(binop_pair(op, &ATy::BigInt, &ATy::Int64), ATy::Int64);
        }
        assert_eq!(
            binop_pair(BinOp::Div, &ATy::Int64, &ATy::Int64),
            ATy::Float64
        );
        assert_eq!(
            binop_pair(BinOp::MatMul, &ATy::Int64, &ATy::Int64),
            ATy::Bottom
        );
    }

    #[test]
    fn float_is_contagious() {
        for op in [BinOp::Add, BinOp::Mul, BinOp::Div, BinOp::Mod, BinOp::Pow] {
            assert_eq!(binop_pair(op, &ATy::Int64, &ATy::Float64), ATy::Float64);
        }
        assert_eq!(
            binop_pair(BinOp::BitAnd, &ATy::Int64, &ATy::Float64),
            ATy::Bottom
        );
    }

    #[test]
    fn container_algebra() {
        assert_eq!(binop_pair(BinOp::Add, &ATy::Str, &ATy::Str), ATy::Str);
        assert_eq!(binop_pair(BinOp::Mul, &ATy::Str, &ATy::Int64), ATy::Str);
        assert_eq!(binop_pair(BinOp::Mul, &ATy::Int64, &ATy::Str), ATy::Str);
        assert_eq!(binop_pair(BinOp::Mod, &ATy::Str, &ATy::Int64), ATy::Str);
        assert_eq!(binop_pair(BinOp::Add, &ATy::List, &ATy::List), ATy::List);
        assert_eq!(binop_pair(BinOp::Mul, &ATy::List, &ATy::Int64), ATy::List);
        assert_eq!(binop_pair(BinOp::BitOr, &ATy::Set, &ATy::Set), ATy::Set);
        assert_eq!(binop_pair(BinOp::Sub, &ATy::Set, &ATy::Set), ATy::Set);
        assert_eq!(
            binop_pair(
                BinOp::Add,
                &ATy::Tuple(vec![ATy::Int64].into()),
                &ATy::Tuple(vec![ATy::Str].into())
            ),
            ATy::Tuple(vec![ATy::Int64, ATy::Str].into())
        );
        // Undefined pairs contribute nothing.
        assert_eq!(binop_pair(BinOp::Add, &ATy::Str, &ATy::List), ATy::Bottom);
    }

    #[test]
    fn builtin_words_and_signatures() {
        assert_eq!(word_of(&ATy::Str), Some("str"));
        assert_eq!(word_of(&ATy::BigInt), Some("int"));
        assert_eq!(
            word_of(&ATy::Inst {
                cls: ClassRef {
                    module: 0,
                    class: coil_ir::types::ClassId(0)
                },
                exact: true
            }),
            None
        );
        assert_eq!(
            sig_result("len", &[ATy::Str]),
            Some(ATy::Int64),
            "len keeps its Int64: it never overflows"
        );
        assert_eq!(
            sig_result("math.factorial", &[ATy::Int64]),
            Some(ATy::BigInt),
            "an Int64 the operation may box reads as BigInt"
        );
    }
}

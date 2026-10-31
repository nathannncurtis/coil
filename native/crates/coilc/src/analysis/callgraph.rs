//! The whole-program call graph over the fixed module set (spec sections
//! 5, 6 and 7): for every reachable function, its callees and callers. A
//! site that can name its callee - a known function, a resolved method of a
//! sealed class, or a module body an import runs - gets a direct edge;
//! everything else gets a `Dynamic` edge carrying the name the site uses
//! when it has one (an attribute or method name, or a global's), which is
//! what the builtin signature table and devirtualisation caches key on.
//! The strongly connected components of the direct subgraph come out in a
//! stable callees-first order, the order a fixpoint pass over the graph
//! consumes.
//!
//! The scan is local and per function: it tracks the values a function's
//! instructions define - the functions, classes, modules and instances of
//! known classes - and reads the world's module namespaces for global
//! loads. `obj.m()` lowers to `GetAttr` + `Call`, so a method call is a
//! call whose callee is an attribute load on a value of known class.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use coil_ir::ir::{Const, Instr, Module};
use coil_ir::types::{ConstIdx, Value};

use crate::analysis::shapes::{self, Shapes};
use crate::build::relative_base;
use crate::world::reach::TopDef;
use crate::world::{ClassRef, FnRef, Program};

/// The builtins whose call site is reflective when the attribute name is
/// not a constant, or that read a class's members wholesale (spec section
/// 6).
const REFLECTIVE: [&str; 5] = ["dir", "getattr", "hasattr", "setattr", "vars"];

/// One call edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edge {
    /// The site names its callee: a known function, a resolved method of a
    /// sealed class, or a module body an import runs.
    Direct(FnRef),
    /// The site cannot pin its callee down, carrying the name it uses when
    /// it has one.
    Dynamic(Option<String>),
}

/// What a call site's callee resolved to in the scan. The decisions that
/// need the sealed classes and the MROs wait until every module's class
/// statements have been seen, so the scan records and `build` resolves.
#[derive(Clone, Debug)]
enum Callee {
    Fn(FnRef),
    Class(ClassRef),
    /// A module value: calling one always fails, so there is no callee.
    Module,
    /// An attribute load on a value of known class: `obj.name`.
    BoundMethod {
        cls: ClassRef,
        name: String,
    },
    /// Anything else, with the name the site loaded it by, if any.
    Name(Option<String>),
}

/// One call site: the function it sits in and what its callee resolved to.
struct Site {
    at: FnRef,
    callee: Callee,
}

/// The classes a value known to be an instance of them - a method's first
/// parameter, a constructor call's result - reached a reflective site in: a
/// `getattr`/`setattr`/`hasattr` whose name argument is not a constant,
/// `dir`/`vars` on the object, or a `__dict__` load (spec section 6). A
/// class flowing to such a site is never sealed.
pub fn reflective(program: &Program) -> BTreeSet<ClassRef> {
    let self_of = shapes::self_params(program);
    Scan::new(program).run(&self_of).reflective
}

/// Build the call graph of the reachable program. The shapes carry the
/// sealed classes; method calls on them resolve along the MRO.
pub fn build(program: &Program, shapes: &Shapes) -> CallGraph {
    let self_of = shapes.self_of_map().clone();
    let scan = Scan::new(program).run(&self_of);
    let mut edges: BTreeMap<FnRef, Vec<Edge>> =
        program.functions.iter().map(|&f| (f, Vec::new())).collect();
    let mut memo: HashMap<ClassRef, Option<Vec<ClassRef>>> = HashMap::new();
    for site in &scan.sites {
        let at = edges
            .get_mut(&site.at)
            .expect("sites sit in reachable functions");
        match &site.callee {
            Callee::Fn(f) => at.push(Edge::Direct(*f)),
            Callee::Module => {}
            Callee::Name(name) => at.push(Edge::Dynamic(name.clone())),
            Callee::BoundMethod { cls, name } => {
                at.push(method_edge(
                    program,
                    &scan.bases,
                    &mut memo,
                    shapes,
                    *cls,
                    name,
                ));
            }
            Callee::Class(cls) => {
                // A constructor call runs `__new__` and then `__init__`; on
                // a sealed class both resolve along the MRO. An unresolved
                // `__new__` is `object.__new__`, runtime code outside the
                // program, so it adds no edge; an unresolved `__init__`
                // stays a dynamic call.
                if shapes.is_sealed(*cls) {
                    if let Some(f) =
                        resolve_method(program, &scan.bases, &mut memo, *cls, "__new__")
                    {
                        at.push(Edge::Direct(f));
                    }
                    at.push(
                        match resolve_method(program, &scan.bases, &mut memo, *cls, "__init__") {
                            Some(f) => Edge::Direct(f),
                            None => Edge::Dynamic(Some("__init__".to_owned())),
                        },
                    );
                } else {
                    at.push(Edge::Dynamic(Some("__init__".to_owned())));
                }
            }
        }
    }
    let mut adjacency: BTreeMap<FnRef, BTreeSet<FnRef>> = program
        .functions
        .iter()
        .map(|&f| (f, BTreeSet::new()))
        .collect();
    let mut callers: BTreeMap<FnRef, BTreeSet<FnRef>> = program
        .functions
        .iter()
        .map(|&f| (f, BTreeSet::new()))
        .collect();
    for (fr, list) in &edges {
        for edge in list {
            if let Edge::Direct(target) = edge {
                adjacency.entry(*fr).or_default().insert(*target);
                callers.entry(*target).or_default().insert(*fr);
            }
        }
    }
    let sccs = strongly_connected(&program.functions, &adjacency);
    CallGraph {
        edges,
        callers: callers
            .into_iter()
            .map(|(f, set)| (f, set.into_iter().collect()))
            .collect(),
        sccs,
    }
}

/// The call graph of the reachable program.
pub struct CallGraph {
    /// Each function's out-edges, in instruction order.
    edges: BTreeMap<FnRef, Vec<Edge>>,
    /// The direct callers of each function, in `FnRef` order.
    callers: BTreeMap<FnRef, Vec<FnRef>>,
    sccs: Vec<Vec<FnRef>>,
}

impl CallGraph {
    /// A function's out-edges, in instruction order.
    pub fn callees(&self, fr: FnRef) -> &[Edge] {
        self.edges.get(&fr).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The functions that call `fr` directly, in `FnRef` order.
    pub fn callers(&self, fr: FnRef) -> &[FnRef] {
        self.callers.get(&fr).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The strongly connected components of the direct subgraph, in a
    /// stable callees-first order.
    pub fn sccs(&self) -> &[Vec<FnRef>] {
        &self.sccs
    }

    /// One line per reachable function - its label and out-edges - then one
    /// line per component.
    pub fn dump(&self, program: &Program) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for (fr, edges) in &self.edges {
            out.push_str(&label(program, *fr));
            out.push(':');
            if !edges.is_empty() {
                let parts: Vec<String> = edges
                    .iter()
                    .map(|edge| match edge {
                        Edge::Direct(f) => format!("direct {}", label(program, *f)),
                        Edge::Dynamic(Some(name)) => format!("dynamic '{name}'"),
                        Edge::Dynamic(None) => "dynamic".to_owned(),
                    })
                    .collect();
                out.push(' ');
                out.push_str(&parts.join(", "));
            }
            out.push('\n');
        }
        for (i, component) in self.sccs.iter().enumerate() {
            let names: Vec<String> = component.iter().map(|&f| label(program, f)).collect();
            let _ = writeln!(out, "scc {i}: {}", names.join(", "));
        }
        out
    }
}

/// The class hierarchy of the program: each class's resolved bases and C3
/// linearisation, computed the way `build` resolves its method edges. The
/// later analyses - inference's attribute walk and method routing (spec
/// section 7) - resolve against the same linearisation.
pub struct Hierarchy {
    mros: HashMap<ClassRef, Option<Vec<ClassRef>>>,
}

/// The class hierarchy: resolved bases from each class's `MakeClass` site,
/// then the C3 linearisation of every class in the program.
pub fn hierarchy(program: &Program) -> Hierarchy {
    let self_of = shapes::self_params(program);
    let scan = Scan::new(program).run(&self_of);
    let mut mros: HashMap<ClassRef, Option<Vec<ClassRef>>> = HashMap::new();
    for &cls in &program.classes {
        let mut visiting = BTreeSet::new();
        let linearized = mro(&scan.bases, cls, &mut mros, &mut visiting);
        mros.insert(cls, linearized);
    }
    Hierarchy { mros }
}

impl Hierarchy {
    /// A class's linearisation, the class first; `None` when a base is not a
    /// program class or the hierarchy is inconsistent.
    pub fn mro(&self, cls: ClassRef) -> Option<&[ClassRef]> {
        self.mros.get(&cls)?.as_deref()
    }

    /// The function a method call of `name` on an instance of `cls` runs: the
    /// first definition along the MRO. Sealing is the caller's concern - an
    /// unsealed class's instances may run a subclass override instead.
    pub fn method(&self, program: &Program, cls: ClassRef, name: &str) -> Option<FnRef> {
        for cr in self.mro(cls)? {
            let ir = program.modules[cr.module as usize]
                .ir
                .as_ref()
                .expect("classes only exist in compiled modules");
            for (method, fid) in &ir.classes[cr.class.0 as usize].methods {
                if ir.consts.sym_str(*method) == name {
                    return Some(FnRef {
                        module: cr.module,
                        function: *fid,
                    });
                }
            }
        }
        None
    }
}

/// `<module>.<qualname>`, the dump's name for a function.
fn label(program: &Program, fr: FnRef) -> String {
    let state = &program.modules[fr.module as usize];
    let ir = state.ir.as_ref().expect("reachable functions are compiled");
    let qualname = ir
        .consts
        .sym_str(ir.functions[fr.function.0 as usize].qualname);
    format!("{}.{}", state.name, qualname)
}

/// The edge for a method call `name` on a known class: direct along the
/// MRO when the class is sealed, dynamic carrying the name otherwise.
fn method_edge(
    program: &Program,
    bases: &BTreeMap<ClassRef, Vec<Option<ClassRef>>>,
    memo: &mut HashMap<ClassRef, Option<Vec<ClassRef>>>,
    shapes: &Shapes,
    cls: ClassRef,
    name: &str,
) -> Edge {
    if !shapes.is_sealed(cls) {
        return Edge::Dynamic(Some(name.to_owned()));
    }
    match resolve_method(program, bases, memo, cls, name) {
        Some(f) => Edge::Direct(f),
        None => Edge::Dynamic(Some(name.to_owned())),
    }
}

/// The function a call of `name` on an instance of `cls` runs: the first
/// definition along the MRO.
fn resolve_method(
    program: &Program,
    bases: &BTreeMap<ClassRef, Vec<Option<ClassRef>>>,
    memo: &mut HashMap<ClassRef, Option<Vec<ClassRef>>>,
    cls: ClassRef,
    name: &str,
) -> Option<FnRef> {
    let mut visiting = BTreeSet::new();
    for cr in mro(bases, cls, memo, &mut visiting)? {
        let ir = program.modules[cr.module as usize]
            .ir
            .as_ref()
            .expect("classes only exist in compiled modules");
        for (method, fid) in &ir.classes[cr.class.0 as usize].methods {
            if ir.consts.sym_str(*method) == name {
                return Some(FnRef {
                    module: cr.module,
                    function: *fid,
                });
            }
        }
    }
    None
}

/// A class's method resolution order, C3 as CPython computes it. `None`
/// when a base does not resolve to a program class or the linearisation is
/// inconsistent - the runtime would have refused the class statement, so no
/// method of the class resolves.
fn mro(
    bases: &BTreeMap<ClassRef, Vec<Option<ClassRef>>>,
    cls: ClassRef,
    memo: &mut HashMap<ClassRef, Option<Vec<ClassRef>>>,
    visiting: &mut BTreeSet<ClassRef>,
) -> Option<Vec<ClassRef>> {
    if let Some(cached) = memo.get(&cls) {
        return cached.clone();
    }
    if !visiting.insert(cls) {
        // A base cycle: no linearisation.
        return None;
    }
    let empty = Vec::new();
    let base_list = bases.get(&cls).unwrap_or(&empty);
    let mut seqs: Vec<Vec<ClassRef>> = Vec::with_capacity(base_list.len() + 1);
    let mut tail: Vec<ClassRef> = Vec::with_capacity(base_list.len());
    let mut ok = true;
    for base in base_list {
        match base {
            Some(base) => match mro(bases, *base, memo, visiting) {
                Some(linearized) => {
                    seqs.push(linearized);
                    tail.push(*base);
                }
                None => {
                    ok = false;
                    break;
                }
            },
            None => {
                ok = false;
                break;
            }
        }
    }
    let mut out = None;
    if ok {
        seqs.push(tail);
        if let Some(mut merged) = merge(&mut seqs) {
            merged.insert(0, cls);
            out = Some(merged);
        }
    }
    visiting.remove(&cls);
    memo.insert(cls, out.clone());
    out
}

/// C3's merge: repeatedly take a head that appears in no other sequence's
/// tail. `None` marks an inconsistent hierarchy.
fn merge(seqs: &mut Vec<Vec<ClassRef>>) -> Option<Vec<ClassRef>> {
    let mut out = Vec::new();
    loop {
        seqs.retain(|s| !s.is_empty());
        if seqs.is_empty() {
            return Some(out);
        }
        let chosen = (0..seqs.len()).find(|&i| {
            let head = seqs[i][0];
            !seqs.iter().any(|s| s[1..].contains(&head))
        });
        let i = chosen?;
        let head = seqs[i][0];
        for s in seqs.iter_mut() {
            if s[0] == head {
                s.remove(0);
            }
        }
        out.push(head);
    }
}

/// The strongly connected components of the direct subgraph, by iterative
/// Tarjan. Roots and children walk in `FnRef` order and members come out
/// sorted, so the order is stable; a component is emitted only after
/// everything it can reach, so callees come before callers.
fn strongly_connected(
    nodes: &BTreeSet<FnRef>,
    adj: &BTreeMap<FnRef, BTreeSet<FnRef>>,
) -> Vec<Vec<FnRef>> {
    let empty = BTreeSet::new();
    let mut order: HashMap<FnRef, u32> = HashMap::new();
    let mut low: HashMap<FnRef, u32> = HashMap::new();
    let mut stack: Vec<FnRef> = Vec::new();
    let mut on_stack: HashSet<FnRef> = HashSet::new();
    let mut out: Vec<Vec<FnRef>> = Vec::new();
    let mut next = 0u32;
    for &root in nodes {
        if order.contains_key(&root) {
            continue;
        }
        order.insert(root, next);
        low.insert(root, next);
        next += 1;
        stack.push(root);
        on_stack.insert(root);
        let children: Vec<FnRef> = adj.get(&root).unwrap_or(&empty).iter().copied().collect();
        // Call frames of the depth-first walk: (node, its children, the
        // next child position).
        let mut frames: Vec<(FnRef, Vec<FnRef>, usize)> = vec![(root, children, 0)];
        let mut at = 0usize;
        while at < frames.len() {
            let node = frames[at].0;
            let mut descended: Option<FnRef> = None;
            while frames[at].2 < frames[at].1.len() {
                let child = frames[at].1[frames[at].2];
                frames[at].2 += 1;
                if !order.contains_key(&child) {
                    descended = Some(child);
                    break;
                }
                if on_stack.contains(&child) {
                    let merged = low[&node].min(order[&child]);
                    low.insert(node, merged);
                }
            }
            if let Some(child) = descended {
                order.insert(child, next);
                low.insert(child, next);
                next += 1;
                stack.push(child);
                on_stack.insert(child);
                let grandchildren: Vec<FnRef> =
                    adj.get(&child).unwrap_or(&empty).iter().copied().collect();
                frames.push((child, grandchildren, 0));
                at += 1;
                continue;
            }
            // The frame is finished: fold the low link into the parent.
            if at > 0 {
                let parent = frames[at - 1].0;
                let merged = low[&parent].min(low[&node]);
                low.insert(parent, merged);
            }
            if low[&node] == order[&node] {
                let mut component = Vec::new();
                while let Some(top) = stack.pop() {
                    on_stack.remove(&top);
                    component.push(top);
                    if top == node {
                        break;
                    }
                }
                component.sort();
                out.push(component);
            }
            frames.pop();
            at = at.saturating_sub(1);
        }
    }
    out
}

/// What one function's scan knows about its values.
#[derive(Default)]
struct Locals {
    consts: HashMap<Value, ConstIdx>,
    /// Values holding a known function.
    fn_of: HashMap<Value, FnRef>,
    /// Values holding a known class.
    cls_of: HashMap<Value, ClassRef>,
    /// Values holding a known module.
    mod_of: HashMap<Value, u32>,
    /// Values holding an instance of a known class: a constructor call's
    /// result, a method's first parameter, an allocation.
    inst_of: HashMap<Value, ClassRef>,
    /// Attribute loads on known-class instances: bound methods.
    method_of: HashMap<Value, (ClassRef, String)>,
    /// Values holding one of the reflective builtins.
    builtin_of: HashMap<Value, &'static str>,
    /// The name a value was loaded by, when it has one.
    name_of: HashMap<Value, String>,
    /// This function's name stores, resolved; an empty list is a store of a
    /// value the scan could not resolve, which poisons the name.
    defs: HashMap<String, Vec<TopDef>>,
}

/// The per-function scan, collecting call sites, class bases and reflective
/// classes across the whole program.
struct Scan<'a> {
    program: &'a Program,
    by_name: HashMap<&'a str, u32>,
    reflective: BTreeSet<ClassRef>,
    /// Each class's resolved bases, from its `MakeClass` site; `None` where
    /// a base value does not statically name a program class.
    bases: BTreeMap<ClassRef, Vec<Option<ClassRef>>>,
    sites: Vec<Site>,
}

impl<'a> Scan<'a> {
    fn new(program: &'a Program) -> Scan<'a> {
        let by_name = program
            .modules
            .iter()
            .enumerate()
            .map(|(i, state)| (state.name.as_str(), i as u32))
            .collect();
        Scan {
            program,
            by_name,
            reflective: BTreeSet::new(),
            bases: BTreeMap::new(),
            sites: Vec::new(),
        }
    }

    fn run(mut self, self_of: &BTreeMap<FnRef, ClassRef>) -> Scan<'a> {
        for fr in &self.program.functions {
            self.scan_fn(*fr, self_of);
        }
        self
    }

    /// The function that runs a module's body, for import edges.
    fn body_of(&self, idx: u32) -> Option<FnRef> {
        self.program.modules[idx as usize]
            .ir
            .as_ref()
            .map(|ir| FnRef {
                module: idx,
                function: ir.body,
            })
    }

    fn scan_fn(&mut self, fr: FnRef, self_of: &BTreeMap<FnRef, ClassRef>) {
        let Some(ir) = self.program.modules[fr.module as usize].ir.as_ref() else {
            return;
        };
        let mut locals = Locals::default();
        if let Some(&cls) = self_of.get(&fr) {
            locals.inst_of.insert(Value(0), cls);
        }
        let function = &ir.functions[fr.function.0 as usize];
        for block in &function.blocks {
            for instr in &block.instrs {
                self.instr(fr, ir, instr, &mut locals);
            }
        }
    }

    fn instr(&mut self, fr: FnRef, ir: &Module, instr: &Instr, l: &mut Locals) {
        match instr {
            Instr::Const { idx, into } => {
                l.consts.insert(*into, *idx);
            }
            Instr::Copy { src, into } => {
                copy_known(&mut l.consts, *src, *into);
                copy_known(&mut l.fn_of, *src, *into);
                copy_known(&mut l.cls_of, *src, *into);
                copy_known(&mut l.mod_of, *src, *into);
                copy_known(&mut l.inst_of, *src, *into);
                copy_known(&mut l.method_of, *src, *into);
                copy_known(&mut l.builtin_of, *src, *into);
                copy_known(&mut l.name_of, *src, *into);
            }
            Instr::Import {
                name,
                fromlist,
                level,
                into,
            } => {
                // The import runs the whole package chain's bodies; a plain
                // import binds the top-level package, a from-import the
                // target (spec section 6).
                let level = const_int(ir, &l.consts, level);
                let fromlist = const_fromlist(ir, &l.consts, fromlist);
                if let Some((chain, bound)) =
                    self.import_chain(fr.module, ir.consts.sym_str(*name), fromlist, level)
                {
                    for idx in chain {
                        if let Some(body) = self.body_of(idx) {
                            self.sites.push(Site {
                                at: fr,
                                callee: Callee::Fn(body),
                            });
                        }
                    }
                    if let Some(bound) = bound {
                        l.mod_of.insert(*into, bound);
                    }
                }
            }
            Instr::GetAttr { obj, name, into } => {
                let attr = ir.consts.sym_str(*name);
                if let Some(&m) = l.mod_of.get(obj) {
                    let full = format!("{}.{}", self.program.modules[m as usize].name, attr);
                    if let Some(&sub) = self.by_name.get(full.as_str()) {
                        l.mod_of.insert(*into, sub);
                    } else if let Some(defs) = self.program.namespaces[m as usize].defs.get(attr) {
                        bind_single(defs, l, *into);
                    }
                } else if let Some(&cls) = l.inst_of.get(obj) {
                    if attr == "__dict__" {
                        self.reflective.insert(cls);
                    }
                    l.method_of.insert(*into, (cls, attr.to_owned()));
                }
                l.name_of.insert(*into, attr.to_owned());
            }
            Instr::LoadName { name, into } | Instr::LoadGlobal { name, into } => {
                let name = ir.consts.sym_str(*name);
                // A store this function performed is the local truth for
                // the name; otherwise the module's namespace answers.
                let defs = l.defs.get(name).cloned().or_else(|| {
                    self.program.namespaces[fr.module as usize]
                        .defs
                        .get(name)
                        .cloned()
                });
                match defs {
                    Some(defs) => bind_single(&defs, l, *into),
                    None => {
                        if let Some(builtin) = REFLECTIVE.iter().find(|r| **r == name) {
                            l.builtin_of.insert(*into, builtin);
                        }
                    }
                }
                l.name_of.insert(*into, name.to_owned());
            }
            Instr::StoreName { name, value } | Instr::StoreGlobal { name, value } => {
                let mut bound = Vec::new();
                if let Some(f) = l.fn_of.get(value) {
                    bound.push(TopDef::Function(*f));
                }
                if let Some(cls) = l.cls_of.get(value) {
                    bound.push(TopDef::Class(*cls));
                }
                if let Some(m) = l.mod_of.get(value) {
                    bound.push(TopDef::Module(*m));
                }
                l.defs.insert(ir.consts.sym_str(*name).to_owned(), bound);
            }
            Instr::MakeFunction { func, into, .. } => {
                l.fn_of.insert(
                    *into,
                    FnRef {
                        module: fr.module,
                        function: *func,
                    },
                );
            }
            Instr::MakeClass {
                desc, bases, into, ..
            } => {
                let cls = ClassRef {
                    module: fr.module,
                    class: *desc,
                };
                l.cls_of.insert(*into, cls);
                let resolved = bases
                    .iter()
                    .map(|base| l.cls_of.get(base).copied())
                    .collect();
                self.bases.insert(cls, resolved);
            }
            Instr::AllocInst { cls, into, .. } | Instr::StackAlloc { cls, into, .. } => {
                l.inst_of.insert(
                    *into,
                    ClassRef {
                        module: fr.module,
                        class: *cls,
                    },
                );
            }
            Instr::Call {
                callee, args, into, ..
            } => {
                self.call_site(fr, ir, l, *callee, args, Some(*into));
            }
            Instr::CallEx { callee, into, .. } => {
                // The argument list is built at runtime, so no argument type
                // or reflective check can see it.
                self.call_site(fr, ir, l, *callee, &[], Some(*into));
            }
            Instr::CallMethod {
                name, recv, into, ..
            } => {
                let name = ir.consts.sym_str(*name);
                let callee = if let Some(&m) = l.mod_of.get(recv) {
                    self.program.namespaces[m as usize]
                        .defs
                        .get(name)
                        .map(|defs| defs.as_slice())
                        .and_then(|defs| match defs {
                            [TopDef::Function(f)] => Some(Callee::Fn(*f)),
                            [TopDef::Class(cls)] => {
                                l.inst_of.insert(*into, *cls);
                                Some(Callee::Class(*cls))
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| Callee::Name(Some(name.to_owned())))
                } else if let Some(&cls) = l.inst_of.get(recv) {
                    Callee::BoundMethod {
                        cls,
                        name: name.to_owned(),
                    }
                } else {
                    Callee::Name(Some(name.to_owned()))
                };
                self.sites.push(Site { at: fr, callee });
            }
            Instr::CallDirect { func, .. } => {
                self.sites.push(Site {
                    at: fr,
                    callee: Callee::Fn(FnRef {
                        module: fr.module,
                        function: *func,
                    }),
                });
            }
            _ => {}
        }
    }

    /// One call site through the `callee` value: the reflective marks first,
    /// then the callee's local resolution. A constructor call's result is an
    /// instance of the class whether or not the class is sealed; sealing is
    /// read when the edges resolve.
    fn call_site(
        &mut self,
        fr: FnRef,
        ir: &Module,
        l: &mut Locals,
        callee: Value,
        args: &[Value],
        into: Option<Value>,
    ) {
        if let Some(builtin) = l.builtin_of.get(&callee).copied() {
            let reflective = match builtin {
                "getattr" | "setattr" | "hasattr" => {
                    args.len() < 2 || const_str(ir, &l.consts, &args[1]).is_none()
                }
                _ => true,
            };
            if reflective
                && !args.is_empty()
                && let Some(&cls) = l.inst_of.get(&args[0])
            {
                self.reflective.insert(cls);
            }
        }
        let callee = if let Some(f) = l.fn_of.get(&callee) {
            Callee::Fn(*f)
        } else if let Some(&cls) = l.cls_of.get(&callee) {
            if let Some(into) = into {
                l.inst_of.insert(into, cls);
            }
            Callee::Class(cls)
        } else if l.mod_of.contains_key(&callee) {
            Callee::Module
        } else if let Some((cls, name)) = l.method_of.get(&callee) {
            Callee::BoundMethod {
                cls: *cls,
                name: name.clone(),
            }
        } else {
            Callee::Name(l.name_of.get(&callee).cloned())
        };
        self.sites.push(Site { at: fr, callee });
    }

    /// The modules an import runs, deepest first, and the one it binds.
    fn import_chain(
        &self,
        importer: u32,
        name: &str,
        fromlist: Option<Vec<String>>,
        level: Option<i64>,
    ) -> Option<(Vec<u32>, Option<u32>)> {
        let level = level?;
        let state = &self.program.modules[importer as usize];
        let target = if level == 0 {
            name.to_owned()
        } else {
            let base = relative_base(&state.name, state.is_package, i32::try_from(level).ok()?)?;
            if name.is_empty() {
                base
            } else {
                format!("{base}.{name}")
            }
        };
        let importer_name = &self.program.modules[importer as usize].name;
        let mut chain = Vec::new();
        let mut parts: Vec<&str> = target.split('.').collect();
        while !parts.is_empty() {
            let joined = parts.join(".");
            if let Some(&idx) = self.by_name.get(joined.as_str()) {
                // The importer's own body is running, and an ancestor of the
                // importer finished before it started: neither re-runs.
                let name = &self.program.modules[idx as usize].name;
                let already_run = importer == idx || importer_name.starts_with(&format!("{name}."));
                if !already_run {
                    chain.push(idx);
                }
            }
            parts.pop();
        }
        let bound = match fromlist {
            None => self
                .by_name
                .get(target.split('.').next().unwrap_or(""))
                .copied(),
            Some(_) => self.by_name.get(target.as_str()).copied(),
        };
        Some((chain, bound))
    }
}

/// Carry one piece of value knowledge across a `Copy`.
fn copy_known<T: Clone>(map: &mut HashMap<Value, T>, src: Value, into: Value) {
    if let Some(known) = map.get(&src).cloned() {
        map.insert(into, known);
    }
}

/// Bind a value loaded to a single resolved definition.
fn bind_single(defs: &[TopDef], l: &mut Locals, into: Value) {
    if let [def] = defs {
        match def {
            TopDef::Function(f) => {
                l.fn_of.insert(into, *f);
            }
            TopDef::Class(cls) => {
                l.cls_of.insert(into, *cls);
            }
            TopDef::Module(m) => {
                l.mod_of.insert(into, *m);
            }
        }
    }
}

/// The integer a constant operand lowers to, if it is one.
fn const_int(ir: &Module, consts: &HashMap<Value, ConstIdx>, value: &Value) -> Option<i64> {
    match &ir.consts.consts[consts.get(value)?.0 as usize] {
        Const::Int(i) => Some(*i),
        _ => None,
    }
}

/// The string a constant operand lowers to, if it is one.
fn const_str<'a>(
    ir: &'a Module,
    consts: &HashMap<Value, ConstIdx>,
    value: &Value,
) -> Option<&'a str> {
    match &ir.consts.consts[consts.get(value)?.0 as usize] {
        Const::Str(sym) => Some(ir.consts.sym_str(*sym)),
        _ => None,
    }
}

/// The names of a from-list: `None` for a plain import, the names otherwise
/// (`*` included).
fn const_fromlist(
    ir: &Module,
    consts: &HashMap<Value, ConstIdx>,
    value: &Value,
) -> Option<Vec<String>> {
    match &ir.consts.consts[consts.get(value)?.0 as usize] {
        Const::None => None,
        Const::Tuple(items) => Some(
            items
                .iter()
                .map(|item| match &ir.consts.consts[item.0 as usize] {
                    Const::Str(sym) => ir.consts.sym_str(*sym).to_owned(),
                    _ => String::new(),
                })
                .collect(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use coil_ir::types::{ClassId, FnId};

    fn node(i: u32) -> FnRef {
        FnRef {
            module: 0,
            function: FnId(i),
        }
    }

    fn graph(edges: &[(u32, u32)]) -> (BTreeSet<FnRef>, BTreeMap<FnRef, BTreeSet<FnRef>>) {
        let nodes: BTreeSet<FnRef> = edges
            .iter()
            .flat_map(|(a, b)| [node(*a), node(*b)])
            .collect();
        let mut adj: BTreeMap<FnRef, BTreeSet<FnRef>> =
            nodes.iter().map(|&n| (n, BTreeSet::new())).collect();
        for (a, b) in edges {
            adj.entry(node(*a)).or_default().insert(node(*b));
        }
        (nodes, adj)
    }

    fn labels(components: &[Vec<FnRef>]) -> Vec<Vec<u32>> {
        components
            .iter()
            .map(|c| c.iter().map(|f| f.function.0).collect())
            .collect()
    }

    #[test]
    fn cycles_come_out_as_one_component() {
        let (nodes, adj) = graph(&[(0, 1), (1, 2), (2, 0), (3, 0)]);
        assert_eq!(
            labels(&strongly_connected(&nodes, &adj)),
            [vec![0, 1, 2], vec![3]],
            "the cycle is one component, members sorted, callees first"
        );
    }

    #[test]
    fn every_node_without_edges_is_its_own_component() {
        let (nodes, adj) = graph(&[(0, 0)]);
        assert_eq!(labels(&strongly_connected(&nodes, &adj)), [vec![0]]);
        let (nodes, adj) = graph(&[]);
        assert!(strongly_connected(&nodes, &adj).is_empty());
    }

    #[test]
    fn nested_cycles_merge_into_the_outer() {
        // a -> b -> c -> a and c -> d -> c: one component of all four.
        let (nodes, adj) = graph(&[(0, 1), (1, 2), (2, 0), (2, 3), (3, 2)]);
        assert_eq!(
            labels(&strongly_connected(&nodes, &adj)),
            [vec![0, 1, 2, 3]]
        );
    }

    #[test]
    fn merge_picks_the_diamond_order() {
        // The bases' own linearisations (class 9 stands in for `object`);
        // the class itself is prepended by `mro`, not by the merge.
        let mut seqs = vec![
            vec![class(1), class(2), class(9)],
            vec![class(3), class(2), class(9)],
            vec![class(1), class(3)],
        ];
        assert_eq!(
            merge(&mut seqs),
            Some(vec![class(1), class(3), class(2), class(9)])
        );
        // A class appearing only in tails cannot be picked: no order.
        let mut inconsistent = vec![vec![class(1), class(2)], vec![class(2), class(1)]];
        assert_eq!(merge(&mut inconsistent), None);
    }

    fn class(i: u32) -> ClassRef {
        ClassRef {
            module: 0,
            class: ClassId(i),
        }
    }
}

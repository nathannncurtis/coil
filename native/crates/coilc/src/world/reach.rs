//! Reachability: the set of modules, functions and classes a closed-world
//! build compiles, grown from the entry modules until it stops growing
//! (spec section 6). A function called only through `getattr(obj, "name")`
//! with a constant name is reached; one found through a non-constant name is
//! not, unless a keep rule says so.
//!
//! The walk starts at the entry and kept modules' bodies. An import executes
//! the target's body, a module attribute load or a constant-name `getattr`
//! resolves against the module's namespace, and `MakeFunction`/`MakeClass`
//! reach the definitions they create. Keep rules add the modules they name
//! and mark the members they keep.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use coil_ir::ir::{Const, Instr, Module};
use coil_ir::types::{ConstIdx, FnId, Value};

use crate::build::Compiled;

use super::keep;
use super::{ClassRef, FnRef, KeepMarks, ModuleState, Program};

/// A top-level binding of a module namespace: the module's own definitions,
/// re-exported imports, and module aliases.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TopDef {
    Function(FnRef),
    Class(ClassRef),
    Module(u32),
}

/// The top-level namespace of one module: every name its body stores, in
/// store order, with re-exported names resolved through the namespaces of
/// the modules it imports.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
struct Namespace {
    defs: BTreeMap<String, Vec<TopDef>>,
}

/// What one `MakeClass` records about a class: its resolved bases (`None`
/// where the base value does not statically name a class of the program) and
/// whether the class statement passed no keyword arguments, so the metaclass
/// can only be `type` if every base's is.
#[derive(Clone, Debug)]
struct ClassInfo {
    bases: Vec<Option<ClassRef>>,
    plain_kwds: bool,
}

/// Refine the front end's compiled module set into a [`Program`]: add the
/// modules the keep rules name, grow the reachable set, apply the keep marks
/// and pick the sealing candidates.
pub fn refine(
    rules: &keep::Rules,
    entries: &[String],
    kept: &BTreeMap<String, String>,
    compiled: Vec<Compiled>,
) -> Program {
    let mut modules = compiled;
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    let by_name: HashMap<&str, u32> = modules
        .iter()
        .enumerate()
        .map(|(i, module)| (module.name.as_str(), i as u32))
        .collect();
    let namespaces = compute_namespaces(&modules, &by_name);
    let mut walk = Walk {
        modules: &modules,
        by_name,
        namespaces,
        functions: BTreeSet::new(),
        classes: BTreeSet::new(),
        reached: BTreeSet::new(),
        class_info: HashMap::new(),
        queue: VecDeque::new(),
    };
    for name in entries
        .iter()
        .map(String::as_str)
        .chain(kept.keys().map(String::as_str))
    {
        if let Some(&idx) = walk.by_name.get(name) {
            walk.import_module(idx);
        }
    }
    walk.run();

    let keep_marks = keep_marks(rules, &modules, &walk);
    let seal_candidates = seal_candidates(&modules, &walk, &keep_marks);

    // The program keeps only the modules whose bodies can run, renumbered in
    // sorted order.
    let mut index: HashMap<u32, u32> = HashMap::new();
    let mut program_modules = Vec::new();
    for (old, module) in modules.iter().enumerate() {
        if walk.reached.contains(&(old as u32)) {
            index.insert(old as u32, program_modules.len() as u32);
            program_modules.push(ModuleState {
                name: module.name.clone(),
                file: module.file.clone(),
                kind: module.kind,
                is_package: module.is_package,
                kept: rules.keeps_module(&module.name),
                ir: module.module.clone(),
            });
        }
    }
    let remap_fn = |fr: &FnRef| FnRef {
        module: index[&fr.module],
        function: fr.function,
    };
    let remap_cls = |cr: &ClassRef| ClassRef {
        module: index[&cr.module],
        class: cr.class,
    };
    Program {
        modules: program_modules,
        functions: walk.functions.iter().map(remap_fn).collect(),
        classes: walk.classes.iter().map(remap_cls).collect(),
        keep: KeepMarks {
            modules: keep_marks.modules.iter().map(|m| index[m]).collect(),
            functions: keep_marks.functions.iter().map(remap_fn).collect(),
            classes: keep_marks.classes.iter().map(remap_cls).collect(),
        },
        seal_candidates: seal_candidates.iter().map(remap_cls).collect(),
    }
}

/// The keep marks over the reached set: module rules pin whole modules, and
/// member rules pin top-level functions and classes of modules that ended up
/// in the program.
fn keep_marks(rules: &keep::Rules, modules: &[Compiled], walk: &Walk) -> KeepMarks {
    let mut marks = KeepMarks::default();
    for idx in &walk.reached {
        let name = &modules[*idx as usize].name;
        if rules.keeps_module(name) {
            marks.modules.insert(*idx);
        }
        for member in rules.member_keeps(name) {
            let Some(defs) = walk.namespaces[*idx as usize]
                .as_ref()
                .and_then(|ns| ns.defs.get(member))
            else {
                continue;
            };
            for def in defs {
                match def {
                    TopDef::Function(fr) => {
                        marks.functions.insert(*fr);
                    }
                    TopDef::Class(cr) => {
                        marks.classes.insert(*cr);
                    }
                    TopDef::Module(_) => {}
                }
            }
        }
    }
    marks
}

/// Reachable classes a build may seal: not kept, a provably plain metaclass,
/// and no reachable subclass overriding a member (spec section 6).
fn seal_candidates(modules: &[Compiled], walk: &Walk, keep: &KeepMarks) -> BTreeSet<ClassRef> {
    let mut ancestors: HashMap<ClassRef, BTreeSet<ClassRef>> = HashMap::new();
    for &cref in &walk.classes {
        base_closure(walk, cref, &mut ancestors, &mut BTreeSet::new());
    }
    let mut plain: HashMap<ClassRef, bool> = HashMap::new();
    let mut candidates = BTreeSet::new();
    for &cref in &walk.classes {
        // Kept classes are never sealed, and neither are a kept module's:
        // the rule promises no sealing (spec section 6).
        if keep.classes.contains(&cref) || keep.modules.contains(&cref.module) {
            continue;
        }
        if !plain_metaclass(walk, cref, &mut plain, &mut BTreeSet::new()) {
            continue;
        }
        let overridden = walk.classes.iter().any(|&sub| {
            sub != cref
                && ancestors
                    .get(&sub)
                    .is_some_and(|bases| bases.contains(&cref))
                && !member_names(modules, sub).is_disjoint(&member_names(modules, cref))
        });
        if !overridden {
            candidates.insert(cref);
        }
    }
    candidates
}

/// The members (own methods) a class defines, for override checks.
fn member_names(modules: &[Compiled], cr: ClassRef) -> BTreeSet<String> {
    modules[cr.module as usize]
        .module
        .as_ref()
        .map_or_else(BTreeSet::new, |m| {
            m.classes[cr.class.0 as usize]
                .methods
                .iter()
                .map(|(name, _)| m.consts.sym_str(*name).to_owned())
                .collect()
        })
}

/// Whether the class statement provably produced `type`: no keyword
/// arguments and every base itself provably plain, so no metaclass can leak
/// in through a base. An unresolvable base value counts as unknown.
fn plain_metaclass(
    walk: &Walk,
    cref: ClassRef,
    memo: &mut HashMap<ClassRef, bool>,
    visiting: &mut BTreeSet<ClassRef>,
) -> bool {
    if let Some(&cached) = memo.get(&cref) {
        return cached;
    }
    if !visiting.insert(cref) {
        // An inheritance cycle: not provably plain.
        return false;
    }
    let answer = walk.class_info.get(&cref).is_some_and(|info| {
        info.plain_kwds
            && info
                .bases
                .iter()
                .all(|base| base.is_some_and(|base| plain_metaclass(walk, base, memo, visiting)))
    });
    visiting.remove(&cref);
    memo.insert(cref, answer);
    answer
}

/// Every class the class inherits from, resolved or not, transitively.
fn base_closure(
    walk: &Walk,
    cref: ClassRef,
    memo: &mut HashMap<ClassRef, BTreeSet<ClassRef>>,
    visiting: &mut BTreeSet<ClassRef>,
) -> BTreeSet<ClassRef> {
    if let Some(cached) = memo.get(&cref) {
        return cached.clone();
    }
    if !visiting.insert(cref) {
        return BTreeSet::new();
    }
    let mut closure = BTreeSet::new();
    if let Some(info) = walk.class_info.get(&cref) {
        for base in info.bases.iter().flatten() {
            closure.insert(*base);
            closure.extend(base_closure(walk, *base, memo, visiting));
        }
    }
    visiting.remove(&cref);
    memo.insert(cref, closure.clone());
    closure
}

/// The reachable-set walk.
struct Walk<'a> {
    modules: &'a [Compiled],
    by_name: HashMap<&'a str, u32>,
    namespaces: Vec<Option<Namespace>>,
    functions: BTreeSet<FnRef>,
    classes: BTreeSet<ClassRef>,
    reached: BTreeSet<u32>,
    class_info: HashMap<ClassRef, ClassInfo>,
    queue: VecDeque<FnRef>,
}

impl Walk<'_> {
    fn run(&mut self) {
        while let Some(fr) = self.queue.pop_front() {
            self.scan_fn(fr);
        }
    }

    /// Import a module: its body executes.
    fn import_module(&mut self, idx: u32) {
        self.reached.insert(idx);
        let Some(module) = &self.modules[idx as usize].module else {
            return;
        };
        self.reach_fn(FnRef {
            module: idx,
            function: module.body,
        });
    }

    /// Import a module and every ancestor package still in the set: all of
    /// their bodies run.
    fn import_chain(&mut self, target: &str) {
        let mut parts: Vec<&str> = target.split('.').collect();
        while !parts.is_empty() {
            let name = parts.join(".");
            if let Some(&idx) = self.by_name.get(name.as_str()) {
                self.import_module(idx);
            }
            parts.pop();
        }
    }

    fn reach_fn(&mut self, fr: FnRef) {
        if self.functions.insert(fr) {
            self.queue.push_back(fr);
        }
    }

    /// Construct a class: its body and every member run.
    fn reach_class(&mut self, cref: ClassRef) {
        if !self.classes.insert(cref) {
            return;
        }
        let desc = &self.modules[cref.module as usize]
            .module
            .as_ref()
            .expect("classes only exist in compiled modules")
            .classes[cref.class.0 as usize];
        let body = FnRef {
            module: cref.module,
            function: desc.body,
        };
        self.reach_fn(body);
        for (_, function) in &desc.methods {
            self.reach_fn(FnRef {
                module: cref.module,
                function: *function,
            });
        }
    }

    fn reach_def(&mut self, def: &TopDef) {
        match *def {
            TopDef::Function(fr) => self.reach_fn(fr),
            TopDef::Class(cr) => self.reach_class(cr),
            TopDef::Module(idx) => self.import_module(idx),
        }
    }

    /// Resolve an import instruction to its absolute target module name.
    fn import_target(&self, importer: u32, module: &str, level: Option<i64>) -> Option<String> {
        let level = level?;
        if level == 0 {
            return Some(module.to_owned());
        }
        let importer = &self.modules[importer as usize];
        let base = crate::build::relative_base(
            &importer.name,
            importer.is_package,
            i32::try_from(level).ok()?,
        )?;
        if module.is_empty() {
            Some(base)
        } else {
            Some(format!("{base}.{module}"))
        }
    }

    /// An attribute load (or constant-name `getattr`) on a module: a
    /// submodule's attribute exists once the submodule is imported, so its
    /// body runs; otherwise the name resolves against the module's
    /// namespace. `into` is marked where the resolution is provably single.
    fn module_attr(
        &mut self,
        idx: u32,
        attr: &str,
        into: Value,
        module_of: &mut HashMap<Value, u32>,
        class_of: &mut HashMap<Value, ClassRef>,
    ) {
        let full = format!("{}.{}", self.modules[idx as usize].name, attr);
        if let Some(&sub) = self.by_name.get(full.as_str()) {
            module_of.insert(into, sub);
            self.import_module(sub);
            return;
        }
        let Some(defs) = self.namespaces[idx as usize]
            .as_ref()
            .and_then(|ns| ns.defs.get(attr).cloned())
        else {
            return;
        };
        for def in &defs {
            self.reach_def(def);
        }
        if let [TopDef::Class(cref)] = defs.as_slice() {
            class_of.insert(into, *cref);
        }
    }

    fn scan_fn(&mut self, fr: FnRef) {
        let Some(module) = &self.modules[fr.module as usize].module else {
            return;
        };
        let function = &module.functions[fr.function.0 as usize];
        let mut consts: HashMap<Value, ConstIdx> = HashMap::new();
        let mut module_of: HashMap<Value, u32> = HashMap::new();
        let mut class_of: HashMap<Value, ClassRef> = HashMap::new();
        // Values holding the `getattr` builtin: loaded by name and not
        // shadowed by a member of the module.
        let mut getattr: HashSet<Value> = HashSet::new();
        let mut defs: HashMap<String, Vec<TopDef>> = HashMap::new();
        for block in &function.blocks {
            for instr in &block.instrs {
                match instr {
                    Instr::Const { idx, into } => {
                        consts.insert(*into, *idx);
                    }
                    Instr::Copy { src, into } => {
                        if let Some(m) = module_of.get(src) {
                            module_of.insert(*into, *m);
                        }
                        if let Some(c) = class_of.get(src) {
                            class_of.insert(*into, *c);
                        }
                        if getattr.contains(src) {
                            getattr.insert(*into);
                        }
                    }
                    Instr::Import {
                        name,
                        fromlist,
                        level,
                        into,
                    } => {
                        let level = const_int(module, &consts, level);
                        let Some(target) =
                            self.import_target(fr.module, module.consts.sym_str(*name), level)
                        else {
                            continue;
                        };
                        self.import_chain(&target);
                        match const_fromlist(module, &consts, fromlist) {
                            // A plain import binds the top-level package.
                            None => {
                                if let Some(&idx) =
                                    self.by_name.get(target.split('.').next().unwrap_or(""))
                                {
                                    module_of.insert(*into, idx);
                                }
                            }
                            Some(names) => {
                                // A from-import binds the module itself and
                                // imports every submodule it names.
                                if let Some(&idx) = self.by_name.get(target.as_str()) {
                                    module_of.insert(*into, idx);
                                }
                                for name in names {
                                    if name == "*" {
                                        continue;
                                    }
                                    let sub = format!("{target}.{name}");
                                    if let Some(&idx) = self.by_name.get(sub.as_str()) {
                                        self.import_module(idx);
                                    }
                                }
                            }
                        }
                    }
                    Instr::ImportStar { module: m } => {
                        if let Some(idx) = module_of.get(m).copied() {
                            let defs: Vec<TopDef> = self.namespaces[idx as usize]
                                .as_ref()
                                .map(|ns| ns.defs.values().flatten().copied().collect())
                                .unwrap_or_default();
                            for def in &defs {
                                self.reach_def(def);
                            }
                        }
                    }
                    Instr::GetAttr { obj, name, into } => {
                        if let Some(idx) = module_of.get(obj).copied() {
                            self.module_attr(
                                idx,
                                module.consts.sym_str(*name),
                                *into,
                                &mut module_of,
                                &mut class_of,
                            );
                        }
                    }
                    Instr::LoadName { name, into } | Instr::LoadGlobal { name, into } => {
                        let loaded = module.consts.sym_str(*name);
                        let resolved = defs.get(loaded).cloned().or_else(|| {
                            self.namespaces[fr.module as usize]
                                .as_ref()
                                .and_then(|ns| ns.defs.get(loaded).cloned())
                        });
                        if let Some(found) = resolved {
                            for def in &found {
                                self.reach_def(def);
                            }
                            // A provably single binding marks the value, so a
                            // later attribute load or base list resolves it.
                            if let [def] = found.as_slice() {
                                match def {
                                    TopDef::Class(cref) => {
                                        class_of.insert(*into, *cref);
                                    }
                                    TopDef::Module(idx) => {
                                        module_of.insert(*into, *idx);
                                    }
                                    TopDef::Function(_) => {}
                                }
                            }
                        }
                        if loaded == "getattr" {
                            let shadowed = defs.contains_key("getattr")
                                || self.namespaces[fr.module as usize]
                                    .as_ref()
                                    .is_some_and(|ns| ns.defs.contains_key("getattr"));
                            if !shadowed {
                                getattr.insert(*into);
                            }
                        }
                    }
                    Instr::StoreName { name, value } | Instr::StoreGlobal { name, value } => {
                        let stored = module.consts.sym_str(*name);
                        if let Some(c) = class_of.get(value) {
                            defs.entry(stored.to_owned())
                                .or_default()
                                .push(TopDef::Class(*c));
                        }
                        if let Some(m) = module_of.get(value) {
                            defs.entry(stored.to_owned())
                                .or_default()
                                .push(TopDef::Module(*m));
                        }
                    }
                    Instr::MakeFunction { func, .. } => {
                        self.reach_fn(FnRef {
                            module: fr.module,
                            function: *func,
                        });
                    }
                    Instr::MakeClass {
                        desc,
                        bases,
                        kwds,
                        into,
                        ..
                    } => {
                        let cref = ClassRef {
                            module: fr.module,
                            class: *desc,
                        };
                        self.reach_class(cref);
                        class_of.insert(*into, cref);
                        let resolved: Vec<Option<ClassRef>> = bases
                            .iter()
                            .map(|base| class_of.get(base).copied())
                            .collect();
                        let plain_kwds = consts.get(kwds).is_some_and(|idx| {
                            matches!(module.consts.consts[idx.0 as usize], Const::None)
                        });
                        self.class_info.insert(
                            cref,
                            ClassInfo {
                                bases: resolved,
                                plain_kwds,
                            },
                        );
                    }
                    Instr::Call {
                        callee, args, into, ..
                    } => {
                        if !getattr.contains(callee) || args.len() < 2 {
                            continue;
                        }
                        let Some(&ConstIdx(idx)) = consts.get(&args[1]) else {
                            continue;
                        };
                        let Const::Str(sym) = &module.consts.consts[idx as usize] else {
                            continue;
                        };
                        if let Some(m) = module_of.get(&args[0]).copied() {
                            self.module_attr(
                                m,
                                module.consts.sym_str(*sym),
                                *into,
                                &mut module_of,
                                &mut class_of,
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// The top-level namespace of every compiled module. Re-exported names
/// resolve through the namespaces of the modules the body imports, so
/// rounds repeat until the namespaces stop growing; a chain of re-exports
/// settles in at most one round per link.
fn compute_namespaces(
    modules: &[Compiled],
    by_name: &HashMap<&str, u32>,
) -> Vec<Option<Namespace>> {
    let mut namespaces: Vec<Option<Namespace>> = vec![None; modules.len()];
    for _ in 0..=modules.len() {
        let mut changed = false;
        for (idx, compiled) in modules.iter().enumerate() {
            let Some(module) = &compiled.module else {
                continue;
            };
            let fresh = module_namespace(idx as u32, module, modules, by_name, &namespaces);
            if namespaces[idx].as_ref() != Some(&fresh) {
                namespaces[idx] = Some(fresh);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    namespaces
}

/// One module's namespace from its body: the names its stores bind, with
/// imported modules marked and re-exports resolved through the namespaces
/// computed so far.
fn module_namespace(
    idx: u32,
    module: &Module,
    modules: &[Compiled],
    by_name: &HashMap<&str, u32>,
    namespaces: &[Option<Namespace>],
) -> Namespace {
    let body = &module.functions[module.body.0 as usize];
    let mut consts: HashMap<Value, ConstIdx> = HashMap::new();
    for block in &body.blocks {
        for instr in &block.instrs {
            if let Instr::Const { idx, into } = instr {
                consts.insert(*into, *idx);
            }
        }
    }
    let mut fn_of: HashMap<Value, FnId> = HashMap::new();
    let mut class_of: HashMap<Value, ClassRef> = HashMap::new();
    let mut module_of: HashMap<Value, u32> = HashMap::new();
    let mut defs: BTreeMap<String, Vec<TopDef>> = BTreeMap::new();
    for block in &body.blocks {
        for instr in &block.instrs {
            match instr {
                Instr::Const { .. } => {}
                Instr::Copy { src, into } => {
                    if let Some(f) = fn_of.get(src) {
                        fn_of.insert(*into, *f);
                    }
                    if let Some(c) = class_of.get(src) {
                        class_of.insert(*into, *c);
                    }
                    if let Some(m) = module_of.get(src) {
                        module_of.insert(*into, *m);
                    }
                }
                Instr::Import {
                    name,
                    fromlist,
                    level,
                    into,
                } => {
                    let level = const_int(module, &consts, level).or(Some(0));
                    let importer = &modules[idx as usize];
                    let target = if level.unwrap_or(0) == 0 {
                        Some(module.consts.sym_str(*name).to_owned())
                    } else {
                        crate::build::relative_base(
                            &importer.name,
                            importer.is_package,
                            level.unwrap_or(0).try_into().unwrap_or(i32::MAX),
                        )
                        .map(|base| {
                            let module = module.consts.sym_str(*name);
                            if module.is_empty() {
                                base
                            } else {
                                format!("{base}.{module}")
                            }
                        })
                    };
                    let Some(target) = target else { continue };
                    let binds = match const_fromlist(module, &consts, fromlist) {
                        // A plain import binds the top-level package.
                        None => target.split('.').next().unwrap_or("").to_owned(),
                        Some(_) => target,
                    };
                    if let Some(&bound) = by_name.get(binds.as_str()) {
                        module_of.insert(*into, bound);
                    }
                }
                Instr::GetAttr { obj, name, into } => {
                    let Some(src) = module_of.get(obj).copied() else {
                        continue;
                    };
                    let attr = module.consts.sym_str(*name);
                    let full = format!("{}.{}", modules[src as usize].name, attr);
                    if let Some(&sub) = by_name.get(full.as_str()) {
                        module_of.insert(*into, sub);
                        continue;
                    }
                    let Some(ns) = namespaces[src as usize].as_ref() else {
                        continue;
                    };
                    let Some(found) = ns.defs.get(attr) else {
                        continue;
                    };
                    // A re-export: the loaded value is what the source binds.
                    if let [single] = found.as_slice() {
                        match single {
                            TopDef::Function(fr) => {
                                fn_of.insert(*into, fr.function);
                            }
                            TopDef::Class(cr) => {
                                class_of.insert(*into, *cr);
                            }
                            TopDef::Module(m) => {
                                module_of.insert(*into, *m);
                            }
                        }
                    }
                }
                Instr::LoadName { name, into } | Instr::LoadGlobal { name, into } => {
                    let loaded = module.consts.sym_str(*name);
                    // A re-export alias of an already-computed binding
                    // (`alias = cls`) carries the same value.
                    if let Some([def]) = defs.get(loaded).map(Vec::as_slice) {
                        match def {
                            TopDef::Function(fr) => {
                                fn_of.insert(*into, fr.function);
                            }
                            TopDef::Class(cr) => {
                                class_of.insert(*into, *cr);
                            }
                            TopDef::Module(m) => {
                                module_of.insert(*into, *m);
                            }
                        }
                    }
                }
                Instr::StoreName { name, value } | Instr::StoreGlobal { name, value } => {
                    let stored = module.consts.sym_str(*name).to_owned();
                    if let Some(f) = fn_of.get(value) {
                        defs.entry(stored.clone())
                            .or_default()
                            .push(TopDef::Function(FnRef {
                                module: idx,
                                function: *f,
                            }));
                    }
                    if let Some(c) = class_of.get(value) {
                        defs.entry(stored.clone())
                            .or_default()
                            .push(TopDef::Class(*c));
                    }
                    if let Some(m) = module_of.get(value) {
                        defs.entry(stored).or_default().push(TopDef::Module(*m));
                    }
                }
                Instr::MakeFunction { func, into, .. } => {
                    fn_of.insert(*into, *func);
                }
                Instr::MakeClass { desc, into, .. } => {
                    class_of.insert(
                        *into,
                        ClassRef {
                            module: idx,
                            class: *desc,
                        },
                    );
                }
                _ => {}
            }
        }
    }
    Namespace { defs }
}

/// The integer an import's `level` operand lowers to, if it is a constant.
fn const_int(module: &Module, consts: &HashMap<Value, ConstIdx>, value: &Value) -> Option<i64> {
    let idx = consts.get(value)?;
    match &module.consts.consts[idx.0 as usize] {
        Const::Int(level) => Some(*level),
        _ => None,
    }
}

/// The names of a from-list: `None` for a plain import, the names otherwise
/// (`*` included). A non-constant list lowers to neither.
fn const_fromlist(
    module: &Module,
    consts: &HashMap<Value, ConstIdx>,
    value: &Value,
) -> Option<Vec<String>> {
    let idx = consts.get(value)?;
    match &module.consts.consts[idx.0 as usize] {
        Const::None => None,
        Const::Tuple(items) => Some(
            items
                .iter()
                .map(|item| match &module.consts.consts[item.0 as usize] {
                    Const::Str(sym) => module.consts.sym_str(*sym).to_owned(),
                    _ => String::new(),
                })
                .collect(),
        ),
        _ => None,
    }
}

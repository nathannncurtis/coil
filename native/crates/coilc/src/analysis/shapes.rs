//! Per compiled class, the attribute shape of its instances (spec section
//! 8.3): the slot names from the stores in `__init__` and the remaining
//! methods (`__init__` first, then the methods in definition order, each in
//! instruction order, first store winning), then the `__slots__` entries.
//! Past 32 slots the class gets a dict. A class that flows to a reflective
//! site, or that a keep rule pins, is unsealed; a sealed class's members are
//! what the call graph resolves method calls against.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use coil_ir::ir::{ClassDesc, Const, Instr, Module};
use coil_ir::types::{ConstIdx, FnId, ShapeId, Sym, Value};

use crate::world::{ClassRef, FnRef, Program};

/// A store past this many attributes materialises a dict (spec section 8.3).
pub const SLOT_LIMIT: usize = 32;

/// Methods whose first parameter is the class, not the instance: the
/// implicit protocol methods. A `@staticmethod`'s or `@classmethod`'s first
/// parameter is not the instance either; those are caught by the decorated
/// store check in [`self_params`].
const NOT_SELF: [&str; 3] = ["__class_getitem__", "__init_subclass__", "__new__"];

/// The attribute shape of one compiled class.
#[derive(Clone, Debug)]
pub struct Shape {
    pub cls: ClassRef,
    /// Slot names in first-store order, then the `__slots__` entries.
    pub slots: Vec<String>,
    /// The stores passed [`SLOT_LIMIT`]: instances materialise a dict
    /// (spec section 8.3).
    pub dict: bool,
    /// A sealing candidate that never flows to a reflective site. The
    /// candidates already exclude kept classes and kept modules' classes
    /// (spec section 6).
    pub sealed: bool,
}

/// The shapes of the program's classes, one [`ShapeId`] per class in
/// `Program::classes` order.
pub struct Shapes {
    shapes: Vec<Shape>,
    index: BTreeMap<ClassRef, ShapeId>,
    /// The classes whose shape is sealed, for the call graph's method
    /// resolution.
    sealed: BTreeSet<ClassRef>,
    /// Methods whose first parameter is the instance, and of which class.
    self_of: BTreeMap<FnRef, ClassRef>,
}

impl Shapes {
    /// Build every class's shape. `reflective` is the set of classes a value
    /// known to be an instance of them reached a reflective site in
    /// ([`crate::analysis::callgraph::reflective`]); those are unsealed.
    pub fn analyze(program: &Program, reflective: &BTreeSet<ClassRef>) -> Shapes {
        let self_of = self_params(program);
        let mut shapes = Vec::new();
        let mut index = BTreeMap::new();
        let mut sealed = BTreeSet::new();
        for &cr in &program.classes {
            let (ir, desc) = class(program, cr);
            // `__init__` first: its stores are what a fresh instance carries,
            // in first-store order; the other methods follow in definition
            // order.
            let mut ordered: Vec<(Sym, FnId)> = Vec::with_capacity(desc.methods.len());
            let init = desc
                .methods
                .iter()
                .find(|(name, _)| ir.consts.sym_str(*name) == "__init__");
            if let Some(init) = init {
                ordered.push(*init);
            }
            ordered.extend(
                desc.methods
                    .iter()
                    .filter(|(name, _)| ir.consts.sym_str(*name) != "__init__")
                    .copied(),
            );
            let mut slots = Vec::new();
            for (_, fid) in ordered {
                let fr = FnRef {
                    module: cr.module,
                    function: fid,
                };
                if self_of.get(&fr) != Some(&cr) {
                    continue;
                }
                for block in &ir.functions[fid.0 as usize].blocks {
                    for instr in &block.instrs {
                        if let Instr::SetAttr {
                            obj: Value(0),
                            name,
                            ..
                        } = instr
                        {
                            let slot = ir.consts.sym_str(*name);
                            if !slots.iter().any(|s| s == slot) {
                                slots.push(slot.to_owned());
                            }
                        }
                    }
                }
            }
            // The layout puts `__slots__` entries after the inferred
            // attributes (spec section 8.3).
            for slot in slots_entries(ir, desc) {
                if !slots.contains(&slot) {
                    slots.push(slot);
                }
            }
            let is_sealed = program.seal_candidates.contains(&cr) && !reflective.contains(&cr);
            if is_sealed {
                sealed.insert(cr);
            }
            index.insert(cr, ShapeId(shapes.len() as u32));
            shapes.push(Shape {
                cls: cr,
                dict: slots.len() > SLOT_LIMIT,
                slots,
                sealed: is_sealed,
            });
        }
        Shapes {
            shapes,
            index,
            sealed,
            self_of,
        }
    }

    /// The class's shape, if the program reached the class.
    pub fn get(&self, cls: ClassRef) -> Option<ShapeId> {
        self.index.get(&cls).copied()
    }

    pub fn shape(&self, id: ShapeId) -> &Shape {
        &self.shapes[id.0 as usize]
    }

    /// The index of the slot `name` in the shape, if the shape carries it.
    pub fn slot(&self, id: ShapeId, name: &str) -> Option<u32> {
        self.shape(id)
            .slots
            .iter()
            .position(|s| s == name)
            .map(|i| i as u32)
    }

    /// Whether the class's shape is sealed.
    pub fn is_sealed(&self, cls: ClassRef) -> bool {
        self.sealed.contains(&cls)
    }

    /// The sealed classes; method calls on them resolve along the MRO.
    pub fn sealed_classes(&self) -> &BTreeSet<ClassRef> {
        &self.sealed
    }

    /// The whole `self` map - the methods whose first parameter is the
    /// instance, and of which class - for the call graph scan.
    pub(crate) fn self_of_map(&self) -> &BTreeMap<FnRef, ClassRef> {
        &self.self_of
    }

    /// One line per class in `Program::classes` order: the slot list and the
    /// `dict` and `sealed` marks.
    pub fn dump(&self, program: &Program) -> String {
        let mut out = String::new();
        for shape in &self.shapes {
            let name = &program.modules[shape.cls.module as usize].name;
            let class = program.modules[shape.cls.module as usize]
                .ir
                .as_ref()
                .expect("classes only exist in compiled modules");
            let class = class
                .consts
                .sym_str(class.classes[shape.cls.class.0 as usize].name);
            out.push_str(&format!("{name}.{class} slots={}", shape.slots.join(", ")));
            if shape.dict {
                out.push_str(" dict");
            }
            if shape.sealed {
                out.push_str(" sealed");
            }
            out.push('\n');
        }
        out
    }
}

/// Methods whose first parameter is the instance: the methods the class body
/// stores undecorated (a decorated method's stored value is the decorator
/// call's result, so a `@staticmethod`'s or `@classmethod`'s first parameter
/// is never mistaken for the instance), minus the protocol methods.
pub(crate) fn self_params(program: &Program) -> BTreeMap<FnRef, ClassRef> {
    let mut out = BTreeMap::new();
    for &cr in &program.classes {
        let (ir, desc) = class(program, cr);
        let body = &ir.functions[desc.body.0 as usize];
        let mut fn_of: HashMap<Value, FnId> = HashMap::new();
        // The plain function each stored name binds; a store of anything
        // else clears it.
        let mut stored: HashMap<&str, FnId> = HashMap::new();
        for block in &body.blocks {
            for instr in &block.instrs {
                match instr {
                    Instr::MakeFunction { func, into, .. } => {
                        fn_of.insert(*into, *func);
                    }
                    Instr::Copy { src, into } => {
                        if let Some(f) = fn_of.get(src) {
                            fn_of.insert(*into, *f);
                        }
                    }
                    Instr::StoreName { name, value } | Instr::StoreGlobal { name, value } => {
                        let name = ir.consts.sym_str(*name);
                        match fn_of.get(value) {
                            Some(f) => {
                                stored.insert(name, *f);
                            }
                            None => {
                                stored.remove(name);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (name, fid) in &desc.methods {
            let name = ir.consts.sym_str(*name);
            if NOT_SELF.contains(&name) || stored.get(name) != Some(fid) {
                continue;
            }
            out.insert(
                FnRef {
                    module: cr.module,
                    function: *fid,
                },
                cr,
            );
        }
    }
    out
}

/// The `__slots__` entries of a class body: the names of the string or
/// string-tuple literal the body stores as `__slots__`. A tuple literal
/// lowers to `BuildTuple` over string constants; a pool `Const::Tuple` never
/// appears for one.
fn slots_entries(ir: &Module, desc: &ClassDesc) -> Vec<String> {
    let body = &ir.functions[desc.body.0 as usize];
    let mut consts: HashMap<Value, ConstIdx> = HashMap::new();
    let mut tuples: HashMap<Value, Vec<String>> = HashMap::new();
    for block in &body.blocks {
        for instr in &block.instrs {
            match instr {
                Instr::Const { idx, into } => {
                    consts.insert(*into, *idx);
                }
                Instr::BuildTuple { items, into } => {
                    let names: Option<Vec<String>> = items
                        .iter()
                        .map(|item| {
                            consts.get(item).and_then(|idx| {
                                match &ir.consts.consts[idx.0 as usize] {
                                    Const::Str(sym) => Some(ir.consts.sym_str(*sym).to_owned()),
                                    _ => None,
                                }
                            })
                        })
                        .collect();
                    if let Some(names) = names {
                        tuples.insert(*into, names);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for block in &body.blocks {
        for instr in &block.instrs {
            let Instr::StoreName { name, value } = instr else {
                continue;
            };
            if ir.consts.sym_str(*name) != "__slots__" {
                continue;
            }
            if let Some(names) = tuples.get(value) {
                out.extend(names.iter().cloned());
            } else if let Some(Const::Str(sym)) = consts
                .get(value)
                .map(|idx| &ir.consts.consts[idx.0 as usize])
            {
                out.push(ir.consts.sym_str(*sym).to_owned());
            }
        }
    }
    out
}

/// The IR and descriptor of a program class.
fn class(program: &Program, cr: ClassRef) -> (&Module, &ClassDesc) {
    let ir = program.modules[cr.module as usize]
        .ir
        .as_ref()
        .expect("classes only exist in compiled modules");
    (ir, &ir.classes[cr.class.0 as usize])
}

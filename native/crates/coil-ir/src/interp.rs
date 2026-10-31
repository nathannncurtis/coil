//! The IR interpreter (spec section 5): executes Coil IR over boxed values.
//!
//! One executor serves two masters: the reference executor for IR tests, and
//! the engine behind runtime `exec`/`eval` (spec section 6, tier 3). It
//! implements every instruction; the optimiser's static instructions are
//! never produced by the front end, but hand-built IR in the crate's tests
//! drives them through the same loop.
//!
//! The interpreter owns control flow: SSA slots, block dispatch, exception
//! routing to `Block::handler`, the frame's current exception and `exc_info`
//! stack. Every value operation is a call into the
//! [`ObjectModel`](model::ObjectModel) seam, which re-enters the interpreter
//! through its public helpers (`call_value`, `call_ir_function`,
//! `new_frame`, `resume_frame`) because attribute lookups, comparisons and
//! calls run Python code.
//!
//! Generators, coroutines and async generators are suspended [`Frame`]s held
//! by the model's generator objects; `Yield` suspends at an instruction
//! boundary, `YieldFrom`/`Await` delegate to a sub-iterator and re-enter the
//! same instruction on resume with the sent value or thrown exception
//! delivered to it.

pub mod model;
pub mod testmodel;

use crate::interp::model::{ErrorTag, IrFunction, ObjectModel, UnboxError};
use crate::ir::{Const, Instr, Module, Scalar, Terminator};
use crate::types::{BlockId, ConstIdx, FnId, InstrId, Value};

/// What one SSA slot holds: a boxed model value, or one of the unboxable
/// scalars.
pub enum Slot<M: ObjectModel> {
    Box(M::Val),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl<M: ObjectModel> Clone for Slot<M> {
    fn clone(&self) -> Self {
        match self {
            Slot::Box(v) => Slot::Box(v.clone()),
            Slot::Int(i) => Slot::Int(*i),
            Slot::Float(f) => Slot::Float(*f),
            Slot::Bool(b) => Slot::Bool(*b),
        }
    }
}

impl<M: ObjectModel> std::fmt::Debug for Slot<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Slot::Box(_) => f.write_str("Box(_)"),
            Slot::Int(i) => write!(f, "Int({i})"),
            Slot::Float(x) => write!(f, "Float({x})"),
            Slot::Bool(b) => write!(f, "Bool({b})"),
        }
    }
}

/// How a suspended frame is resumed.
pub enum Resume<Val> {
    /// First execution: the frame starts at the entry block.
    Start,
    /// `.send(v)`: the value the `Yield` result slot or the `YieldFrom`
    /// delegation receives.
    Send(Val),
    /// `.throw(e)`: the exception delivered at the suspension point.
    Throw(Val),
}

/// Why `run` stopped.
pub enum Flow<M: ObjectModel> {
    /// `Return`ed a value.
    Done(M::Val),
    /// `Yield`ed (or the delegation yielded); the frame is suspended and
    /// resumable.
    Suspended(M::Val),
}

/// One activation: the SSA slots, the closure cells, the class-body
/// namespace (`None` for functions; the module frame also uses `None` and
/// resolves names against the globals), the exception state the handler
/// instructions manage, and the resume state generators need. Held by the
/// model inside generator objects between suspensions.
pub struct Frame<M: ObjectModel> {
    pub fid: FnId,
    pub slots: Vec<Slot<M>>,
    pub closure: Vec<M::Val>,
    pub namespace: Option<M::Val>,
    block: BlockId,
    ip: usize,
    line: u32,
    /// `sys.exc_info()[0]` for this frame: the exception being handled.
    /// `CatchEnd` clears it, bare `raise` re-raises it, `PopExcInfo`
    /// restores the raise-point value the landing's `PushExcInfo` saved.
    cur_exc: Option<M::Val>,
    /// The exception propagating into the current landing block; `Reraise`
    /// re-raises it. `route_or_propagate` sets it alongside `cur_exc`.
    in_flight: Option<M::Val>,
    /// The raise-point `cur_exc` awaiting the landing's `PushExcInfo`.
    saved_prev: Option<Option<M::Val>>,
    /// The `PushExcInfo`/`PopExcInfo` stack of saved previous exceptions.
    exc_info: Vec<Option<M::Val>>,
    /// A `YieldFrom`/`Await` in flight: `(into, iter)`.
    deleg: Option<(Value, M::Val)>,
    /// The `Yield` result slot awaiting the sent value.
    resume_store: Option<Value>,
}

impl<M: ObjectModel> Frame<M> {
    fn new(
        fid: FnId,
        slots: Vec<Slot<M>>,
        closure: Vec<M::Val>,
        namespace: Option<M::Val>,
    ) -> Self {
        Frame {
            fid,
            slots,
            closure,
            namespace,
            block: BlockId(0),
            ip: 0,
            line: 0,
            cur_exc: None,
            in_flight: None,
            saved_prev: None,
            exc_info: Vec::new(),
            deleg: None,
            resume_store: None,
        }
    }
}

// One bit per IR instruction kind (and terminator kind, and `Phi`), set when
// executed. The tag list is the single inventory of the IR: coverage reports
// are generated from it, never by hand.

const TAG_NAMES: &[&str] = &[
    "Const",
    "Copy",
    "IAddOvf",
    "ISubOvf",
    "IMulOvf",
    "IDivOvf",
    "INegOvf",
    "ShlOvf",
    "IAdd",
    "ISub",
    "IMul",
    "IDiv",
    "IMod",
    "INeg",
    "Shl",
    "Shr",
    "BitAnd",
    "BitOr",
    "BitXor",
    "BitNot",
    "ICmp",
    "FAdd",
    "FSub",
    "FMul",
    "FDiv",
    "FMod",
    "FNeg",
    "FCmp",
    "Box",
    "Unbox",
    "BinOp",
    "UnaryOp",
    "Compare",
    "GetAttr",
    "SetAttr",
    "DelAttr",
    "GetItem",
    "SetItem",
    "DelItem",
    "GetIter",
    "Next",
    "Call",
    "CallEx",
    "CallMethod",
    "Truth",
    "Len",
    "Contains",
    "Format",
    "BuildList",
    "BuildTuple",
    "BuildDict",
    "BuildSet",
    "BuildStr",
    "BuildSlice",
    "Unpack",
    "Import",
    "ImportStar",
    "MakeFunction",
    "MakeClass",
    "MakeCell",
    "LoadClosure",
    "LoadCell",
    "StoreCell",
    "LoadGlobal",
    "StoreGlobal",
    "LoadName",
    "StoreName",
    "CallDirect",
    "LoadField",
    "StoreField",
    "AllocInst",
    "AllocTuple",
    "TypeGuard",
    "ShapeGuard",
    "BoundsCheck",
    "ListGetUnchecked",
    "TupleGetUnchecked",
    "StackAlloc",
    "Yield",
    "YieldFrom",
    "Await",
    "Safepoint",
    "CatchBegin",
    "CatchEnd",
    "PushExcInfo",
    "PopExcInfo",
    "FinallyEnter",
    "FinallyExit",
    "CatchStarBegin",
    "PrepReraiseStar",
    "Jump",
    "Branch",
    "Switch",
    "Return",
    "Raise",
    "Reraise",
    "Unreachable",
    "Phi",
];

const T_TERM: usize = 90;
const T_PHI: usize = T_TERM + 7;

/// The name of one coverage tag: `Instr` variants, then `Terminator`
/// variants, then `Phi`.
pub fn tag_name(tag: usize) -> &'static str {
    TAG_NAMES[tag]
}

pub fn tag_count() -> usize {
    TAG_NAMES.len()
}

fn tag_of_instr(i: &Instr) -> usize {
    use Instr as I;

    match i {
        I::Const { .. } => 0,
        I::Copy { .. } => 1,
        I::IAddOvf { .. } => 2,
        I::ISubOvf { .. } => 3,
        I::IMulOvf { .. } => 4,
        I::IDivOvf { .. } => 5,
        I::INegOvf { .. } => 6,
        I::ShlOvf { .. } => 7,
        I::IAdd { .. } => 8,
        I::ISub { .. } => 9,
        I::IMul { .. } => 10,
        I::IDiv { .. } => 11,
        I::IMod { .. } => 12,
        I::INeg { .. } => 13,
        I::Shl { .. } => 14,
        I::Shr { .. } => 15,
        I::BitAnd { .. } => 16,
        I::BitOr { .. } => 17,
        I::BitXor { .. } => 18,
        I::BitNot { .. } => 19,
        I::ICmp { .. } => 20,
        I::FAdd { .. } => 21,
        I::FSub { .. } => 22,
        I::FMul { .. } => 23,
        I::FDiv { .. } => 24,
        I::FMod { .. } => 25,
        I::FNeg { .. } => 26,
        I::FCmp { .. } => 27,
        I::Box { .. } => 28,
        I::Unbox { .. } => 29,
        I::BinOp { .. } => 30,
        I::UnaryOp { .. } => 31,
        I::Compare { .. } => 32,
        I::GetAttr { .. } => 33,
        I::SetAttr { .. } => 34,
        I::DelAttr { .. } => 35,
        I::GetItem { .. } => 36,
        I::SetItem { .. } => 37,
        I::DelItem { .. } => 38,
        I::GetIter { .. } => 39,
        I::Next { .. } => 40,
        I::Call { .. } => 41,
        I::CallEx { .. } => 42,
        I::CallMethod { .. } => 43,
        I::Truth { .. } => 44,
        I::Len { .. } => 45,
        I::Contains { .. } => 46,
        I::Format { .. } => 47,
        I::BuildList { .. } => 48,
        I::BuildTuple { .. } => 49,
        I::BuildDict { .. } => 50,
        I::BuildSet { .. } => 51,
        I::BuildStr { .. } => 52,
        I::BuildSlice { .. } => 53,
        I::Unpack { .. } => 54,
        I::Import { .. } => 55,
        I::ImportStar { .. } => 56,
        I::MakeFunction { .. } => 57,
        I::MakeClass { .. } => 58,
        I::MakeCell { .. } => 59,
        I::LoadClosure { .. } => 60,
        I::LoadCell { .. } => 61,
        I::StoreCell { .. } => 62,
        I::LoadGlobal { .. } => 63,
        I::StoreGlobal { .. } => 64,
        I::LoadName { .. } => 65,
        I::StoreName { .. } => 66,
        I::CallDirect { .. } => 67,
        I::LoadField { .. } => 68,
        I::StoreField { .. } => 69,
        I::AllocInst { .. } => 70,
        I::AllocTuple { .. } => 71,
        I::TypeGuard { .. } => 72,
        I::ShapeGuard { .. } => 73,
        I::BoundsCheck { .. } => 74,
        I::ListGetUnchecked { .. } => 75,
        I::TupleGetUnchecked { .. } => 76,
        I::StackAlloc { .. } => 77,
        I::Yield { .. } => 78,
        I::YieldFrom { .. } => 79,
        I::Await { .. } => 80,
        I::Safepoint => 81,
        I::CatchBegin { .. } => 82,
        I::CatchEnd => 83,
        I::PushExcInfo { .. } => 84,
        I::PopExcInfo => 85,
        I::FinallyEnter { .. } => 86,
        I::FinallyExit { .. } => 87,
        I::CatchStarBegin { .. } => 88,
        I::PrepReraiseStar { .. } => 89,
    }
}

fn tag_of_term(t: &Terminator) -> usize {
    match t {
        Terminator::Jump { .. } => T_TERM,
        Terminator::Branch { .. } => T_TERM + 1,
        Terminator::Switch { .. } => T_TERM + 2,
        Terminator::Return { .. } => T_TERM + 3,
        Terminator::Raise { .. } => T_TERM + 4,
        Terminator::Reraise => T_TERM + 5,
        Terminator::Unreachable => T_TERM + 6,
    }
}

/// The executed coverage tags.
#[derive(Clone)]
pub struct Coverage {
    seen: Vec<bool>,
}

impl Coverage {
    fn new() -> Self {
        Coverage {
            seen: vec![false; TAG_NAMES.len()],
        }
    }

    pub fn hit(&mut self, tag: usize) {
        self.seen[tag] = true;
    }

    /// The executed tags, in tag order.
    pub fn covered(&self) -> Vec<usize> {
        self.seen
            .iter()
            .enumerate()
            .filter(|(_, seen)| **seen)
            .map(|(tag, _)| tag)
            .collect()
    }

    /// The never-executed tags, in tag order.
    pub fn uncovered(&self) -> Vec<usize> {
        self.seen
            .iter()
            .enumerate()
            .filter(|(_, seen)| !**seen)
            .map(|(tag, _)| tag)
            .collect()
    }
}

/// CPython's default recursion limit; deep recursion observes
/// `RecursionError`.
const RECURSION_LIMIT: usize = 1000;

/// The interpreter. Holds the module under execution, the module globals,
/// the runtime class values of `MakeClass` results and the coverage
/// counters. The model is passed by every method that needs it, so model
/// operations can re-enter through `&mut Interp`.
pub struct Interp<'m, M: ObjectModel> {
    module: &'m Module,
    /// The module-level globals dict, `LoadGlobal`'s first stop.
    pub globals: M::Val,
    depth: usize,
    /// The runtime class values of `MakeClass` results, by `ClassId`, for
    /// `AllocInst` and `TypeGuard`.
    classes: Vec<Option<M::Val>>,
    pub coverage: Coverage,
    file: String,
}

enum Step<M: ObjectModel> {
    Next,
    Suspend(M::Val),
}

impl<'m, M: ObjectModel> Interp<'m, M> {
    pub fn new(module: &'m Module, globals: M::Val) -> Self {
        Interp {
            module,
            globals,
            depth: 0,
            classes: vec![None; module.classes.len()],
            coverage: Coverage::new(),
            file: module.file.display().to_string(),
        }
    }

    pub fn module(&self) -> &Module {
        self.module
    }

    pub fn flags_of(&self, func: FnId) -> crate::ir::FnFlags {
        self.module.functions[func.0 as usize].flags
    }

    /// The runtime class value registered for `ClassId` by `MakeClass`.
    pub fn class_value(&self, cls: crate::types::ClassId) -> Option<&M::Val> {
        self.classes[cls.0 as usize].as_ref()
    }

    /// Runs the module body.
    pub fn run_main(&mut self, m: &mut M) -> Result<M::Val, M::Val> {
        let body = self.module.body;
        let mut frame = self.new_frame(m, body, vec![], &[], &[], &m.none(), &m.none(), None)?;
        match self.run(m, &mut frame, Resume::Start)? {
            Flow::Done(v) => Ok(v),
            Flow::Suspended(_) => Err(m.error(ErrorTag::RuntimeError, "module body yielded")),
        }
    }

    // ---- model entry points ---------------------------------------------

    /// Calls any callable value (IR function, native builtin, class, bound
    /// method) through the model.
    pub fn call_value(
        &mut self,
        m: &mut M,
        callee: &M::Val,
        args: &[M::Val],
        kwargs: &[(&str, M::Val)],
    ) -> Result<M::Val, M::Val> {
        m.call(self, callee, args, kwargs)
    }

    /// Builds a frame for `fid` with `args`/`kwargs` bound against the
    /// parameter list, applying `defaults` (a tuple value or `None`) and
    /// `kwdefaults` (a dict value or `None`). Errors follow CPython's
    /// `TypeError` messages. For generator functions the model builds the
    /// frame here and stores it without running it.
    #[allow(clippy::too_many_arguments)]
    pub fn new_frame(
        &mut self,
        m: &mut M,
        fid: FnId,
        closure: Vec<M::Val>,
        args: &[M::Val],
        kwargs: &[(&str, M::Val)],
        defaults: &M::Val,
        kwdefaults: &M::Val,
        namespace: Option<M::Val>,
    ) -> Result<Frame<M>, M::Val> {
        let default_items: Vec<M::Val> = if m.is_none(defaults) {
            Vec::new()
        } else {
            m.tuple_items(defaults).unwrap_or_default()
        };
        self.bind_args(
            m,
            fid,
            closure,
            args,
            kwargs,
            &default_items,
            kwdefaults,
            namespace,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_args(
        &mut self,
        m: &mut M,
        fid: FnId,
        closure: Vec<M::Val>,
        args: &[M::Val],
        kwargs: &[(&str, M::Val)],
        defaults: &[M::Val],
        kwdefaults: &M::Val,
        namespace: Option<M::Val>,
    ) -> Result<Frame<M>, M::Val> {
        let func = &self.module.functions[fid.0 as usize];
        let params = &func.params;
        let qualname = self.module.consts.sym_str(func.qualname).to_owned();
        let n_pos = params.positional.len();
        let has_kwarg = params.kwarg.is_some();
        let arity = params.arity();
        let mut slots: Vec<Slot<M>> = Vec::with_capacity(arity);
        let mut set = vec![false; arity];

        let take = args.len().min(n_pos);
        for a in &args[..take] {
            slots.push(Slot::Box(a.clone()));
        }
        for set_flag in &mut set[..take] {
            *set_flag = true;
        }
        if args.len() > n_pos && params.vararg.is_none() {
            let given = args.len();
            let want = if defaults.is_empty() {
                if n_pos == 1 {
                    "1 positional argument".to_owned()
                } else {
                    format!("{n_pos} positional arguments")
                }
            } else {
                let lo = n_pos - defaults.len();
                if lo == n_pos {
                    format!("{n_pos} positional arguments")
                } else if lo == 1 {
                    format!("from 1 to {n_pos} positional arguments")
                } else {
                    format!("from {lo} to {n_pos} positional arguments")
                }
            };
            let were = if given == 1 { "was" } else { "were" };
            return Err(m.error(
                ErrorTag::TypeError,
                &format!("{qualname}() takes {want} but {given} {were} given"),
            ));
        }
        if params.vararg.is_some() {
            let extras: Vec<M::Val> = if args.len() > n_pos {
                args[n_pos..].to_vec()
            } else {
                Vec::new()
            };
            slots.push(Slot::Box(m.tuple(extras)));
            set[n_pos] = true;
        }
        for _ in 0..params.kwonly.len() {
            slots.push(Slot::Box(m.none()));
        }
        if has_kwarg {
            slots.push(Slot::Box(m.new_dict()));
            set[n_pos + usize::from(params.vararg.is_some()) + params.kwonly.len()] = true;
        }

        let mut leftover: Vec<(&str, M::Val)> = Vec::new();
        for (name, value) in kwargs {
            let pos = params
                .positional
                .iter()
                .position(|p| self.module.consts.sym_str(*p) == *name);
            let kwonly = params
                .kwonly
                .iter()
                .position(|p| self.module.consts.sym_str(*p) == *name);
            match (pos, kwonly) {
                (Some(i), _) => {
                    if set[i] {
                        return Err(m.error(
                            ErrorTag::TypeError,
                            &format!("{qualname}() got multiple values for argument '{name}'"),
                        ));
                    }
                    slots[i] = Slot::Box(value.clone());
                    set[i] = true;
                }
                (None, Some(j)) => {
                    let at = n_pos + usize::from(params.vararg.is_some()) + j;
                    if set[at] {
                        return Err(m.error(
                            ErrorTag::TypeError,
                            &format!("{qualname}() got multiple values for argument '{name}'"),
                        ));
                    }
                    slots[at] = Slot::Box(value.clone());
                    set[at] = true;
                }
                (None, None) if has_kwarg => leftover.push((*name, value.clone())),
                (None, None) => {
                    return Err(m.error(
                        ErrorTag::TypeError,
                        &format!("{qualname}() got an unexpected keyword argument '{name}'"),
                    ));
                }
            }
        }
        if has_kwarg
            && let Slot::Box(d) =
                &slots[n_pos + usize::from(params.vararg.is_some()) + params.kwonly.len()]
        {
            for (name, value) in leftover {
                m.dict_set(d, name, value);
            }
        }

        // positional defaults and missing-argument errors
        let first_default = n_pos.saturating_sub(defaults.len());
        let mut missing: Vec<&str> = Vec::new();
        for i in 0..n_pos {
            if set[i] {
                continue;
            }
            if i >= first_default {
                slots[i] = Slot::Box(defaults[i - first_default].clone());
            } else {
                missing.push(self.module.consts.sym_str(params.positional[i]));
            }
        }
        if !missing.is_empty() {
            let noun = if missing.len() == 1 {
                "argument"
            } else {
                "arguments"
            };
            return Err(m.error(
                ErrorTag::TypeError,
                &format!(
                    "{qualname}() missing {} required positional {}{}: {}",
                    missing.len(),
                    noun,
                    if missing.len() == 1 { "" } else { "s" },
                    name_list(&missing)
                ),
            ));
        }
        // keyword-only defaults
        let mut missing_kw: Vec<&str> = Vec::new();
        for (j, p) in params.kwonly.iter().enumerate() {
            let at = n_pos + usize::from(params.vararg.is_some()) + j;
            if !set[at] {
                let name = self.module.consts.sym_str(*p);
                if !m.is_none(kwdefaults)
                    && let Some(v) = m.dict_get(kwdefaults, name)
                {
                    slots[at] = Slot::Box(v);
                } else {
                    missing_kw.push(name);
                }
            }
        }
        if !missing_kw.is_empty() {
            let noun = if missing_kw.len() == 1 {
                "argument"
            } else {
                "arguments"
            };
            return Err(m.error(
                ErrorTag::TypeError,
                &format!(
                    "{qualname}() missing {} required keyword-only {}{}: {}",
                    missing_kw.len(),
                    noun,
                    if missing_kw.len() == 1 { "" } else { "s" },
                    name_list(&missing_kw)
                ),
            ));
        }
        Ok(Frame::new(fid, slots, closure, namespace))
    }

    /// Calls an IR function value's code now (the non-generator path of
    /// `ObjectModel::call`).
    pub fn call_ir_function(
        &mut self,
        m: &mut M,
        func: &IrFunction<M::Val>,
        args: &[M::Val],
        kwargs: &[(&str, M::Val)],
        namespace: Option<M::Val>,
    ) -> Result<M::Val, M::Val> {
        let defaults = func.defaults.clone();
        let kwdefaults = func.kwdefaults.clone();
        let mut frame = self.new_frame(
            m,
            func.func,
            func.cells.clone(),
            args,
            kwargs,
            &defaults,
            &kwdefaults,
            namespace,
        )?;
        match self.run(m, &mut frame, Resume::Start)? {
            Flow::Done(v) => Ok(v),
            Flow::Suspended(_) => {
                Err(m.error(ErrorTag::TypeError, "'generator' object is not callable"))
            }
        }
    }

    /// Resumes a suspended (or fresh) frame: `Start` begins at the entry
    /// block, `Send`/`Throw` deliver at the suspension point.
    pub fn resume_frame(
        &mut self,
        m: &mut M,
        frame: &mut Frame<M>,
        action: Resume<M::Val>,
    ) -> Result<Flow<M>, M::Val> {
        self.run(m, frame, action)
    }

    // ---- execution --------------------------------------------------------

    fn run(
        &mut self,
        m: &mut M,
        frame: &mut Frame<M>,
        action: Resume<M::Val>,
    ) -> Result<Flow<M>, M::Val> {
        self.depth += 1;
        if self.depth > RECURSION_LIMIT {
            self.depth -= 1;
            return Err(m.error(ErrorTag::RecursionError, "maximum recursion depth exceeded"));
        }
        let delivered = match action {
            Resume::Start => None,
            Resume::Send(v) => match frame.deleg.take() {
                Some((into, iter)) => self.deleg_step(m, frame, into, &iter, Some(v))?,
                None => {
                    if let Some(into) = frame.resume_store.take() {
                        frame.slots[into.idx()] = Slot::Box(v);
                    }
                    None
                }
            },
            Resume::Throw(e) => {
                frame.resume_store = None;
                match frame.deleg.take() {
                    Some((into, iter)) => match m.iter_throw(self, &iter, e) {
                        Ok(y) => Some(y),
                        Err(err) => self.deleg_caught(m, frame, into, err)?,
                    },
                    None => {
                        self.route_or_propagate(m, frame, e)?;
                        None
                    }
                }
            }
        };
        if let Some(y) = delivered {
            // still suspended in the delegation: leave `deleg` set by the
            // step that produced the yield
            self.depth -= 1;
            return Ok(Flow::Suspended(y));
        }
        let out = self.loop_(m, frame);
        if !matches!(&out, Ok(Flow::Suspended(_))) {
            self.depth -= 1;
        }
        out
    }

    /// One `YieldFrom`/`Await` step: send `value` (None = first) into
    /// `iter`; a yield suspends (`Ok(Some(y))`, with `deleg` left set for
    /// the resume), `StopIteration` completes `into`, another exception
    /// routes in this frame.
    fn deleg_step(
        &mut self,
        m: &mut M,
        frame: &mut Frame<M>,
        into: Value,
        iter: &M::Val,
        value: Option<M::Val>,
    ) -> Result<Option<M::Val>, M::Val> {
        match m.iter_send(self, iter, value) {
            Ok(y) => {
                frame.deleg = Some((into, iter.clone()));
                Ok(Some(y))
            }
            Err(e) => self.deleg_caught(m, frame, into, e),
        }
    }

    fn deleg_caught(
        &mut self,
        m: &mut M,
        frame: &mut Frame<M>,
        into: Value,
        e: M::Val,
    ) -> Result<Option<M::Val>, M::Val> {
        if self.is_stop_iteration(m, &e) {
            let value = m.getattr(self, &e, "value").unwrap_or_else(|_| m.none());
            frame.slots[into.idx()] = Slot::Box(value);
            Ok(None)
        } else {
            self.route_or_propagate(m, frame, e)?;
            Ok(None)
        }
    }

    fn loop_(&mut self, m: &mut M, frame: &mut Frame<M>) -> Result<Flow<M>, M::Val> {
        loop {
            let fid = frame.fid;
            let block_id = frame.block;
            let (n_instrs, has_instr) = {
                let b = &self.module.functions[fid.0 as usize].blocks[block_id.0 as usize];
                (b.instrs.len(), frame.ip < b.instrs.len())
            };
            let _ = n_instrs;
            if has_instr {
                let ip = frame.ip;
                let instr = self.module.functions[fid.0 as usize].blocks[block_id.0 as usize]
                    .instrs[ip]
                    .clone();
                let id = self.global_instr_id(fid, block_id, ip);
                if let Some((line, _)) = self.module.functions[fid.0 as usize].line_of(id) {
                    frame.line = line;
                }
                self.coverage.hit(tag_of_instr(&instr));
                frame.ip += 1;
                if let Step::Suspend(v) = self.exec_instr(m, frame, &instr)? {
                    return Ok(Flow::Suspended(v));
                }
            } else {
                let term = self.module.functions[fid.0 as usize].blocks[block_id.0 as usize]
                    .term
                    .clone();
                self.coverage.hit(tag_of_term(&term));
                match term {
                    Terminator::Jump { to } => {
                        self.enter_block(frame, fid, block_id, to);
                    }
                    Terminator::Branch {
                        cond,
                        then_to,
                        else_to,
                    } => {
                        let on = match frame.slots[cond.idx()] {
                            Slot::Bool(b) => b,
                            Slot::Box(ref v) => match m.truth(self, v) {
                                Ok(b) => b,
                                Err(e) => {
                                    self.route_or_propagate(m, frame, e)?;
                                    continue;
                                }
                            },
                            _ => false,
                        };
                        let to = if on { then_to } else { else_to };
                        self.enter_block(frame, fid, block_id, to);
                    }
                    Terminator::Switch { on, cases, default } => {
                        let key = match frame.slots[on.idx()] {
                            Slot::Int(i) => Some(i),
                            Slot::Box(ref v) => m.unbox_int(v).ok(),
                            _ => None,
                        };
                        let to = key
                            .and_then(|k| cases.iter().find(|(v, _)| *v == k).map(|(_, b)| *b))
                            .unwrap_or(default);
                        self.enter_block(frame, fid, block_id, to);
                    }
                    Terminator::Return { value } => {
                        let v = self.take_val(frame, value);
                        return Ok(Flow::Done(v));
                    }
                    Terminator::Raise { exc, cause } => {
                        let exc_v = match exc {
                            Some(v) => {
                                let raw = self.val(frame, v).clone();
                                if m.is_class(&raw) {
                                    m.call(self, &raw, &[], &[])?
                                } else {
                                    raw
                                }
                            }
                            None => match frame.cur_exc.clone() {
                                Some(e) => e,
                                None => m.error(
                                    ErrorTag::RuntimeError,
                                    "No active exception to re-raise",
                                ),
                            },
                        };
                        if let Some(cur) = frame.cur_exc.clone()
                            && !m.is_same(&cur, &exc_v)
                        {
                            m.set_exc_context(&exc_v, Some(cur));
                        }
                        if let Some(c) = cause {
                            let cv = self.val(frame, c).clone();
                            if m.is_none(&cv) {
                                m.set_exc_cause(&exc_v, None);
                            } else {
                                m.set_exc_cause(&exc_v, Some(cv));
                            }
                        }
                        self.route_or_propagate(m, frame, exc_v)?;
                    }
                    Terminator::Reraise => {
                        let e = frame.in_flight.clone().unwrap_or_else(|| {
                            m.error(ErrorTag::RuntimeError, "No active exception to re-raise")
                        });
                        self.route_or_propagate(m, frame, e)?;
                    }
                    Terminator::Unreachable => {
                        panic!("interpreter reached Unreachable");
                    }
                }
            }
        }
    }

    fn global_instr_id(&self, fid: FnId, block: BlockId, ip: usize) -> InstrId {
        let func = &self.module.functions[fid.0 as usize];
        let mut n = 0u32;
        for b in &func.blocks[..block.0 as usize] {
            n += b.instrs.len() as u32;
        }
        InstrId(n + ip as u32)
    }

    /// Routes an exception from `frame`'s current block: to the block's
    /// handler if one is set, else out of the frame with a traceback entry.
    fn route_or_propagate(
        &mut self,
        m: &mut M,
        frame: &mut Frame<M>,
        exc: M::Val,
    ) -> Result<(), M::Val> {
        frame.saved_prev = Some(frame.cur_exc.clone());
        frame.cur_exc = Some(exc.clone());
        frame.in_flight = Some(exc.clone());
        let fid = frame.fid;
        let handler = self.module.functions[fid.0 as usize].blocks[frame.block.0 as usize].handler;
        match handler {
            Some(h) => {
                self.enter_block(frame, fid, frame.block, h);
                Ok(())
            }
            None => {
                let func = &self.module.functions[fid.0 as usize];
                let qualname = self.module.consts.sym_str(func.qualname).to_owned();
                m.add_traceback(&exc, &self.file, frame.line, &qualname);
                Err(exc)
            }
        }
    }

    fn enter_block(&mut self, frame: &mut Frame<M>, fid: FnId, from: BlockId, to: BlockId) {
        let func = &self.module.functions[fid.0 as usize];
        let block = &func.blocks[to.0 as usize];
        if !block.phis.is_empty() {
            for phi in &block.phis {
                let incoming = phi
                    .incomings
                    .iter()
                    .find(|(_, b)| *b == from)
                    .unwrap_or_else(|| {
                        panic!(
                            "phi in block {} has no incoming from block {}",
                            to.0, from.0
                        )
                    });
                let v = frame.slots[incoming.0.idx()].clone();
                frame.slots[phi.into.idx()] = v;
            }
            self.coverage.hit(T_PHI);
        }
        frame.block = to;
        frame.ip = 0;
    }

    fn val<'a>(&self, frame: &'a Frame<M>, v: Value) -> &'a M::Val {
        match &frame.slots[v.idx()] {
            Slot::Box(b) => b,
            _ => panic!("value v{} used boxed but holds a primitive", v.0),
        }
    }

    fn take_val(&self, frame: &mut Frame<M>, v: Value) -> M::Val {
        match frame.slots[v.idx()] {
            Slot::Box(ref b) => b.clone(),
            _ => panic!("value v{} used boxed but holds a primitive", v.0),
        }
    }

    fn const_val(&self, m: &mut M, idx: ConstIdx) -> M::Val {
        match &self.module.consts.consts[idx.0 as usize] {
            Const::None => m.none(),
            Const::Bool(b) => m.bool(*b),
            Const::Int(i) => m.int(*i),
            Const::BigInt(s) => m.bigint(s),
            Const::Float(f) => m.float(*f),
            Const::Str(s) => m.str(self.module.consts.sym_str(*s)),
            Const::Bytes(b) => m.bytes(b),
            Const::Tuple(items) => {
                let vals = items.iter().map(|i| self.const_val(m, *i)).collect();
                m.tuple(vals)
            }
            Const::Ellipsis => m.ellipsis(),
        }
    }

    fn is_stop_iteration(&mut self, m: &mut M, e: &M::Val) -> bool {
        match m.builtin("StopIteration") {
            Some(stop) => m.exc_matches(e, &stop),
            None => false,
        }
    }

    fn exec_instr(
        &mut self,
        m: &mut M,
        frame: &mut Frame<M>,
        instr: &Instr,
    ) -> Result<Step<M>, M::Val> {
        use Instr as I;
        macro_rules! try_or_route {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(e) => {
                        self.route_or_propagate(m, frame, e)?;
                        return Ok(Step::Next);
                    }
                }
            };
        }
        match instr {
            I::Const { idx, into } => {
                let v = self.const_val(m, *idx);
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Copy { src, into } => {
                let v = frame.slots[src.idx()].clone();
                frame.slots[into.idx()] = v;
            }
            I::IAddOvf {
                lhs,
                rhs,
                into,
                ovf,
            } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                match a.checked_add(b) {
                    Some(v) => {
                        frame.slots[into.idx()] = Slot::Int(v);
                        frame.slots[ovf.idx()] = Slot::Bool(false);
                    }
                    None => {
                        frame.slots[into.idx()] = Slot::Int(a.wrapping_add(b));
                        frame.slots[ovf.idx()] = Slot::Bool(true);
                    }
                }
            }
            I::ISubOvf {
                lhs,
                rhs,
                into,
                ovf,
            } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                match a.checked_sub(b) {
                    Some(v) => {
                        frame.slots[into.idx()] = Slot::Int(v);
                        frame.slots[ovf.idx()] = Slot::Bool(false);
                    }
                    None => {
                        frame.slots[into.idx()] = Slot::Int(a.wrapping_sub(b));
                        frame.slots[ovf.idx()] = Slot::Bool(true);
                    }
                }
            }
            I::IMulOvf {
                lhs,
                rhs,
                into,
                ovf,
            } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                match a.checked_mul(b) {
                    Some(v) => {
                        frame.slots[into.idx()] = Slot::Int(v);
                        frame.slots[ovf.idx()] = Slot::Bool(false);
                    }
                    None => {
                        frame.slots[into.idx()] = Slot::Int(a.wrapping_mul(b));
                        frame.slots[ovf.idx()] = Slot::Bool(true);
                    }
                }
            }
            I::IDivOvf {
                lhs,
                rhs,
                into,
                ovf,
            } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                if b == 0 {
                    let e = m.error(
                        ErrorTag::ZeroDivisionError,
                        "integer division or modulo by zero",
                    );
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                match floor_div_mod(a, b) {
                    Some((q, _)) => {
                        frame.slots[into.idx()] = Slot::Int(q as i64);
                        frame.slots[ovf.idx()] = Slot::Bool(false);
                    }
                    None => {
                        frame.slots[into.idx()] = Slot::Int(0);
                        frame.slots[ovf.idx()] = Slot::Bool(true);
                    }
                }
            }
            I::INegOvf { src, into, ovf } => {
                let a = self.int(frame, *src);
                match a.checked_neg() {
                    Some(v) => {
                        frame.slots[into.idx()] = Slot::Int(v);
                        frame.slots[ovf.idx()] = Slot::Bool(false);
                    }
                    None => {
                        frame.slots[into.idx()] = Slot::Int(i64::MIN);
                        frame.slots[ovf.idx()] = Slot::Bool(true);
                    }
                }
            }
            I::ShlOvf {
                lhs,
                rhs,
                into,
                ovf,
            } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                if b < 0 {
                    let e = m.error(ErrorTag::ValueError, "negative shift count");
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                let sh = b as u32;
                match a.checked_shl(sh) {
                    Some(v) if sh < 63 && (v >> sh) == a => {
                        frame.slots[into.idx()] = Slot::Int(v);
                        frame.slots[ovf.idx()] = Slot::Bool(false);
                    }
                    _ => {
                        frame.slots[into.idx()] = Slot::Int(0);
                        frame.slots[ovf.idx()] = Slot::Bool(true);
                    }
                }
            }
            I::IAdd { lhs, rhs, into } => {
                let v = self.int(frame, *lhs).wrapping_add(self.int(frame, *rhs));
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::ISub { lhs, rhs, into } => {
                let v = self.int(frame, *lhs).wrapping_sub(self.int(frame, *rhs));
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::IMul { lhs, rhs, into } => {
                let v = self.int(frame, *lhs).wrapping_mul(self.int(frame, *rhs));
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::IDiv { lhs, rhs, into } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                if b == 0 {
                    let e = m.error(
                        ErrorTag::ZeroDivisionError,
                        "integer division or modulo by zero",
                    );
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                let (q, _) = floor_div_mod(a, b).unwrap_or((0, 0));
                frame.slots[into.idx()] = Slot::Int(q as i64);
            }
            I::IMod { lhs, rhs, into } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                if b == 0 {
                    let e = m.error(
                        ErrorTag::ZeroDivisionError,
                        "integer division or modulo by zero",
                    );
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                let (_, r) = floor_div_mod(a, b).unwrap_or((0, 0));
                frame.slots[into.idx()] = Slot::Int(r as i64);
            }
            I::INeg { src, into } => {
                let v = self.int(frame, *src).wrapping_neg();
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::Shl { lhs, rhs, into } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                if b < 0 {
                    let e = m.error(ErrorTag::ValueError, "negative shift count");
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                let sh = (b as u64 & 127) as u32;
                frame.slots[into.idx()] = Slot::Int((((a as u128) << sh) as i128) as i64);
            }
            I::Shr { lhs, rhs, into } => {
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                if b < 0 {
                    let e = m.error(ErrorTag::ValueError, "negative shift count");
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                frame.slots[into.idx()] = Slot::Int(a >> (b as u64 & 63));
            }
            I::BitAnd { lhs, rhs, into } => {
                let v = self.int(frame, *lhs) & self.int(frame, *rhs);
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::BitOr { lhs, rhs, into } => {
                let v = self.int(frame, *lhs) | self.int(frame, *rhs);
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::BitXor { lhs, rhs, into } => {
                let v = self.int(frame, *lhs) ^ self.int(frame, *rhs);
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::BitNot { src, into } => {
                let v = !self.int(frame, *src);
                frame.slots[into.idx()] = Slot::Int(v);
            }
            I::ICmp { op, lhs, rhs, into } => {
                use crate::ir::ICmpOp;
                let (a, b) = (self.int(frame, *lhs), self.int(frame, *rhs));
                let v = match op {
                    ICmpOp::Eq => a == b,
                    ICmpOp::Ne => a != b,
                    ICmpOp::Lt => a < b,
                    ICmpOp::Le => a <= b,
                    ICmpOp::Gt => a > b,
                    ICmpOp::Ge => a >= b,
                };
                frame.slots[into.idx()] = Slot::Bool(v);
            }
            I::FAdd { lhs, rhs, into } => {
                let v = self.float(frame, *lhs) + self.float(frame, *rhs);
                frame.slots[into.idx()] = Slot::Float(v);
            }
            I::FSub { lhs, rhs, into } => {
                let v = self.float(frame, *lhs) - self.float(frame, *rhs);
                frame.slots[into.idx()] = Slot::Float(v);
            }
            I::FMul { lhs, rhs, into } => {
                let v = self.float(frame, *lhs) * self.float(frame, *rhs);
                frame.slots[into.idx()] = Slot::Float(v);
            }
            I::FDiv { lhs, rhs, into } => {
                let (a, b) = (self.float(frame, *lhs), self.float(frame, *rhs));
                if b == 0.0 {
                    let e = m.error(ErrorTag::ZeroDivisionError, "float division by zero");
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                frame.slots[into.idx()] = Slot::Float(a / b);
            }
            I::FMod { lhs, rhs, into } => {
                let (a, b) = (self.float(frame, *lhs), self.float(frame, *rhs));
                if b == 0.0 {
                    let e = m.error(ErrorTag::ZeroDivisionError, "float modulo");
                    self.route_or_propagate(m, frame, e)?;
                    return Ok(Step::Next);
                }
                frame.slots[into.idx()] = Slot::Float(py_fmod(a, b));
            }
            I::FNeg { src, into } => {
                let v = -self.float(frame, *src);
                frame.slots[into.idx()] = Slot::Float(v);
            }
            I::FCmp { op, lhs, rhs, into } => {
                use crate::ir::FCmpOp;
                let (a, b) = (self.float(frame, *lhs), self.float(frame, *rhs));
                let v = match op {
                    FCmpOp::Eq => a == b,
                    FCmpOp::Ne => a != b,
                    FCmpOp::Lt => a < b,
                    FCmpOp::Le => a <= b,
                    FCmpOp::Gt => a > b,
                    FCmpOp::Ge => a >= b,
                };
                frame.slots[into.idx()] = Slot::Bool(v);
            }
            I::Box { src, scalar, into } => {
                let v = match (&frame.slots[src.idx()], scalar) {
                    (Slot::Int(i), Scalar::Int64) => m.int(*i),
                    (Slot::Float(f), Scalar::Float64) => m.float(*f),
                    (Slot::Bool(b), Scalar::Bool) => m.bool(*b),
                    _ => panic!("Box of a mismatched scalar"),
                };
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Unbox { src, scalar, into } => {
                let v = self.val(frame, *src).clone();
                match scalar {
                    Scalar::Int64 => match m.unbox_int(&v) {
                        Ok(i) => frame.slots[into.idx()] = Slot::Int(i),
                        Err(UnboxError::Overflow) => {
                            let e = m.error(
                                ErrorTag::OverflowError,
                                "Python int too large to convert to C long",
                            );
                            self.route_or_propagate(m, frame, e)?;
                            return Ok(Step::Next);
                        }
                        Err(UnboxError::Type) => {
                            let e = m.error(ErrorTag::TypeError, "an integer is required");
                            self.route_or_propagate(m, frame, e)?;
                            return Ok(Step::Next);
                        }
                    },
                    Scalar::Float64 => match m.unbox_float(&v) {
                        Ok(f) => frame.slots[into.idx()] = Slot::Float(f),
                        Err(_) => {
                            let e = m.error(ErrorTag::TypeError, "a float is required");
                            self.route_or_propagate(m, frame, e)?;
                            return Ok(Step::Next);
                        }
                    },
                    Scalar::Bool => match m.unbox_bool(&v) {
                        Ok(b) => frame.slots[into.idx()] = Slot::Bool(b),
                        Err(_) => {
                            let e = m.error(ErrorTag::TypeError, "a boolean is required");
                            self.route_or_propagate(m, frame, e)?;
                            return Ok(Step::Next);
                        }
                    },
                }
            }
            I::BinOp { op, lhs, rhs, into } => {
                let l = self.val(frame, *lhs).clone();
                let r = self.val(frame, *rhs).clone();
                let v = try_or_route!(m.binop(self, *op, &l, &r));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::UnaryOp { op, operand, into } => {
                let v = self.val(frame, *operand).clone();
                let r = try_or_route!(m.unary(self, *op, &v));
                frame.slots[into.idx()] = Slot::Box(r);
            }
            I::Compare { op, lhs, rhs, into } => {
                let l = self.val(frame, *lhs).clone();
                let r = self.val(frame, *rhs).clone();
                let v = try_or_route!(m.compare(self, *op, &l, &r));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Contains {
                haystack,
                needle,
                into,
            } => {
                let h = self.val(frame, *haystack).clone();
                let n = self.val(frame, *needle).clone();
                let v = try_or_route!(m.contains(self, &h, &n));
                frame.slots[into.idx()] = Slot::Box(m.bool(v));
            }
            I::GetAttr { obj, name, into } => {
                let o = self.val(frame, *obj).clone();
                let n = self.module.consts.sym_str(*name);
                let v = try_or_route!(m.getattr(self, &o, n));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::SetAttr { obj, name, value } => {
                let o = self.val(frame, *obj).clone();
                let n = self.module.consts.sym_str(*name);
                let v = self.val(frame, *value).clone();
                try_or_route!(m.setattr(self, &o, n, &v));
            }
            I::DelAttr { obj, name } => {
                let o = self.val(frame, *obj).clone();
                let n = self.module.consts.sym_str(*name);
                try_or_route!(m.delattr(self, &o, n));
            }
            I::GetItem { obj, key, into } => {
                let o = self.val(frame, *obj).clone();
                let k = self.val(frame, *key).clone();
                let v = try_or_route!(m.getitem(self, &o, &k));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::SetItem { obj, key, value } => {
                let o = self.val(frame, *obj).clone();
                let k = self.val(frame, *key).clone();
                let v = self.val(frame, *value).clone();
                try_or_route!(m.setitem(self, &o, &k, &v));
            }
            I::DelItem { obj, key } => {
                let o = self.val(frame, *obj).clone();
                let k = self.val(frame, *key).clone();
                try_or_route!(m.delitem(self, &o, &k));
            }
            I::GetIter { obj, into } => {
                let o = self.val(frame, *obj).clone();
                let v = try_or_route!(m.get_iter(self, &o));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Next { iter, into } => {
                let it = self.val(frame, *iter).clone();
                let v = try_or_route!(m.iter_next(self, &it));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Call {
                callee,
                args,
                kwnames,
                into,
            } => {
                let c = self.val(frame, *callee).clone();
                let a: Vec<M::Val> = args.iter().map(|v| self.val(frame, *v).clone()).collect();
                let kw: Vec<(&str, M::Val)> = kwnames
                    .iter()
                    .map(|(n, v)| (self.module.consts.sym_str(*n), self.val(frame, *v).clone()))
                    .collect();
                let v = try_or_route!(m.call(self, &c, &a, &kw));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::CallEx {
                callee,
                args,
                kwargs,
                into,
            } => {
                let c = self.val(frame, *callee).clone();
                let a = self.val(frame, *args).clone();
                let k = self.val(frame, *kwargs).clone();
                let v = try_or_route!(m.call_ex(self, &c, &a, &k));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::CallMethod {
                name,
                recv,
                args,
                kwnames,
                into,
            } => {
                let r = self.val(frame, *recv).clone();
                let n = self.module.consts.sym_str(*name);
                let a: Vec<M::Val> = args.iter().map(|v| self.val(frame, *v).clone()).collect();
                let kw: Vec<(&str, M::Val)> = kwnames
                    .iter()
                    .map(|(n, v)| (self.module.consts.sym_str(*n), self.val(frame, *v).clone()))
                    .collect();
                let bound = try_or_route!(m.getattr(self, &r, n));
                let v = try_or_route!(m.call(self, &bound, &a, &kw));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Truth { value, into } => {
                let v = self.val(frame, *value).clone();
                let b = try_or_route!(m.truth(self, &v));
                frame.slots[into.idx()] = Slot::Bool(b);
            }
            I::Len { value, into } => {
                let v = self.val(frame, *value).clone();
                let n = try_or_route!(m.len(self, &v));
                frame.slots[into.idx()] = Slot::Int(n);
            }
            I::Format { value, spec, into } => {
                let v = self.val(frame, *value).clone();
                let s = self.val(frame, *spec).clone();
                let r = try_or_route!(m.format(self, &v, &s));
                frame.slots[into.idx()] = Slot::Box(r);
            }
            I::BuildList { items, into } => {
                let v: Vec<M::Val> = items.iter().map(|i| self.val(frame, *i).clone()).collect();
                frame.slots[into.idx()] = Slot::Box(m.build_list(v));
            }
            I::BuildTuple { items, into } => {
                let v: Vec<M::Val> = items.iter().map(|i| self.val(frame, *i).clone()).collect();
                frame.slots[into.idx()] = Slot::Box(m.tuple(v));
            }
            I::BuildDict { items, into } => {
                let v: Vec<(M::Val, M::Val)> = items
                    .iter()
                    .map(|(k, x)| (self.val(frame, *k).clone(), self.val(frame, *x).clone()))
                    .collect();
                let d = try_or_route!(m.build_dict(self, v));
                frame.slots[into.idx()] = Slot::Box(d);
            }
            I::BuildSet { items, into } => {
                let v: Vec<M::Val> = items.iter().map(|i| self.val(frame, *i).clone()).collect();
                let s = try_or_route!(m.build_set(self, v));
                frame.slots[into.idx()] = Slot::Box(s);
            }
            I::BuildStr { parts, into } => {
                let v: Vec<M::Val> = parts.iter().map(|p| self.val(frame, *p).clone()).collect();
                frame.slots[into.idx()] = Slot::Box(m.build_str(v));
            }
            I::BuildSlice {
                start,
                stop,
                step,
                into,
            } => {
                let s = start.map(|v| self.val(frame, v).clone());
                let e = self.val(frame, *stop).clone();
                let st = step.map(|v| self.val(frame, v).clone());
                frame.slots[into.idx()] = Slot::Box(m.build_slice(s, e, st));
            }
            I::Unpack { src, n, star, into } => {
                let s = self.val(frame, *src).clone();
                let parts = try_or_route!(m.unpack(self, &s, *n, *star));
                for (i, v) in parts.into_iter().enumerate() {
                    frame.slots[into[i].idx()] = Slot::Box(v);
                }
            }
            I::Import {
                name,
                fromlist,
                level,
                into,
            } => {
                let n = self.module.consts.sym_str(*name);
                let f = self.val(frame, *fromlist).clone();
                let l = self.val(frame, *level).clone();
                let v = try_or_route!(m.import(self, n, &f, &l));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::ImportStar { module } => {
                let mod_v = self.val(frame, *module).clone();
                let names = try_or_route!(m.star_names(self, &mod_v));
                let ns = self.namespace_of(frame);
                for (name, v) in names {
                    m.dict_set(&ns, &name, v);
                }
            }
            I::MakeFunction {
                func,
                cells,
                defaults,
                into,
            } => {
                let c: Vec<M::Val> = cells.iter().map(|v| self.val(frame, *v).clone()).collect();
                let d = self.val(frame, defaults[0]).clone();
                let k = self.val(frame, defaults[1]).clone();
                let v = m.make_function(self, *func, c, d, k);
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::MakeClass {
                desc,
                bases,
                cells,
                kwds,
                into,
            } => {
                let b: Vec<M::Val> = bases.iter().map(|v| self.val(frame, *v).clone()).collect();
                let c: Vec<M::Val> = cells.iter().map(|v| self.val(frame, *v).clone()).collect();
                let k = self.val(frame, *kwds).clone();
                let d = &self.module.classes[desc.0 as usize];
                let body = m.make_function(self, d.body, c, m.none(), m.none());
                let name = self.module.consts.sym_str(d.name).to_owned();
                let cls = try_or_route!(m.make_class(self, &name, &body, &b, &k));
                self.classes[desc.0 as usize] = Some(cls.clone());
                frame.slots[into.idx()] = Slot::Box(cls);
            }
            I::MakeCell { initial, into } => {
                let i = initial.map(|v| self.val(frame, v).clone());
                frame.slots[into.idx()] = Slot::Box(m.new_cell(i));
            }
            I::LoadClosure { index, into } => {
                let cell = frame.closure[*index as usize].clone();
                frame.slots[into.idx()] = Slot::Box(cell);
            }
            I::LoadCell { cell, into } => {
                let c = self.val(frame, *cell).clone();
                match m.cell_get(&c) {
                    Some(v) => frame.slots[into.idx()] = Slot::Box(v),
                    None => {
                        let e = m.error(ErrorTag::NameError, "cannot access freed variable");
                        self.route_or_propagate(m, frame, e)?;
                        return Ok(Step::Next);
                    }
                }
            }
            I::StoreCell { cell, value } => {
                let c = self.val(frame, *cell).clone();
                let v = self.val(frame, *value).clone();
                m.cell_set(&c, v);
            }
            I::LoadGlobal { name, into } => {
                let n = self.module.consts.sym_str(*name);
                let v = m.dict_get(&self.globals, n).or_else(|| m.builtin(n));
                match v {
                    Some(v) => frame.slots[into.idx()] = Slot::Box(v),
                    None => {
                        let e = m.error(ErrorTag::NameError, &format!("name '{n}' is not defined"));
                        self.route_or_propagate(m, frame, e)?;
                        return Ok(Step::Next);
                    }
                }
            }
            I::StoreGlobal { name, value } => {
                let n = self.module.consts.sym_str(*name);
                let v = self.val(frame, *value).clone();
                m.dict_set(&self.globals, n, v);
            }
            I::LoadName { name, into } => {
                let n = self.module.consts.sym_str(*name);
                let v = match &frame.namespace {
                    Some(ns) => m
                        .dict_get(ns, n)
                        .or_else(|| m.dict_get(&self.globals, n))
                        .or_else(|| m.builtin(n)),
                    None => m.dict_get(&self.globals, n).or_else(|| m.builtin(n)),
                };
                match v {
                    Some(v) => frame.slots[into.idx()] = Slot::Box(v),
                    None => {
                        let e = m.error(ErrorTag::NameError, &format!("name '{n}' is not defined"));
                        self.route_or_propagate(m, frame, e)?;
                        return Ok(Step::Next);
                    }
                }
            }
            I::StoreName { name, value } => {
                let n = self.module.consts.sym_str(*name);
                let v = self.val(frame, *value).clone();
                let ns = self.namespace_of(frame);
                m.dict_set(&ns, n, v);
            }
            I::CallDirect { func, args, into } => {
                let a: Vec<M::Val> = args.iter().map(|v| self.val(frame, *v).clone()).collect();
                let none_v = m.none();
                let mut f = self.new_frame(m, *func, vec![], &a, &[], &none_v, &none_v, None)?;
                let r = self.run(m, &mut f, Resume::Start)?;
                match r {
                    Flow::Done(v) => frame.slots[into.idx()] = Slot::Box(v),
                    Flow::Suspended(_) => {
                        let e = m.error(ErrorTag::TypeError, "'generator' object is not callable");
                        self.route_or_propagate(m, frame, e)?;
                        return Ok(Step::Next);
                    }
                }
            }
            I::LoadField {
                obj,
                shape: _,
                slot,
                into,
            } => {
                let o = self.val(frame, *obj).clone();
                let v = try_or_route!(m.field_get(&o, *slot));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::StoreField {
                obj,
                shape: _,
                slot,
                value,
                barrier: _,
            } => {
                let o = self.val(frame, *obj).clone();
                let v = self.val(frame, *value).clone();
                try_or_route!(m.field_set(&o, *slot, &v));
            }
            I::AllocInst {
                cls,
                shape: _,
                into,
            }
            | I::StackAlloc {
                cls,
                shape: _,
                into,
            } => {
                let class = match self.classes[cls.0 as usize].clone() {
                    Some(c) => c,
                    None => {
                        let e = m.error(ErrorTag::TypeError, "class not yet created");
                        self.route_or_propagate(m, frame, e)?;
                        return Ok(Step::Next);
                    }
                };
                let v = try_or_route!(m.alloc_instance(self, &class, 0));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::AllocTuple { items, into } => {
                let v: Vec<M::Val> = items.iter().map(|i| self.val(frame, *i).clone()).collect();
                frame.slots[into.idx()] = Slot::Box(m.tuple(v));
            }
            I::TypeGuard {
                value,
                ty,
                ok,
                fail,
            } => {
                let v = self.val(frame, *value).clone();
                let matched = m.type_matches(self, &v, ty).unwrap_or(false);
                frame.slots[ok.idx()] = Slot::Box(v);
                frame.slots[fail.idx()] = Slot::Bool(!matched);
            }
            I::ShapeGuard {
                obj,
                shape: _,
                ok,
                fail,
            } => {
                let v = self.val(frame, *obj).clone();
                frame.slots[ok.idx()] = Slot::Box(v);
                frame.slots[fail.idx()] = Slot::Bool(false);
            }
            I::BoundsCheck { index, len, into } => {
                let i = self.int(frame, *index);
                let l = self.int(frame, *len);
                frame.slots[into.idx()] = Slot::Bool(0 <= i && i < l);
            }
            I::ListGetUnchecked { list, index, into } => {
                let l = self.val(frame, *list).clone();
                let i = self.int(frame, *index);
                let iv = m.int(i);
                let v = try_or_route!(m.getitem(self, &l, &iv));
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::TupleGetUnchecked { tuple, index, into } => {
                let t = self.val(frame, *tuple).clone();
                let items = m.tuple_items(&t).unwrap_or_default();
                let v = items
                    .get(*index as usize)
                    .cloned()
                    .unwrap_or_else(|| m.none());
                frame.slots[into.idx()] = Slot::Box(v);
            }
            I::Yield { value, into, .. } => {
                let v = self.val(frame, *value).clone();
                frame.resume_store = Some(*into);
                return Ok(Step::Suspend(v));
            }
            I::YieldFrom {
                iter, send, into, ..
            } => {
                let it = self.val(frame, *iter).clone();
                let s = self.val(frame, *send).clone();
                let first = if m.is_none(&s) { None } else { Some(s) };
                if let Some(y) = self.deleg_step(m, frame, *into, &it, first)? {
                    return Ok(Step::Suspend(y));
                }
            }
            I::Await { value, into, .. } => {
                let v = self.val(frame, *value).clone();
                let it = try_or_route!(m.await_iter(self, &v));
                if let Some(y) = self.deleg_step(m, frame, *into, &it, None)? {
                    return Ok(Step::Suspend(y));
                }
            }
            I::Safepoint => {}
            I::CatchBegin {
                types,
                matched,
                exc,
            } => {
                let cur = frame.cur_exc.clone().unwrap_or_else(|| m.none());
                let t = self.val(frame, *types).clone();
                let hit = m.exc_matches(&cur, &t);
                frame.slots[matched.idx()] = Slot::Bool(hit);
                frame.slots[exc.idx()] = Slot::Box(cur);
            }
            I::CatchEnd => {
                frame.cur_exc = None;
            }
            I::PushExcInfo { into } => {
                let prev = frame.saved_prev.take().unwrap_or(None);
                frame.exc_info.push(prev);
                let cur = frame.cur_exc.clone().unwrap_or_else(|| m.none());
                frame.slots[into.idx()] = Slot::Box(cur);
            }
            I::PopExcInfo => {
                frame.cur_exc = frame.exc_info.pop().flatten();
            }
            I::FinallyEnter { into } => {
                frame.slots[into.idx()] = Slot::Box(m.none());
            }
            I::FinallyExit { reason } => {
                let r = self.val(frame, *reason).clone();
                if !m.is_none(&r) && m.is_exception_instance(&r) {
                    self.route_or_propagate(m, frame, r)?;
                    return Ok(Step::Next);
                }
            }
            I::CatchStarBegin {
                src,
                types,
                matched,
                exc,
                rest,
            } => {
                let s = self.val(frame, *src).clone();
                let t = self.val(frame, *types).clone();
                let (hit, e, r) = try_or_route!(m.catch_star_split(self, &s, &t));
                frame.slots[matched.idx()] = Slot::Bool(hit);
                frame.slots[exc.idx()] = Slot::Box(e.clone());
                frame.slots[rest.idx()] = Slot::Box(r);
                if hit && !m.is_none(&e) {
                    frame.cur_exc = Some(e);
                }
            }
            I::PrepReraiseStar { orig, excs, into } => {
                let o = self.val(frame, *orig).clone();
                let x = self.val(frame, *excs).clone();
                let v = try_or_route!(m.prep_reraise_star(self, &o, &x));
                frame.slots[into.idx()] = Slot::Box(v);
            }
        }
        Ok(Step::Next)
    }

    fn int(&self, frame: &Frame<M>, v: Value) -> i64 {
        match frame.slots[v.idx()] {
            Slot::Int(i) => i,
            _ => panic!("value v{} used as Int64 but holds something else", v.0),
        }
    }

    fn float(&self, frame: &Frame<M>, v: Value) -> f64 {
        match frame.slots[v.idx()] {
            Slot::Float(f) => f,
            _ => panic!("value v{} used as Float64 but holds something else", v.0),
        }
    }

    fn namespace_of(&self, frame: &Frame<M>) -> M::Val {
        match &frame.namespace {
            Some(ns) => ns.clone(),
            None => self.globals.clone(),
        }
    }
}

/// CPython's "'a' and 'b'" / "'a', 'b', and 'c'" missing-argument listing.
fn name_list(names: &[&str]) -> String {
    match names.len() {
        0 => String::new(),
        1 => format!("'{}'", names[0]),
        2 => format!("'{}' and '{}'", names[0], names[1]),
        _ => {
            let head = names[..names.len() - 1]
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{head}, and '{}'", names[names.len() - 1])
        }
    }
}

/// Python's floor division and matching modulo over `i64`, widened to avoid
/// overflow; `None` for `i64::MIN / -1`.
fn floor_div_mod(a: i64, b: i64) -> Option<(i128, i128)> {
    let (a, b) = (a as i128, b as i128);
    let mut q = a / b;
    let mut r = a % b;
    if r != 0 && ((r < 0) != (b < 0)) {
        q -= 1;
        r += b;
    }
    if q < i64::MIN as i128 || q > i64::MAX as i128 {
        return None;
    }
    Some((q, r))
}

/// Python's float `%`: the result takes the divisor's sign.
pub fn py_fmod(a: f64, b: f64) -> f64 {
    let mut r = a % b;
    if r != 0.0 && (r < 0.0) != (b < 0.0) {
        r += b;
    }
    r
}

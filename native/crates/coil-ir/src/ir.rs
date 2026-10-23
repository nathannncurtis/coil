//! Coil IR data structures (spec section 5): one typed SSA IR with explicit
//! exception edges for the whole middle end.

use std::path::PathBuf;

use crate::types::{BlockId, ClassId, ConstIdx, FnId, InstrId, ShapeId, Sym, Type, Value};

/// Function flags. `GENERATOR`, `COROUTINE` and `ASYNC_GEN` mirror CPython's
/// code-object flags; `HAS_CELLS` marks a function whose locals include cell
/// or free variables.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FnFlags(pub u16);

impl FnFlags {
    pub const GENERATOR: FnFlags = FnFlags(1 << 0);
    pub const COROUTINE: FnFlags = FnFlags(1 << 1);
    pub const ASYNC_GEN: FnFlags = FnFlags(1 << 2);
    pub const HAS_CELLS: FnFlags = FnFlags(1 << 3);

    pub const fn contains(self, other: FnFlags) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: FnFlags) -> FnFlags {
        FnFlags(self.0 | other.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// A parameter list. Parameters define the first values of the function, in
/// this order: positional, then `vararg`, then keyword-only, then `kwarg`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Params {
    pub positional: Vec<Sym>,
    pub vararg: Option<Sym>,
    pub kwonly: Vec<Sym>,
    pub kwarg: Option<Sym>,
}

impl Params {
    pub fn arity(&self) -> usize {
        self.positional.len()
            + usize::from(self.vararg.is_some())
            + self.kwonly.len()
            + usize::from(self.kwarg.is_some())
    }

    pub fn names(&self) -> Vec<Sym> {
        let mut names = self.positional.clone();
        if let Some(v) = self.vararg {
            names.push(v);
        }
        names.extend_from_slice(&self.kwonly);
        if let Some(k) = self.kwarg {
            names.push(k);
        }
        names
    }
}

/// One local of a function: a plain local, a cell variable captured by inner
/// functions, or a free variable captured from an enclosing function.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LocalMeta {
    pub name: Sym,
    pub kind: LocalKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LocalKind {
    Local,
    Cell,
    Free,
}

/// A compiled class. `ClassId` indexes `Module::classes`. `body` is the
/// function executing the class statement; `methods` are the member
/// functions; `has_ext_base` marks a class with an extension type in its MRO,
/// whose instances live in the counted regime (section 9.3).
#[derive(Clone, PartialEq, Debug)]
pub struct ClassDesc {
    pub name: Sym,
    pub bases: Box<[Type]>,
    pub body: FnId,
    pub methods: Box<[(Sym, FnId)]>,
    pub has_ext_base: bool,
}

/// A constant. `BigInt` holds the decimal literal; `Str`, `Tuple` and
/// `Ellipsis` reference the pool.
#[derive(Clone, PartialEq, Debug)]
pub enum Const {
    None,
    Bool(bool),
    Int(i64),
    /// Arbitrary-precision integer as a decimal string.
    BigInt(String),
    Float(f64),
    Str(Sym),
    Bytes(Box<[u8]>),
    Tuple(Box<[ConstIdx]>),
    Ellipsis,
}

/// The module-level constant and string pool. `Sym` indices `strings`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct ConstPool {
    pub strings: Vec<String>,
    pub consts: Vec<Const>,
}

impl ConstPool {
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return Sym(i as u32);
        }
        self.strings.push(s.to_owned());
        Sym(self.strings.len() as u32 - 1)
    }

    pub fn sym_str(&self, sym: Sym) -> &str {
        &self.strings[sym.0 as usize]
    }

    pub fn const_idx(&mut self, c: Const) -> ConstIdx {
        if let Some(i) = self.consts.iter().position(|x| *x == c) {
            return ConstIdx(i as u32);
        }
        self.consts.push(c);
        ConstIdx(self.consts.len() as u32 - 1)
    }
}

/// A module: the unit of IR serialisation into the build cache.
#[derive(Clone, PartialEq, Debug)]
pub struct Module {
    pub name: Sym,
    pub file: PathBuf,
    pub functions: Vec<Function>,
    pub classes: Vec<ClassDesc>,
    pub consts: ConstPool,
    /// The function executing the module body.
    pub body: FnId,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Function {
    pub qualname: Sym,
    pub params: Params,
    pub blocks: Vec<Block>,
    pub entry: BlockId,
    pub locals: Vec<LocalMeta>,
    pub flags: FnFlags,
    /// Types indexed by `Value`; parameters are the first values.
    pub types: Vec<Type>,
    /// Line table: `(instruction, line, column)`, sorted by instruction.
    pub lines: Vec<(InstrId, u32, u32)>,
}

impl Function {
    fn block_offsets(&self) -> Vec<u32> {
        let mut offsets = Vec::with_capacity(self.blocks.len() + 1);
        let mut n = 0u32;
        for b in &self.blocks {
            offsets.push(n);
            n += b.instrs.len() as u32;
        }
        offsets.push(n);
        offsets
    }

    /// The function-wide instruction id of `blocks[block][index]`.
    pub fn instr_id(&self, block: BlockId, index: usize) -> Option<InstrId> {
        let offsets = self.block_offsets();
        let block = block.0 as usize;
        if block >= self.blocks.len() || index >= self.blocks[block].instrs.len() {
            return None;
        }
        Some(InstrId(offsets[block] + index as u32))
    }

    /// The `(line, column)` recorded for an instruction, if any.
    pub fn line_of(&self, id: InstrId) -> Option<(u32, u32)> {
        self.lines
            .binary_search_by_key(&id, |(i, _, _)| *i)
            .ok()
            .map(|p| (self.lines[p].1, self.lines[p].2))
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Block {
    pub phis: Vec<Phi>,
    pub instrs: Vec<Instr>,
    pub term: Terminator,
    /// Landing block for exceptions raised by this block's may-raise
    /// instructions; `None` propagates to the caller.
    pub handler: Option<BlockId>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Phi {
    pub into: Value,
    /// One incoming value per CFG predecessor.
    pub incomings: Box<[(Value, BlockId)]>,
}

/// Binary operator for the dynamic `BinOp` instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    LShift,
    RShift,
    BitAnd,
    BitOr,
    BitXor,
    MatMul,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnaryOp {
    Pos,
    Neg,
    Invert,
    Not,
}

/// Comparison flavours for the dynamic `Compare` instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Is,
    IsNot,
    In,
    NotIn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ICmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FCmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Store-barrier kind for `StoreField`: `None` (the stored value is statically
/// a tracing-regime type), `Ext` (statically an extension-type instance:
/// unconditional count update) or `Dynamic` (unknown: test the regime bit,
/// then update). Section 9.3.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BarrierKind {
    None,
    Ext,
    Dynamic,
}

/// The unboxable scalars, parameterising `Box` and `Unbox`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scalar {
    Int64,
    Float64,
    Bool,
}

/// One instruction. Every variant's doc line gives its operand and result
/// types. Results are the `Value` fields; instructions with no `Value` result
/// field produce nothing.
#[derive(Clone, PartialEq, Debug)]
pub enum Instr {
    // constants and moves
    /// `Const(idx) -> into`: the pool constant `idx`, of its constant type.
    Const { idx: ConstIdx, into: Value },
    /// `Copy(src) -> into`: the same value and type as `src`.
    Copy { src: Value, into: Value },

    // unboxed arithmetic
    /// `IAddOvf(lhs, rhs) -> (into: Int64, ovf: Bool)`: `Int64 + Int64` with an
    /// overflow result; never raises.
    IAddOvf {
        lhs: Value,
        rhs: Value,
        into: Value,
        ovf: Value,
    },
    /// `ISubOvf(lhs, rhs) -> (into: Int64, ovf: Bool)`.
    ISubOvf {
        lhs: Value,
        rhs: Value,
        into: Value,
        ovf: Value,
    },
    /// `IMulOvf(lhs, rhs) -> (into: Int64, ovf: Bool)`.
    IMulOvf {
        lhs: Value,
        rhs: Value,
        into: Value,
        ovf: Value,
    },
    /// `IDivOvf(lhs, rhs) -> (into: Int64, ovf: Bool)`: floor division with an
    /// overflow result (`i64::MIN // -1`); raises `ZeroDivisionError` on
    /// `rhs == 0`.
    IDivOvf {
        lhs: Value,
        rhs: Value,
        into: Value,
        ovf: Value,
    },
    /// `INegOvf(src) -> (into: Int64, ovf: Bool)`: overflows on `-i64::MIN`;
    /// never raises.
    INegOvf { src: Value, into: Value, ovf: Value },
    /// `ShlOvf(lhs, rhs) -> (into: Int64, ovf: Bool)`: overflows when bits or
    /// the sign would be lost; raises `ValueError` on a negative count.
    ShlOvf {
        lhs: Value,
        rhs: Value,
        into: Value,
        ovf: Value,
    },
    /// `IAdd(lhs, rhs) -> into: Int64`: unchecked; emitted only when overflow
    /// is proven impossible.
    IAdd { lhs: Value, rhs: Value, into: Value },
    /// `ISub(lhs, rhs) -> into: Int64`: unchecked.
    ISub { lhs: Value, rhs: Value, into: Value },
    /// `IMul(lhs, rhs) -> into: Int64`: unchecked.
    IMul { lhs: Value, rhs: Value, into: Value },
    /// `IDiv(lhs, rhs) -> into: Int64`: floor division, unchecked; raises
    /// `ZeroDivisionError` on `rhs == 0`.
    IDiv { lhs: Value, rhs: Value, into: Value },
    /// `IMod(lhs, rhs) -> into: Int64`: raises `ZeroDivisionError` on
    /// `rhs == 0`.
    IMod { lhs: Value, rhs: Value, into: Value },
    /// `INeg(src) -> into: Int64`: unchecked.
    INeg { src: Value, into: Value },
    /// `Shl(lhs, rhs) -> into: Int64`: unchecked; raises `ValueError` on a
    /// negative count.
    Shl { lhs: Value, rhs: Value, into: Value },
    /// `Shr(lhs, rhs) -> into: Int64`: arithmetic; raises `ValueError` on a
    /// negative count.
    Shr { lhs: Value, rhs: Value, into: Value },
    /// `BitAnd(lhs, rhs) -> into: Int64`.
    BitAnd { lhs: Value, rhs: Value, into: Value },
    /// `BitOr(lhs, rhs) -> into: Int64`.
    BitOr { lhs: Value, rhs: Value, into: Value },
    /// `BitXor(lhs, rhs) -> into: Int64`.
    BitXor { lhs: Value, rhs: Value, into: Value },
    /// `BitNot(src) -> into: Int64`.
    BitNot { src: Value, into: Value },
    /// `ICmp(op, lhs, rhs) -> into: Bool` over `Int64` operands.
    ICmp {
        op: ICmpOp,
        lhs: Value,
        rhs: Value,
        into: Value,
    },
    /// `FAdd(lhs, rhs) -> into: Float64`.
    FAdd { lhs: Value, rhs: Value, into: Value },
    /// `FSub(lhs, rhs) -> into: Float64`.
    FSub { lhs: Value, rhs: Value, into: Value },
    /// `FMul(lhs, rhs) -> into: Float64`.
    FMul { lhs: Value, rhs: Value, into: Value },
    /// `FDiv(lhs, rhs) -> into: Float64`: Python `/`; raises
    /// `ZeroDivisionError` on `rhs == 0.0`, as `IDiv` does.
    FDiv { lhs: Value, rhs: Value, into: Value },
    /// `FMod(lhs, rhs) -> into: Float64`: Python `%` (the result takes the
    /// divisor's sign); raises `ZeroDivisionError` on `rhs == 0.0`.
    FMod { lhs: Value, rhs: Value, into: Value },
    /// `FNeg(src) -> into: Float64`.
    FNeg { src: Value, into: Value },
    /// `FCmp(op, lhs, rhs) -> into: Bool` over `Float64` operands.
    FCmp {
        op: FCmpOp,
        lhs: Value,
        rhs: Value,
        into: Value,
    },

    // boxing
    /// `Box(src, scalar) -> into`: `src: Int64|Float64|Bool` becomes the
    /// boxed Python value: `BigInt`, boxed `Float64` or boxed `Bool`.
    Box {
        src: Value,
        scalar: Scalar,
        into: Value,
    },
    /// `Unbox(src, scalar) -> into` (guarded): `src: BigInt|Float64|Bool`
    /// becomes the primitive `Int64`/`Float64`/`Bool`; raises `TypeError` or
    /// `OverflowError` when it cannot.
    Unbox {
        src: Value,
        scalar: Scalar,
        into: Value,
    },

    // dynamic operations
    /// `BinOp(op, lhs, rhs) -> into`: boxed operands, boxed result.
    BinOp {
        op: BinOp,
        lhs: Value,
        rhs: Value,
        into: Value,
    },
    /// `UnaryOp(op, operand) -> into`: boxed.
    UnaryOp {
        op: UnaryOp,
        operand: Value,
        into: Value,
    },
    /// `Compare(op, lhs, rhs) -> into: Bool` (boxed).
    Compare {
        op: CmpOp,
        lhs: Value,
        rhs: Value,
        into: Value,
    },
    /// `GetAttr(obj, name) -> into`: boxed.
    GetAttr { obj: Value, name: Sym, into: Value },
    /// `SetAttr(obj, name, value)`: no result.
    SetAttr { obj: Value, name: Sym, value: Value },
    /// `DelAttr(obj, name)`: no result.
    DelAttr { obj: Value, name: Sym },
    /// `GetItem(obj, key) -> into`: boxed.
    GetItem { obj: Value, key: Value, into: Value },
    /// `SetItem(obj, key, value)`: no result.
    SetItem {
        obj: Value,
        key: Value,
        value: Value,
    },
    /// `DelItem(obj, key)`: no result.
    DelItem { obj: Value, key: Value },
    /// `GetIter(obj) -> into`: the iterator of `obj`.
    GetIter { obj: Value, into: Value },
    /// `Next(iter) -> into`: the next item; raises `StopIteration` at
    /// exhaustion.
    Next { iter: Value, into: Value },
    /// `Call(callee, args, kwnames) -> into`: boxed call.
    Call {
        callee: Value,
        args: Box<[Value]>,
        kwnames: Box<[(Sym, Value)]>,
        into: Value,
    },
    /// `CallEx(callee, args, kwargs) -> into`: call with a runtime argument
    /// list (CPython's `CALL_FUNCTION_EX`). `args` is the positional
    /// argument tuple; `kwargs` is a dict or the `None` constant.
    CallEx {
        callee: Value,
        args: Value,
        kwargs: Value,
        into: Value,
    },
    /// `CallMethod(name, recv, args, kwnames) -> into`: `recv.name(...)` with
    /// one lookup.
    CallMethod {
        name: Sym,
        recv: Value,
        args: Box<[Value]>,
        kwnames: Box<[(Sym, Value)]>,
        into: Value,
    },
    /// `Truth(value) -> into: Bool`: `bool(value)`.
    Truth { value: Value, into: Value },
    /// `Len(value) -> into: Int64`: `len(value)`.
    Len { value: Value, into: Value },
    /// `Contains(haystack, needle) -> into: Bool`: `needle in haystack`.
    Contains {
        haystack: Value,
        needle: Value,
        into: Value,
    },
    /// `Format(value, spec) -> into: Str`: `format(value, spec)`.
    Format {
        value: Value,
        spec: Value,
        into: Value,
    },
    /// `BuildList(items) -> into: List`.
    BuildList { items: Box<[Value]>, into: Value },
    /// `BuildTuple(items) -> into: Tuple`.
    BuildTuple { items: Box<[Value]>, into: Value },
    /// `BuildDict(items) -> into: Dict`: raises on an unhashable key.
    BuildDict {
        items: Box<[(Value, Value)]>,
        into: Value,
    },
    /// `BuildSet(items) -> into: Set`: raises on an unhashable item.
    BuildSet { items: Box<[Value]>, into: Value },
    /// `BuildStr(parts) -> into: Str`: concatenation of `Str` parts
    /// (f-strings, implicit concatenation).
    BuildStr { parts: Box<[Value]>, into: Value },
    /// `BuildSlice(start, stop, step) -> into`: a slice object.
    BuildSlice {
        start: Option<Value>,
        stop: Value,
        step: Option<Value>,
        into: Value,
    },
    /// `Unpack(src, n, star) -> into[0..n]`: unpack `src` into `n` targets,
    /// `star` naming the starred target's index; raises `ValueError`.
    Unpack {
        src: Value,
        n: u32,
        star: Option<u32>,
        into: Box<[Value]>,
    },
    /// `Import(name, fromlist, level) -> into`: the imported module.
    Import {
        name: Sym,
        fromlist: Value,
        level: Value,
        into: Value,
    },
    /// `ImportStar(module)`: bind every exported name of `module` in the
    /// current namespace (`from module import *`).
    ImportStar { module: Value },
    /// `MakeFunction(func, cells, defaults) -> into`: a function object.
    MakeFunction {
        func: FnId,
        cells: Box<[Value]>,
        defaults: Box<[Value]>,
        into: Value,
    },
    /// `MakeClass(desc, bases, cells, kwds) -> into`: run the class body of
    /// `desc` and create the class. `bases` and `kwds` (a dict or the `None`
    /// constant) are the `__build_class__` arguments; `cells` closes the
    /// class body function over its free variables.
    MakeClass {
        desc: ClassId,
        bases: Box<[Value]>,
        cells: Box<[Value]>,
        kwds: Value,
        into: Value,
    },
    /// `MakeCell(initial) -> into`: a fresh closure cell.
    MakeCell { initial: Option<Value>, into: Value },
    /// `LoadClosure(index) -> into`: the `index`-th cell of this function's
    /// closure, bound by `MakeFunction`/`MakeClass` to the `index`-th `Free`
    /// local in `locals` order; `LoadCell`/`StoreCell` operate on it.
    LoadClosure { index: u32, into: Value },
    /// `LoadCell(cell) -> into`: the cell's value; raises `NameError` when
    /// empty.
    LoadCell { cell: Value, into: Value },
    /// `StoreCell(cell, value)`: no result.
    StoreCell { cell: Value, value: Value },
    /// `LoadGlobal(name) -> into`: raises `NameError`.
    LoadGlobal { name: Sym, into: Value },
    /// `StoreGlobal(name, value)`: no result.
    StoreGlobal { name: Sym, value: Value },
    /// `LoadName(name) -> into`: scoped name load; raises `NameError`.
    LoadName { name: Sym, into: Value },
    /// `StoreName(name, value)`: no result.
    StoreName { name: Sym, value: Value },

    // static operations the optimiser introduces
    /// `CallDirect(func, args) -> into`: direct call to a known function.
    CallDirect {
        func: FnId,
        args: Box<[Value]>,
        into: Value,
    },
    /// `LoadField(obj, shape, slot) -> into`: slot load on a proven shape.
    LoadField {
        obj: Value,
        shape: ShapeId,
        slot: u32,
        into: Value,
    },
    /// `StoreField(obj, shape, slot, value)`: slot store with the store
    /// barrier `barrier` selects (section 9.3).
    StoreField {
        obj: Value,
        shape: ShapeId,
        slot: u32,
        value: Value,
        barrier: BarrierKind,
    },
    /// `AllocInst(cls, shape) -> into`: heap instance of `cls`.
    AllocInst {
        cls: ClassId,
        shape: ShapeId,
        into: Value,
    },
    /// `AllocTuple(items) -> into`: tuple allocation for a proven type.
    AllocTuple { items: Box<[Value]>, into: Value },
    /// `TypeGuard(value, ty) -> (ok, fail: Bool)`: `ok` is `value` narrowed
    /// to `ty`; `fail` tells the branch.
    TypeGuard {
        value: Value,
        ty: Type,
        ok: Value,
        fail: Value,
    },
    /// `ShapeGuard(obj, shape) -> (ok, fail: Bool)`.
    ShapeGuard {
        obj: Value,
        shape: ShapeId,
        ok: Value,
        fail: Value,
    },
    /// `BoundsCheck(index, len) -> into: Bool`: `0 <= index < len`.
    BoundsCheck {
        index: Value,
        len: Value,
        into: Value,
    },
    /// `ListGetUnchecked(list, index) -> into`: item load after a passed
    /// bounds check.
    ListGetUnchecked {
        list: Value,
        index: Value,
        into: Value,
    },
    /// `TupleGetUnchecked(tuple, index) -> into`: constant-index tuple load.
    TupleGetUnchecked {
        tuple: Value,
        index: u32,
        into: Value,
    },
    /// `StackAlloc(cls, shape) -> into`: non-escaping instance on the frame.
    StackAlloc {
        cls: ClassId,
        shape: ShapeId,
        into: Value,
    },

    // control
    /// `Yield(state, value) -> into`: suspend, yielding `value`; `into` is
    /// the value sent (or thrown) at resume; `state` names the resume point.
    /// Only in generator functions.
    Yield {
        state: u32,
        value: Value,
        into: Value,
    },
    /// `YieldFrom(state, iter, send) -> into`: delegate to `iter`;
    /// `into` is the result passed to `StopIteration`.
    YieldFrom {
        state: u32,
        iter: Value,
        send: Value,
        into: Value,
    },
    /// `Await(state, value) -> into`: only in coroutine functions.
    Await {
        state: u32,
        value: Value,
        into: Value,
    },
    /// `Safepoint`: poll for collection and the eval breaker; may raise
    /// `KeyboardInterrupt`.
    Safepoint,

    // exceptions
    /// `CatchBegin(types) -> (matched: Bool, exc)`: test the current
    /// exception against `types` (a boxed tuple of classes); `exc` is the
    /// exception instance.
    CatchBegin {
        types: Value,
        matched: Value,
        exc: Value,
    },
    /// `CatchEnd`: leave the handler; the exception is cleared.
    CatchEnd,
    /// `PushExcInfo() -> into`: push the current exception on the `exc_info`
    /// stack and yield it.
    PushExcInfo { into: Value },
    /// `PopExcInfo`: pop the `exc_info` stack.
    PopExcInfo,
    /// `FinallyEnter() -> into`: the `finally` reason slot, set to `None`.
    FinallyEnter { into: Value },
    /// `FinallyExit(reason)`: no result; `reason` is the reason slot
    /// (`None`, the exception, or the return value). Re-raises when the slot
    /// holds an exception, so it may raise.
    FinallyExit { reason: Value },
}

/// Whether the instruction can raise and therefore needs the after-call error
/// test lowered by the backend (section 12.4). Every may-raise instruction
/// must have a resolved handler (section 5, `verify.rs`).
impl Instr {
    pub fn may_raise(&self) -> bool {
        use Instr as I;
        matches!(
            self,
            I::Unbox { .. }
                | I::FinallyExit { .. }
                | I::IDivOvf { .. }
                | I::ShlOvf { .. }
                | I::IDiv { .. }
                | I::FDiv { .. }
                | I::FMod { .. }
                | I::IMod { .. }
                | I::Shl { .. }
                | I::Shr { .. }
                | I::BinOp { .. }
                | I::UnaryOp { .. }
                | I::Compare { .. }
                | I::GetAttr { .. }
                | I::SetAttr { .. }
                | I::DelAttr { .. }
                | I::GetItem { .. }
                | I::SetItem { .. }
                | I::DelItem { .. }
                | I::GetIter { .. }
                | I::Next { .. }
                | I::Call { .. }
                | I::CallEx { .. }
                | I::CallMethod { .. }
                | I::Truth { .. }
                | I::Len { .. }
                | I::Contains { .. }
                | I::Format { .. }
                | I::BuildDict { .. }
                | I::BuildSet { .. }
                | I::Unpack { .. }
                | I::Import { .. }
                | I::ImportStar { .. }
                | I::MakeClass { .. }
                | I::LoadCell { .. }
                | I::LoadGlobal { .. }
                | I::LoadName { .. }
                | I::CallDirect { .. }
                | I::Yield { .. }
                | I::YieldFrom { .. }
                | I::Await { .. }
                | I::Safepoint
        )
    }
}

/// The single terminator of a block.
#[derive(Clone, PartialEq, Debug)]
pub enum Terminator {
    /// `Jump(to)`: unconditional edge to `to`; no operands, no result.
    Jump { to: BlockId },
    /// `Branch(cond, then_to, else_to)`: `cond: Bool`.
    Branch {
        cond: Value,
        then_to: BlockId,
        else_to: BlockId,
    },
    /// `Switch(on, cases, default)`: `on: Int64`, cases are `(value, block)`.
    Switch {
        on: Value,
        cases: Box<[(i64, BlockId)]>,
        default: BlockId,
    },
    /// `Return(value)`: return `value` (boxed or unboxed per its type) to
    /// the caller; no result.
    Return { value: Value },
    /// `Raise(exc, cause)`: `exc: None` re-raises the current exception;
    /// `cause: None` chains implicitly, `const None` suppresses (`from None`).
    Raise {
        exc: Option<Value>,
        cause: Option<Value>,
    },
    /// `Reraise`: re-raise the in-flight exception, keeping its traceback.
    Reraise,
    /// `Unreachable`: provably dead control flow.
    Unreachable,
}

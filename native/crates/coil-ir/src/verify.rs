//! Verifier (spec section 5): SSA dominance over a dominator tree computed
//! here, a resolved handler for every may-raise instruction, boxed/unboxed
//! consistency, no `Top` in an unboxed operand, exactly one (well-formed)
//! terminator per block, `Yield` only in generator functions, and store
//! barriers consistent with the stored value's type. Every error names the
//! block and instruction. Exception edges are CFG edges for dominance.
//! Blocks unreachable from the entry are type-checked but not
//! dominance-checked: they have no dominators by definition.

use std::collections::HashMap;
use std::fmt;

use crate::ir::{BarrierKind, ClassDesc, FnFlags, Function, Instr, Module, Scalar, Terminator};
use crate::print::instr_results;
use crate::types::{BlockId, InstrId, Type, Value};

#[derive(Clone, PartialEq, Debug)]
pub enum Error {
    BadEntry {
        entry: BlockId,
        blocks: u32,
    },
    /// One terminator per block, and its targets must be real blocks.
    BadTerminator {
        block: BlockId,
        target: BlockId,
    },
    /// A may-raise instruction whose block's handler is not a distinct,
    /// existing block.
    UnresolvedHandler {
        block: BlockId,
        instr: InstrId,
        handler: BlockId,
    },
    ValueNotDefined {
        block: BlockId,
        instr: InstrId,
        value: Value,
    },
    ValueDefinedTwice {
        block: BlockId,
        instr: InstrId,
        value: Value,
    },
    MissingType {
        block: BlockId,
        instr: InstrId,
        value: Value,
    },
    OperandNotDominated {
        block: BlockId,
        instr: InstrId,
        value: Value,
        def_block: BlockId,
    },
    /// Phi errors carry the phi's value instead of an instruction id: phis
    /// are not part of the instruction numbering.
    PhiIncomingNotDominated {
        block: BlockId,
        value: Value,
        from: BlockId,
        incoming: Value,
        def_block: BlockId,
    },
    PhiPredecessorMismatch {
        block: BlockId,
        value: Value,
    },
    OperandType {
        block: BlockId,
        instr: InstrId,
        value: Value,
        expected: Type,
        found: Type,
    },
    ResultType {
        block: BlockId,
        instr: InstrId,
        value: Value,
        expected: Type,
        found: Type,
    },
    BoxedOperand {
        block: BlockId,
        instr: InstrId,
        value: Value,
        found: Type,
    },
    BoxedResult {
        block: BlockId,
        instr: InstrId,
        value: Value,
        found: Type,
    },
    TopUnboxedOperand {
        block: BlockId,
        instr: InstrId,
        value: Value,
    },
    YieldOutsideGenerator {
        block: BlockId,
        instr: InstrId,
    },
    BarrierMismatch {
        block: BlockId,
        instr: InstrId,
        value: Value,
        barrier: BarrierKind,
        found: Type,
    },
    /// A phi merges an unboxed incoming with a different type (boxed, or
    /// another unboxed kind); lowering must `Box`/`Unbox` before the merge.
    PhiBoxingMismatch {
        block: BlockId,
        value: Value,
        incoming: Value,
        found: Type,
    },
    /// A value defined at or after the first may-raise instruction of a block
    /// is used where only the exception edge to that block's handler leads:
    /// when the instruction raises, the value was never computed.
    UsedAfterRaise {
        block: BlockId,
        value: Value,
        def_block: BlockId,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = |b: BlockId, i: InstrId| format!("block {}, instr {}", b.0, i.0);
        match self {
            Error::BadEntry { entry, blocks } => {
                write!(
                    f,
                    "entry block {} out of range ({} blocks)",
                    entry.0, blocks
                )
            }
            Error::BadTerminator { block, target } => write!(
                f,
                "block {}: terminator targets nonexistent block {}",
                block.0, target.0
            ),
            Error::UnresolvedHandler {
                block,
                instr,
                handler,
            } => write!(
                f,
                "{}: may-raise instruction's handler does not resolve to a distinct block (handler {})",
                at(*block, *instr),
                handler.0
            ),
            Error::ValueNotDefined {
                block,
                instr,
                value,
            } => write!(
                f,
                "{}: operand v{} has no definition",
                at(*block, *instr),
                value.0
            ),
            Error::ValueDefinedTwice {
                block,
                instr,
                value,
            } => write!(
                f,
                "{}: v{} defined again (first definition in another instruction)",
                at(*block, *instr),
                value.0
            ),
            Error::MissingType {
                block,
                instr,
                value,
            } => write!(
                f,
                "{}: v{} has no type in the function's type table",
                at(*block, *instr),
                value.0
            ),
            Error::OperandNotDominated {
                block,
                instr,
                value,
                def_block,
            } => write!(
                f,
                "{}: operand v{} defined in non-dominating block {}",
                at(*block, *instr),
                value.0,
                def_block.0
            ),
            Error::PhiIncomingNotDominated {
                block,
                value,
                from,
                incoming,
                def_block,
            } => write!(
                f,
                "block {}, phi v{}: incoming v{} from block {} defined in non-dominating block {}",
                block.0, value.0, incoming.0, from.0, def_block.0
            ),
            Error::PhiPredecessorMismatch { block, value } => write!(
                f,
                "block {}, phi v{}: incomings do not match the block's predecessors",
                block.0, value.0
            ),
            Error::OperandType {
                block,
                instr,
                value,
                expected,
                found,
            } => write!(
                f,
                "{}: operand v{} must be {expected}, found {found}",
                at(*block, *instr),
                value.0
            ),
            Error::ResultType {
                block,
                instr,
                value,
                expected,
                found,
            } => write!(
                f,
                "{}: result v{} must be {expected}, found {found}",
                at(*block, *instr),
                value.0
            ),
            Error::BoxedOperand {
                block,
                instr,
                value,
                found,
            } => write!(
                f,
                "{}: operand v{} must be boxed, found unboxed {found}",
                at(*block, *instr),
                value.0
            ),
            Error::BoxedResult {
                block,
                instr,
                value,
                found,
            } => write!(
                f,
                "{}: result v{} must be boxed, found unboxed {found}",
                at(*block, *instr),
                value.0
            ),
            Error::TopUnboxedOperand {
                block,
                instr,
                value,
            } => write!(
                f,
                "{}: unboxed operand v{} has type Top",
                at(*block, *instr),
                value.0
            ),
            Error::YieldOutsideGenerator { block, instr } => write!(
                f,
                "{}: yield outside a generator function",
                at(*block, *instr)
            ),
            Error::BarrierMismatch {
                block,
                instr,
                value,
                barrier,
                found,
            } => write!(
                f,
                "{}: store barrier {barrier:?} inconsistent with stored type {found} of v{}",
                at(*block, *instr),
                value.0
            ),
            Error::PhiBoxingMismatch {
                block,
                value,
                incoming,
                found,
            } => write!(
                f,
                "b{}: phi v{} merges v{} of type {found}, a boxed/unboxed mismatch; box or unbox before the merge",
                block.0, value.0, incoming.0
            ),
            Error::UsedAfterRaise {
                block,
                value,
                def_block,
            } => write!(
                f,
                "b{}: v{} is defined in b{} at or after a may-raise instruction, so it is undefined on the exception edge",
                block.0, value.0, def_block.0
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Verify every function of the module, returning the first error.
pub fn verify_module(m: &Module) -> Result<(), Error> {
    for f in &m.functions {
        verify_function(f, &m.classes)?;
    }
    Ok(())
}

/// Verify one function against the module's class table: instance types
/// resolve their regime through it (spec 8.3).
pub fn verify_function(f: &Function, classes: &[ClassDesc]) -> Result<(), Error> {
    let n = f.blocks.len() as u32;
    if n == 0 || f.entry.0 >= n {
        return Err(Error::BadEntry {
            entry: f.entry,
            blocks: n,
        });
    }
    let arity = f.params.arity() as u32;

    // Instruction ids across blocks, phis excluded, as in print.rs.
    let mut offset = Vec::with_capacity(f.blocks.len() + 1);
    let mut acc = 0u32;
    for b in &f.blocks {
        offset.push(acc);
        acc += b.instrs.len() as u32;
    }
    offset.push(acc);

    // Where each value is defined. Parameters are values 0..arity and are
    // defined at the entry by fiat.
    let mut defs: HashMap<u32, (BlockId, Option<u32>)> = HashMap::new();
    if f.types.len() < arity as usize {
        return Err(Error::MissingType {
            block: f.entry,
            instr: InstrId(0),
            value: Value(f.types.len() as u32),
        });
    }
    for v in 0..arity {
        defs.insert(v, (f.entry, None));
    }
    for (bi, b) in f.blocks.iter().enumerate() {
        let block = BlockId(bi as u32);
        for p in &b.phis {
            if defs.insert(p.into.0, (block, None)).is_some() {
                return Err(Error::ValueDefinedTwice {
                    block,
                    instr: InstrId(offset[bi]),
                    value: p.into,
                });
            }
        }
        for (ii, ins) in b.instrs.iter().enumerate() {
            for r in instr_results(ins) {
                if defs.insert(r.0, (block, Some(ii as u32))).is_some() {
                    return Err(Error::ValueDefinedTwice {
                        block,
                        instr: InstrId(offset[bi] + ii as u32),
                        value: r,
                    });
                }
            }
        }
    }

    // Structural pass first: the CFG walk below indexes by block id. This is
    // the one-terminator rule (a block's single terminator may only name
    // real blocks) and the resolved-handler rule; a handler of None
    // propagates to the caller.
    for (bi, b) in f.blocks.iter().enumerate() {
        let block = BlockId(bi as u32);
        for t in term_targets(&b.term) {
            if t.0 >= n {
                return Err(Error::BadTerminator { block, target: t });
            }
        }
        if let Some(h) = b.handler
            && (h.0 >= n || h == block)
        {
            if let Some((ii, _)) = b.instrs.iter().enumerate().find(|(_, i)| i.may_raise()) {
                return Err(Error::UnresolvedHandler {
                    block,
                    instr: InstrId(offset[bi] + ii as u32),
                    handler: h,
                });
            }
            return Err(Error::BadTerminator { block, target: h });
        }
    }

    let preds = predecessors(f);
    let dom = Dominators::new(f);

    for (bi, b) in f.blocks.iter().enumerate() {
        let block = BlockId(bi as u32);
        let reachable = dom.order[bi].is_some();

        // Phis: one incoming per predecessor, each defined where it
        // dominates the end of that predecessor.
        let mut want: Vec<BlockId> = preds[bi].clone();
        want.sort_unstable_by_key(|b| b.0);
        for p in &b.phis {
            if f.types.get(p.into.idx()).is_none() {
                return Err(Error::MissingType {
                    block,
                    instr: InstrId(offset[bi]),
                    value: p.into,
                });
            }
            let mut have: Vec<BlockId> = p.incomings.iter().map(|(_, from)| *from).collect();
            have.sort_unstable_by_key(|b| b.0);
            if have != want {
                return Err(Error::PhiPredecessorMismatch {
                    block,
                    value: p.into,
                });
            }
            for (v, from) in p.incomings.iter() {
                // An unreachable predecessor never executes; anything may
                // flow from it.
                let from_reachable = dom.order[from.0 as usize].is_some();
                match defs.get(&v.0) {
                    None => {
                        if v.0 >= arity && from_reachable {
                            return Err(Error::ValueNotDefined {
                                block,
                                instr: InstrId(offset[bi]),
                                value: *v,
                            });
                        }
                    }
                    Some((db, dpos)) => {
                        if !def_dominates_use(&dom, *db, *dpos, *from, usize::MAX, from_reachable) {
                            return Err(Error::PhiIncomingNotDominated {
                                block,
                                value: p.into,
                                from: *from,
                                incoming: *v,
                                def_block: *db,
                            });
                        }
                        // The edge from `from` to its handler leaves at the
                        // first may-raise instruction, not at the block end.
                        if from_reachable
                            && *db == *from
                            && exception_edge_only(f, *from, block)
                            && after_first_raise(f, *db, *dpos)
                        {
                            return Err(Error::UsedAfterRaise {
                                block,
                                value: *v,
                                def_block: *db,
                            });
                        }
                    }
                }
                let (pt, it) = (&f.types[p.into.idx()], f.types.get(v.idx()));
                if let Some(it) = it
                    && !is_bottom(pt)
                    && !is_bottom(it)
                    && (is_unboxed(pt) || is_unboxed(it))
                    && pt != it
                {
                    return Err(Error::PhiBoxingMismatch {
                        block,
                        value: p.into,
                        incoming: *v,
                        found: it.clone(),
                    });
                }
            }
        }

        // Instructions: types per family, then dominance of every operand.
        for (ii, ins) in b.instrs.iter().enumerate() {
            let id = InstrId(offset[bi] + ii as u32);
            let mut ops = Vec::new();
            check_types(f, classes, block, id, ins, &mut ops)?;
            if reachable {
                for v in ops {
                    match defs.get(&v.0) {
                        None => {
                            if v.0 >= arity {
                                return Err(Error::ValueNotDefined {
                                    block,
                                    instr: id,
                                    value: v,
                                });
                            }
                        }
                        Some((db, dpos)) => {
                            if !def_dominates_use(&dom, *db, *dpos, block, ii, true) {
                                return Err(Error::OperandNotDominated {
                                    block,
                                    instr: id,
                                    value: v,
                                    def_block: *db,
                                });
                            }
                            if late_for_handler(f, &dom, *db, *dpos, block) {
                                return Err(Error::UsedAfterRaise {
                                    block,
                                    value: v,
                                    def_block: *db,
                                });
                            }
                        }
                    }
                }
            }
        }

        // The terminator's operands are used at the block's end. Types are
        // checked in unreachable blocks too, as instruction types are.
        let mut ops = Vec::new();
        check_term_types(
            f,
            classes,
            block,
            InstrId(offset[bi] + b.instrs.len() as u32),
            &b.term,
            &mut ops,
        )?;
        if reachable {
            for v in ops {
                match defs.get(&v.0) {
                    None => {
                        if v.0 >= arity {
                            return Err(Error::ValueNotDefined {
                                block,
                                instr: InstrId(offset[bi] + b.instrs.len() as u32),
                                value: v,
                            });
                        }
                    }
                    Some((db, dpos)) => {
                        if !def_dominates_use(&dom, *db, *dpos, block, usize::MAX, true) {
                            return Err(Error::OperandNotDominated {
                                block,
                                instr: InstrId(offset[bi] + b.instrs.len() as u32),
                                value: v,
                                def_block: *db,
                            });
                        }
                        if late_for_handler(f, &dom, *db, *dpos, block) {
                            return Err(Error::UsedAfterRaise {
                                block,
                                value: v,
                                def_block: *db,
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Whether the definition at `def_block` (instruction `dpos`, `None` for a
/// phi) reaches the use at `block`, instruction `upos` (`usize::MAX` = the
/// block's end, as for terminators and phi incomings).
fn def_dominates_use(
    dom: &Dominators,
    def_block: BlockId,
    dpos: Option<u32>,
    block: BlockId,
    upos: usize,
    reachable: bool,
) -> bool {
    if !reachable {
        return true;
    }
    if def_block == block {
        return match dpos {
            None => true,
            Some(d) => upos == usize::MAX || (d as usize) < upos,
        };
    }
    dom.dominates(def_block, block)
}

/// Whether the definition at instruction `dpos` of `block` comes at or after
/// the block's first may-raise instruction (phis, `None`, come before it).
fn after_first_raise(f: &Function, block: BlockId, dpos: Option<u32>) -> bool {
    let Some(d) = dpos else { return false };
    f.blocks[block.0 as usize]
        .instrs
        .iter()
        .position(Instr::may_raise)
        .is_some_and(|first| d as usize >= first)
}

/// Whether `to` is reached from `from` only through `from`'s exception edge.
fn exception_edge_only(f: &Function, from: BlockId, to: BlockId) -> bool {
    let b = &f.blocks[from.0 as usize];
    b.handler == Some(to) && !term_targets(&b.term).contains(&to)
}

/// Whether a value defined in `def_block` after its first may-raise
/// instruction is used in `block`, which every path reaches through
/// `def_block`'s handler and only over the exception edge.
fn late_for_handler(
    f: &Function,
    dom: &Dominators,
    def_block: BlockId,
    dpos: Option<u32>,
    block: BlockId,
) -> bool {
    let Some(h) = f.blocks[def_block.0 as usize].handler else {
        return false;
    };
    def_block != block
        && exception_edge_only(f, def_block, h)
        && dom.dominates(h, block)
        // Inside the handler's own region the value can also arrive normally.
        && !dom.dominates(h, def_block)
        && after_first_raise(f, def_block, dpos)
}

fn term_targets(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Jump { to } => vec![*to],
        Terminator::Branch {
            then_to, else_to, ..
        } => vec![*then_to, *else_to],
        Terminator::Switch { cases, default, .. } => {
            let mut ts: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            ts.push(*default);
            ts
        }
        _ => Vec::new(),
    }
}

/// Terminator targets and exception edges.
fn successors(f: &Function) -> Vec<Vec<BlockId>> {
    let mut succs = Vec::with_capacity(f.blocks.len());
    for b in &f.blocks {
        let mut ss = term_targets(&b.term);
        if let Some(h) = b.handler {
            ss.push(h);
        }
        ss.sort_unstable_by_key(|b| b.0);
        ss.dedup();
        succs.push(ss);
    }
    succs
}

/// Terminator sources and exception edges, deduplicated, in block order.
fn predecessors(f: &Function) -> Vec<Vec<BlockId>> {
    let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); f.blocks.len()];
    for (i, b) in f.blocks.iter().enumerate() {
        let from = BlockId(i as u32);
        for t in term_targets(&b.term) {
            let p = &mut preds[t.0 as usize];
            if !p.contains(&from) {
                p.push(from);
            }
        }
        if let Some(h) = b.handler {
            let p = &mut preds[h.0 as usize];
            if !p.contains(&from) {
                p.push(from);
            }
        }
    }
    preds
}

/// Immediate dominators over the blocks reachable from the entry, by the
/// Cooper-Harvey-Kennedy iteration on reverse postorder.
struct Dominators {
    idom: Vec<Option<BlockId>>,
    order: Vec<Option<u32>>,
}

impl Dominators {
    fn new(f: &Function) -> Self {
        let n = f.blocks.len();
        let succs = successors(f);

        // Reverse postorder by depth-first search from the entry.
        let mut post: Vec<BlockId> = Vec::new();
        let mut seen = vec![false; n];
        let mut stack: Vec<(BlockId, usize)> = vec![(f.entry, 0)];
        seen[f.entry.0 as usize] = true;
        while let Some(&mut (b, ref mut next)) = stack.last_mut() {
            let ss = &succs[b.0 as usize];
            if *next < ss.len() {
                let s = ss[*next];
                *next += 1;
                if !seen[s.0 as usize] {
                    seen[s.0 as usize] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
                stack.pop();
            }
        }
        let rpo: Vec<BlockId> = post.iter().rev().copied().collect();
        let mut order = vec![None; n];
        for (i, b) in rpo.iter().enumerate() {
            order[b.0 as usize] = Some(i as u32);
        }

        let preds = predecessors(f);
        let mut idom: Vec<Option<BlockId>> = vec![None; n];
        idom[f.entry.0 as usize] = Some(f.entry);
        let mut changed = true;
        while changed {
            changed = false;
            for &b in rpo.iter().skip(1) {
                let mut new_idom = None;
                for p in &preds[b.0 as usize] {
                    let p = *p;
                    if idom[p.0 as usize].is_none() {
                        continue;
                    }
                    new_idom = Some(match new_idom {
                        None => p,
                        Some(cur) => Self::intersect(&idom, &order, p, cur),
                    });
                }
                if new_idom != idom[b.0 as usize] {
                    idom[b.0 as usize] = new_idom;
                    changed = true;
                }
            }
        }
        Self { idom, order }
    }

    fn intersect(
        idom: &[Option<BlockId>],
        order: &[Option<u32>],
        mut a: BlockId,
        mut b: BlockId,
    ) -> BlockId {
        while a != b {
            while order[a.0 as usize] > order[b.0 as usize] {
                a = idom[a.0 as usize].unwrap();
            }
            while order[b.0 as usize] > order[a.0 as usize] {
                b = idom[b.0 as usize].unwrap();
            }
        }
        a
    }

    fn dominates(&self, a: BlockId, b: BlockId) -> bool {
        let mut cur = b;
        loop {
            if cur == a {
                return true;
            }
            match self.idom[cur.0 as usize] {
                Some(next) if next != cur => cur = next,
                _ => return false,
            }
        }
    }
}

/// The store barrier a type requires: a tracing-regime type needs `None`, a
/// statically counted value `Ext`, and a type that could be either `Dynamic`
/// (section 9.3).
fn regime(classes: &[ClassDesc], ty: &Type) -> BarrierKind {
    match ty {
        Type::Ext(_) => BarrierKind::Ext,
        Type::Top => BarrierKind::Dynamic,
        // Instances of a compiled class with an extension base are counted
        // too (spec 8.3); a class id that names no class is unknown.
        Type::Inst { cls, .. } => match classes.get(cls.0 as usize) {
            Some(c) if c.has_ext_base => BarrierKind::Ext,
            Some(_) => BarrierKind::None,
            None => BarrierKind::Dynamic,
        },
        Type::Union(parts) => {
            let mut ext = false;
            let mut tracing = false;
            let mut dynamic = false;
            for p in parts.iter() {
                match regime(classes, p) {
                    BarrierKind::Ext => ext = true,
                    BarrierKind::None => tracing = true,
                    BarrierKind::Dynamic => dynamic = true,
                }
            }
            if dynamic || (ext && tracing) {
                BarrierKind::Dynamic
            } else if ext {
                BarrierKind::Ext
            } else {
                BarrierKind::None
            }
        }
        _ => BarrierKind::None,
    }
}

fn scalar_ty(s: Scalar) -> Type {
    match s {
        Scalar::Int64 => Type::Int64,
        Scalar::Float64 => Type::Float64,
        Scalar::Bool => Type::Bool,
    }
}

fn is_unboxed(t: &Type) -> bool {
    matches!(t, Type::Int64 | Type::Float64 | Type::Bool)
}

fn is_bottom(t: &Type) -> bool {
    matches!(t, Type::Bottom)
}

/// One instruction's operand and result types, per the contracts in `ir.rs`.
/// Every operand is collected into `ops` for the dominance pass.
struct Chk<'a> {
    f: &'a Function,
    classes: &'a [ClassDesc],
    block: BlockId,
    id: InstrId,
    ops: &'a mut Vec<Value>,
}

impl Chk<'_> {
    fn use_any(&mut self, v: Value) -> Result<(), Error> {
        self.ops.push(v);
        if self.f.types.get(v.idx()).is_none() {
            return Err(Error::MissingType {
                block: self.block,
                instr: self.id,
                value: v,
            });
        }
        Ok(())
    }

    fn use_unboxed(&mut self, v: Value, s: Scalar) -> Result<(), Error> {
        self.use_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_bottom(t) {
            return Ok(());
        }
        if t.is_top() {
            return Err(Error::TopUnboxedOperand {
                block: self.block,
                instr: self.id,
                value: v,
            });
        }
        let want = scalar_ty(s);
        if *t != want {
            return Err(Error::OperandType {
                block: self.block,
                instr: self.id,
                value: v,
                expected: want,
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn use_boxed(&mut self, v: Value) -> Result<(), Error> {
        self.use_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_unboxed(t) {
            return Err(Error::BoxedOperand {
                block: self.block,
                instr: self.id,
                value: v,
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn use_exact(&mut self, v: Value, want: Type) -> Result<(), Error> {
        self.use_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_bottom(t) {
            return Ok(());
        }
        if *t != want {
            return Err(Error::OperandType {
                block: self.block,
                instr: self.id,
                value: v,
                expected: want,
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn def_any(&mut self, v: Value) -> Result<(), Error> {
        if self.f.types.get(v.idx()).is_none() {
            return Err(Error::MissingType {
                block: self.block,
                instr: self.id,
                value: v,
            });
        }
        Ok(())
    }

    fn def_exact(&mut self, v: Value, want: Type) -> Result<(), Error> {
        self.def_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_bottom(t) {
            return Ok(());
        }
        if *t != want {
            return Err(Error::ResultType {
                block: self.block,
                instr: self.id,
                value: v,
                expected: want,
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn def_boxed(&mut self, v: Value) -> Result<(), Error> {
        self.def_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_unboxed(t) {
            return Err(Error::BoxedResult {
                block: self.block,
                instr: self.id,
                value: v,
                found: t.clone(),
            });
        }
        Ok(())
    }

    /// The result carries the same type as `other` (`Copy`, shape-guarded
    /// values).
    fn def_same(&mut self, v: Value, other: Value) -> Result<(), Error> {
        self.def_any(v)?;
        let want = self.f.types[other.idx()].clone();
        self.def_exact(v, want)
    }

    /// A tuple of any element types; inference refines the elements.
    fn def_tuple(&mut self, v: Value) -> Result<(), Error> {
        self.def_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_bottom(t) {
            return Ok(());
        }
        if !matches!(t, Type::Tuple(_)) {
            return Err(Error::ResultType {
                block: self.block,
                instr: self.id,
                value: v,
                expected: Type::Tuple(Box::from([])),
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn use_tuple(&mut self, v: Value) -> Result<(), Error> {
        self.use_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_bottom(t) {
            return Ok(());
        }
        if !matches!(t, Type::Tuple(_)) {
            return Err(Error::OperandType {
                block: self.block,
                instr: self.id,
                value: v,
                expected: Type::Tuple(Box::from([])),
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn def_inst(&mut self, v: Value, cls: crate::types::ClassId) -> Result<(), Error> {
        self.def_any(v)?;
        let t = &self.f.types[v.idx()];
        if is_bottom(t) {
            return Ok(());
        }
        if !matches!(t, Type::Inst { cls: c, .. } if *c == cls) {
            return Err(Error::ResultType {
                block: self.block,
                instr: self.id,
                value: v,
                expected: Type::Inst { cls, exact: true },
                found: t.clone(),
            });
        }
        Ok(())
    }

    fn barrier(&mut self, v: Value, barrier: BarrierKind) -> Result<(), Error> {
        let t = self.f.types[v.idx()].clone();
        // `Dynamic` tests the regime at run time, so it is correct for any
        // stored type; the others must match it exactly.
        if barrier != BarrierKind::Dynamic && regime(self.classes, &t) != barrier {
            return Err(Error::BarrierMismatch {
                block: self.block,
                instr: self.id,
                value: v,
                barrier,
                found: t,
            });
        }
        Ok(())
    }
}

fn check_types(
    f: &Function,
    classes: &[ClassDesc],
    block: BlockId,
    id: InstrId,
    ins: &Instr,
    ops: &mut Vec<Value>,
) -> Result<(), Error> {
    use Instr as I;
    let mut c = Chk {
        f,
        classes,
        block,
        id,
        ops,
    };
    let b = Type::Bool;
    match ins {
        I::Const { into, .. } => c.def_any(*into)?,
        I::Copy { src, into } => {
            c.use_any(*src)?;
            c.def_same(*into, *src)?;
        }
        I::IAddOvf {
            lhs,
            rhs,
            into,
            ovf,
        }
        | I::ISubOvf {
            lhs,
            rhs,
            into,
            ovf,
        }
        | I::IMulOvf {
            lhs,
            rhs,
            into,
            ovf,
        }
        | I::IDivOvf {
            lhs,
            rhs,
            into,
            ovf,
        }
        | I::ShlOvf {
            lhs,
            rhs,
            into,
            ovf,
        } => {
            c.use_unboxed(*lhs, Scalar::Int64)?;
            c.use_unboxed(*rhs, Scalar::Int64)?;
            c.def_exact(*into, Type::Int64)?;
            c.def_exact(*ovf, b)?;
        }
        I::INegOvf { src, into, ovf } => {
            c.use_unboxed(*src, Scalar::Int64)?;
            c.def_exact(*into, Type::Int64)?;
            c.def_exact(*ovf, b)?;
        }
        I::IAdd { lhs, rhs, into }
        | I::ISub { lhs, rhs, into }
        | I::IMul { lhs, rhs, into }
        | I::IDiv { lhs, rhs, into }
        | I::IMod { lhs, rhs, into }
        | I::Shl { lhs, rhs, into }
        | I::Shr { lhs, rhs, into }
        | I::BitAnd { lhs, rhs, into }
        | I::BitOr { lhs, rhs, into }
        | I::BitXor { lhs, rhs, into } => {
            c.use_unboxed(*lhs, Scalar::Int64)?;
            c.use_unboxed(*rhs, Scalar::Int64)?;
            c.def_exact(*into, Type::Int64)?;
        }
        I::INeg { src, into } | I::BitNot { src, into } => {
            c.use_unboxed(*src, Scalar::Int64)?;
            c.def_exact(*into, Type::Int64)?;
        }
        I::ICmp { lhs, rhs, into, .. } => {
            c.use_unboxed(*lhs, Scalar::Int64)?;
            c.use_unboxed(*rhs, Scalar::Int64)?;
            c.def_exact(*into, b)?;
        }
        I::FAdd { lhs, rhs, into }
        | I::FSub { lhs, rhs, into }
        | I::FMul { lhs, rhs, into }
        | I::FDiv { lhs, rhs, into }
        | I::FMod { lhs, rhs, into } => {
            c.use_unboxed(*lhs, Scalar::Float64)?;
            c.use_unboxed(*rhs, Scalar::Float64)?;
            c.def_exact(*into, Type::Float64)?;
        }
        I::FNeg { src, into } => {
            c.use_unboxed(*src, Scalar::Float64)?;
            c.def_exact(*into, Type::Float64)?;
        }
        I::FCmp { lhs, rhs, into, .. } => {
            c.use_unboxed(*lhs, Scalar::Float64)?;
            c.use_unboxed(*rhs, Scalar::Float64)?;
            c.def_exact(*into, b)?;
        }
        I::Box { src, scalar, into } => {
            c.use_unboxed(*src, *scalar)?;
            c.def_boxed(*into)?;
        }
        I::Unbox { src, scalar, into } => {
            c.use_boxed(*src)?;
            c.def_exact(*into, scalar_ty(*scalar))?;
        }
        I::BinOp { lhs, rhs, into, .. } => {
            c.use_boxed(*lhs)?;
            c.use_boxed(*rhs)?;
            c.def_boxed(*into)?;
        }
        I::UnaryOp { operand, into, .. } => {
            c.use_boxed(*operand)?;
            c.def_boxed(*into)?;
        }
        I::Compare { lhs, rhs, into, .. } => {
            c.use_boxed(*lhs)?;
            c.use_boxed(*rhs)?;
            c.def_exact(*into, b)?;
        }
        I::GetAttr { obj, into, .. } => {
            c.use_boxed(*obj)?;
            c.def_boxed(*into)?;
        }
        I::SetAttr { obj, value, .. } => {
            c.use_boxed(*obj)?;
            c.use_boxed(*value)?;
        }
        I::DelAttr { obj, .. } => c.use_boxed(*obj)?,
        I::GetItem { obj, key, into } => {
            c.use_boxed(*obj)?;
            c.use_boxed(*key)?;
            c.def_boxed(*into)?;
        }
        I::SetItem { obj, key, value } => {
            c.use_boxed(*obj)?;
            c.use_boxed(*key)?;
            c.use_boxed(*value)?;
        }
        I::DelItem { obj, key } => {
            c.use_boxed(*obj)?;
            c.use_boxed(*key)?;
        }
        I::GetIter { obj, into } => {
            c.use_boxed(*obj)?;
            c.def_boxed(*into)?;
        }
        I::Next { iter, into } => {
            c.use_boxed(*iter)?;
            c.def_boxed(*into)?;
        }
        I::Call {
            callee,
            args,
            kwnames,
            into,
        } => {
            c.use_boxed(*callee)?;
            for a in args.iter() {
                c.use_boxed(*a)?;
            }
            for (_, v) in kwnames.iter() {
                c.use_boxed(*v)?;
            }
            c.def_boxed(*into)?;
        }
        I::CallMethod {
            recv,
            args,
            kwnames,
            into,
            ..
        } => {
            c.use_boxed(*recv)?;
            for a in args.iter() {
                c.use_boxed(*a)?;
            }
            for (_, v) in kwnames.iter() {
                c.use_boxed(*v)?;
            }
            c.def_boxed(*into)?;
        }
        I::CallEx {
            callee,
            args,
            kwargs,
            into,
        } => {
            c.use_boxed(*callee)?;
            c.use_boxed(*args)?;
            c.use_boxed(*kwargs)?;
            c.def_boxed(*into)?;
        }
        I::Truth { value, into } => {
            c.use_boxed(*value)?;
            c.def_exact(*into, b)?;
        }
        I::Len { value, into } => {
            c.use_boxed(*value)?;
            c.def_exact(*into, Type::Int64)?;
        }
        I::Contains {
            haystack,
            needle,
            into,
        } => {
            c.use_boxed(*haystack)?;
            c.use_boxed(*needle)?;
            c.def_exact(*into, b)?;
        }
        I::Format { value, spec, into } => {
            c.use_boxed(*value)?;
            c.use_boxed(*spec)?;
            c.def_boxed(*into)?;
        }
        I::BuildList { items, into } => {
            for v in items.iter() {
                c.use_boxed(*v)?;
            }
            c.def_exact(*into, Type::List)?;
        }
        I::BuildTuple { items, into } | I::AllocTuple { items, into } => {
            for v in items.iter() {
                c.use_boxed(*v)?;
            }
            c.def_tuple(*into)?;
        }
        I::BuildDict { items, into } => {
            for (k, v) in items.iter() {
                c.use_boxed(*k)?;
                c.use_boxed(*v)?;
            }
            c.def_exact(*into, Type::Dict)?;
        }
        I::BuildSet { items, into } => {
            for v in items.iter() {
                c.use_boxed(*v)?;
            }
            c.def_exact(*into, Type::Set)?;
        }
        I::BuildStr { parts, into } => {
            for v in parts.iter() {
                c.use_boxed(*v)?;
            }
            c.def_exact(*into, Type::Str)?;
        }
        I::BuildSlice {
            start,
            stop,
            step,
            into,
        } => {
            if let Some(v) = start {
                c.use_boxed(*v)?;
            }
            c.use_boxed(*stop)?;
            if let Some(v) = step {
                c.use_boxed(*v)?;
            }
            c.def_boxed(*into)?;
        }
        I::Unpack { src, into, .. } => {
            c.use_boxed(*src)?;
            for v in into.iter() {
                c.def_boxed(*v)?;
            }
        }
        I::Import {
            fromlist,
            level,
            into,
            ..
        } => {
            c.use_boxed(*fromlist)?;
            c.use_boxed(*level)?;
            c.def_boxed(*into)?;
        }
        I::ImportStar { module } => c.use_boxed(*module)?,
        I::MakeFunction {
            cells,
            defaults,
            into,
            ..
        } => {
            for v in cells.iter() {
                c.use_boxed(*v)?;
            }
            for v in defaults.iter() {
                c.use_boxed(*v)?;
            }
            c.def_boxed(*into)?;
        }
        I::MakeClass {
            bases,
            cells,
            kwds,
            into,
            ..
        } => {
            for v in bases.iter() {
                c.use_boxed(*v)?;
            }
            for v in cells.iter() {
                c.use_boxed(*v)?;
            }
            c.use_boxed(*kwds)?;
            c.def_boxed(*into)?;
        }
        I::MakeCell { initial, into } => {
            if let Some(v) = initial {
                c.use_boxed(*v)?;
            }
            c.def_boxed(*into)?;
        }
        I::LoadCell { cell, into } => {
            c.use_boxed(*cell)?;
            c.def_boxed(*into)?;
        }
        I::StoreCell { cell, value } => {
            c.use_boxed(*cell)?;
            c.use_boxed(*value)?;
        }
        I::LoadGlobal { into, .. } | I::LoadName { into, .. } => c.def_boxed(*into)?,
        I::StoreGlobal { value, .. } | I::StoreName { value, .. } => c.use_boxed(*value)?,
        I::CallDirect { args, into, .. } => {
            for v in args.iter() {
                c.use_boxed(*v)?;
            }
            c.def_boxed(*into)?;
        }
        I::LoadField { obj, into, .. } => {
            c.use_boxed(*obj)?;
            c.def_boxed(*into)?;
        }
        I::StoreField {
            obj,
            value,
            barrier,
            ..
        } => {
            c.use_boxed(*obj)?;
            c.use_boxed(*value)?;
            c.barrier(*value, *barrier)?;
        }
        I::AllocInst { cls, into, .. } | I::StackAlloc { cls, into, .. } => {
            c.def_inst(*into, *cls)?;
        }
        I::TypeGuard {
            value,
            ty,
            ok,
            fail,
        } => {
            c.use_any(*value)?;
            c.def_exact(*ok, ty.clone())?;
            c.def_exact(*fail, b)?;
        }
        I::ShapeGuard { obj, ok, fail, .. } => {
            c.use_any(*obj)?;
            c.def_same(*ok, *obj)?;
            c.def_exact(*fail, b)?;
        }
        I::BoundsCheck { index, len, into } => {
            c.use_unboxed(*index, Scalar::Int64)?;
            c.use_unboxed(*len, Scalar::Int64)?;
            c.def_exact(*into, b)?;
        }
        I::ListGetUnchecked { list, index, into } => {
            c.use_exact(*list, Type::List)?;
            c.use_unboxed(*index, Scalar::Int64)?;
            c.def_boxed(*into)?;
        }
        I::TupleGetUnchecked { tuple, into, .. } => {
            c.use_tuple(*tuple)?;
            c.def_boxed(*into)?;
        }
        I::Yield { value, into, .. } => {
            if !f.flags.contains(FnFlags::GENERATOR) && !f.flags.contains(FnFlags::ASYNC_GEN) {
                return Err(Error::YieldOutsideGenerator {
                    block: c.block,
                    instr: c.id,
                });
            }
            c.use_boxed(*value)?;
            c.def_boxed(*into)?;
        }
        I::YieldFrom {
            iter, send, into, ..
        } => {
            c.use_boxed(*iter)?;
            c.use_boxed(*send)?;
            c.def_boxed(*into)?;
        }
        I::Await { value, into, .. } => {
            c.use_boxed(*value)?;
            c.def_boxed(*into)?;
        }
        I::Safepoint | I::CatchEnd | I::PopExcInfo => {}
        I::CatchBegin {
            types,
            matched,
            exc,
        } => {
            c.use_boxed(*types)?;
            c.def_exact(*matched, b)?;
            c.def_boxed(*exc)?;
        }
        I::PushExcInfo { into } => c.def_boxed(*into)?,
        I::FinallyEnter { into } => c.def_exact(*into, Type::None)?,
        I::FinallyExit { reason } => c.use_boxed(*reason)?,
    }
    Ok(())
}

fn check_term_types(
    f: &Function,
    classes: &[ClassDesc],
    block: BlockId,
    id: InstrId,
    t: &Terminator,
    ops: &mut Vec<Value>,
) -> Result<(), Error> {
    let mut c = Chk {
        f,
        classes,
        block,
        id,
        ops,
    };
    match t {
        Terminator::Jump { .. } | Terminator::Reraise | Terminator::Unreachable => {}
        Terminator::Branch { cond, .. } => c.use_unboxed(*cond, Scalar::Bool)?,
        Terminator::Switch { on, .. } => c.use_unboxed(*on, Scalar::Int64)?,
        Terminator::Return { value } => c.use_any(*value)?,
        Terminator::Raise { exc, cause } => {
            if let Some(v) = exc {
                c.use_any(*v)?;
            }
            if let Some(v) = cause {
                c.use_any(*v)?;
            }
        }
    }
    Ok(())
}

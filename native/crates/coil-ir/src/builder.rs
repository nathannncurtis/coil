//! SSA construction on the fly, after Braun, Buchwald, Hack, Leißa, Mallon
//! and Zwinkau, "Simple and Efficient Construction of Static Single
//! Assignment Form" (CC 2013): no dominance frontiers, the CFG is used as
//! emitted. Blocks start unsealed; `seal_block` records that every
//! predecessor (including exception edges) is known. A handler edge counts
//! as a predecessor of its handler block, so phis there merge the raising
//! blocks' definitions as of the *end* of the raising block; lowering spills
//! values it needs live-in to a handler before a may-raise instruction.

use std::collections::HashMap;
use std::fmt;

use crate::ir::{Block, FnFlags, Function, Instr, LocalMeta, Params, Phi, Terminator};
use crate::print::instr_results;
use crate::types::{BlockId, InstrId, Sym, Type, Value};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Error {
    ParamCount {
        expected: usize,
        given: usize,
    },
    UnknownBlock {
        block: BlockId,
    },
    UnknownValue {
        value: Value,
    },
    UnknownInstr {
        instr: InstrId,
    },
    NoCurrentBlock,
    MissingTerminator {
        block: BlockId,
    },
    UnsealedBlock {
        block: BlockId,
    },
    /// The variable is read on a path where nothing has written it; the
    /// front end must write an explicit unbound marker first.
    UndefinedVariable {
        var: Sym,
        block: BlockId,
    },
    /// Sealing fixes a block's predecessor set, so no edge may be added to
    /// or removed from a block that is already sealed.
    EdgeIntoSealed {
        from: BlockId,
        to: BlockId,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ParamCount { expected, given } => write!(
                f,
                "function takes {expected} parameters but {given} parameter types were given"
            ),
            Error::UnknownBlock { block } => write!(f, "unknown block {}", block.0),
            Error::UnknownValue { value } => write!(f, "unknown value v{}", value.0),
            Error::UnknownInstr { instr } => write!(f, "unknown instruction {}", instr.0),
            Error::NoCurrentBlock => write!(f, "no current block to emit into"),
            Error::MissingTerminator { block } => write!(f, "block {} has no terminator", block.0),
            Error::UnsealedBlock { block } => write!(f, "block {} is not sealed", block.0),
            Error::UndefinedVariable { var, block } => {
                write!(
                    f,
                    "variable {} read but never written before block {}",
                    var.0, block.0
                )
            }
            Error::EdgeIntoSealed { from, to } => {
                write!(f, "edge from block {} into sealed block {}", from.0, to.0)
            }
        }
    }
}

impl std::error::Error for Error {}

struct Blk {
    phis: Vec<Phi>,
    instrs: Vec<Instr>,
    term: Option<Terminator>,
    handler: Option<BlockId>,
    sealed: bool,
}

/// Where a value came into being, at emission time: parameters are defined
/// at the entry by fiat, phis and instruction results carry their block and
/// the emission counter at their creation. A phi is `incomplete` when a read
/// in an unsealed block created it; the others are the transient memos of
/// `read_variable_recursive`, which never outlive the read.
#[derive(Clone, Copy)]
enum DefSite {
    Nowhere,
    Param,
    Instr {
        block: BlockId,
        at: u32,
    },
    Phi {
        block: BlockId,
        at: u32,
        incomplete: bool,
    },
}

/// Builds one [`Function`] in SSA form. Parameters are the first values, in
/// `Params` order, and are the initial definitions of their variables in the
/// entry block.
pub struct FunctionBuilder {
    qualname: Sym,
    params: Params,
    locals: Vec<LocalMeta>,
    flags: FnFlags,
    entry: BlockId,
    blocks: Vec<Blk>,
    types: Vec<Type>,
    def_site: Vec<DefSite>,
    lines: Vec<(InstrId, u32, u32)>,
    current: Option<BlockId>,
    next_instr: u32,
    /// Emission id of each instruction, per block, parallel to `instrs`.
    emission: Vec<Vec<u32>>,
    /// Reverse edges, maintained as terminators and handlers are set: the
    /// predecessors of each block, deduplicated, in block order.
    preds: Vec<Vec<BlockId>>,
    defs: Vec<HashMap<Sym, Value>>,
    incomplete: Vec<Vec<(Sym, Value)>>,
    phi_vars: HashMap<Value, Sym>,
}

impl FunctionBuilder {
    pub fn new(qualname: Sym, params: Params, param_types: Vec<Type>) -> Result<Self, Error> {
        if param_types.len() != params.arity() {
            return Err(Error::ParamCount {
                expected: params.arity(),
                given: param_types.len(),
            });
        }
        let entry = BlockId(0);
        let mut defs = vec![HashMap::new()];
        for (i, name) in params.names().iter().enumerate() {
            defs[0].insert(*name, Value(i as u32));
        }
        let def_site = vec![DefSite::Param; param_types.len()];
        Ok(Self {
            qualname,
            params,
            locals: Vec::new(),
            flags: FnFlags(0),
            entry,
            blocks: vec![Blk {
                phis: Vec::new(),
                instrs: Vec::new(),
                term: None,
                handler: None,
                sealed: false,
            }],
            types: param_types,
            def_site,
            lines: Vec::new(),
            current: Some(entry),
            next_instr: 0,
            emission: vec![Vec::new()],
            preds: vec![Vec::new()],
            defs,
            incomplete: vec![Vec::new()],
            phi_vars: HashMap::new(),
        })
    }

    pub fn entry(&self) -> BlockId {
        self.entry
    }

    pub fn current_block(&self) -> Option<BlockId> {
        self.current
    }

    pub fn block_count(&self) -> u32 {
        self.blocks.len() as u32
    }

    pub fn set_flags(&mut self, flags: FnFlags) {
        self.flags = flags;
    }

    pub fn add_local(&mut self, local: LocalMeta) {
        self.locals.push(local);
    }

    /// Record the source position of an instruction whose id `push` returned.
    /// Ids are emission-ordered until `finish`, which re-keys the table into
    /// block order.
    pub fn record_line(&mut self, id: InstrId, line: u32, col: u32) -> Result<(), Error> {
        if id.0 >= self.next_instr {
            return Err(Error::UnknownInstr { instr: id });
        }
        self.lines.push((id, line, col));
        Ok(())
    }

    /// Append a new unsealed block and return its id.
    pub fn new_block(&mut self) -> BlockId {
        self.blocks.push(Blk {
            phis: Vec::new(),
            instrs: Vec::new(),
            term: None,
            handler: None,
            sealed: false,
        });
        self.defs.push(HashMap::new());
        self.incomplete.push(Vec::new());
        self.emission.push(Vec::new());
        self.preds.push(Vec::new());
        BlockId(self.blocks.len() as u32 - 1)
    }

    pub fn switch_to(&mut self, block: BlockId) -> Result<(), Error> {
        self.check_block(block)?;
        self.current = Some(block);
        Ok(())
    }

    /// Allocate a fresh value of `ty` for an instruction result.
    pub fn fresh(&mut self, ty: Type) -> Value {
        self.types.push(ty);
        self.def_site.push(DefSite::Nowhere);
        Value(self.types.len() as u32 - 1)
    }

    /// Append an instruction to the current block. The returned id is
    /// emission-ordered; `finish` re-keys what `record_line` stored into
    /// block order, the id space of `Function`.
    pub fn push(&mut self, instr: Instr) -> Result<InstrId, Error> {
        let current = self.current.ok_or(Error::NoCurrentBlock)?;
        let results = instr_results(&instr);
        self.blocks[current.0 as usize].instrs.push(instr);
        let id = InstrId(self.next_instr);
        self.next_instr += 1;
        self.emission[current.0 as usize].push(id.0);
        for r in results {
            if let Some(site) = self.def_site.get_mut(r.0 as usize) {
                *site = DefSite::Instr {
                    block: current,
                    at: id.0,
                };
            }
        }
        Ok(id)
    }

    /// Set the terminator of `block`. Overwriting is allowed while the block
    /// is unsealed; edges into sealed blocks must not change.
    pub fn set_term(&mut self, block: BlockId, term: Terminator) -> Result<(), Error> {
        self.check_block(block)?;
        let mut old_targets = self.blocks[block.0 as usize]
            .term
            .as_ref()
            .map(term_targets)
            .unwrap_or_default();
        if let Some(h) = self.blocks[block.0 as usize].handler {
            old_targets.push(h);
        }
        let mut new_targets = term_targets(&term);
        if let Some(h) = self.blocks[block.0 as usize].handler {
            new_targets.push(h);
        }
        self.check_sealed_edges(block, &old_targets, &new_targets)?;
        let old = self.blocks[block.0 as usize].term.take();
        if let Some(old) = &old {
            for t in term_targets(old) {
                self.remove_edge(block, t);
            }
        }
        for t in term_targets(&term) {
            self.add_edge(block, t);
        }
        self.blocks[block.0 as usize].term = Some(term);
        Ok(())
    }

    /// Set the exception handler of `block`, adding the exception edge to
    /// the handler's predecessors. Overwriting is allowed while the block is
    /// unsealed; edges into sealed blocks must not change.
    pub fn set_handler(&mut self, block: BlockId, handler: Option<BlockId>) -> Result<(), Error> {
        self.check_block(block)?;
        if let Some(h) = handler {
            self.check_block(h)?;
        }
        let old_handler = self.blocks[block.0 as usize].handler;
        let mut old_targets = old_handler.map(|h| vec![h]).unwrap_or_default();
        if let Some(t) = &self.blocks[block.0 as usize].term {
            old_targets.extend(term_targets(t));
        }
        let mut new_targets = handler.map(|h| vec![h]).unwrap_or_default();
        if let Some(t) = &self.blocks[block.0 as usize].term {
            new_targets.extend(term_targets(t));
        }
        self.check_sealed_edges(block, &old_targets, &new_targets)?;
        if let Some(old) = old_handler {
            self.remove_edge(block, old);
        }
        if let Some(h) = handler {
            self.add_edge(block, h);
        }
        self.blocks[block.0 as usize].handler = handler;
        Ok(())
    }

    /// Reject edge changes into already-sealed blocks, which would make
    /// phis and memoized reads stale. Checks net presence, so an edge kept
    /// through the other slot (terminator or handler) is not a change.
    fn check_sealed_edges(
        &self,
        from: BlockId,
        old: &[BlockId],
        new: &[BlockId],
    ) -> Result<(), Error> {
        for t in new {
            if (t.0 as usize) < self.blocks.len()
                && self.blocks[t.0 as usize].sealed
                && !old.contains(t)
            {
                return Err(Error::EdgeIntoSealed { from, to: *t });
            }
        }
        for t in old {
            if (t.0 as usize) < self.blocks.len()
                && self.blocks[t.0 as usize].sealed
                && !new.contains(t)
            {
                return Err(Error::EdgeIntoSealed { from, to: *t });
            }
        }
        Ok(())
    }

    pub fn write_variable(&mut self, var: Sym, block: BlockId, value: Value) -> Result<(), Error> {
        self.check_block(block)?;
        if value.0 as usize >= self.types.len() {
            return Err(Error::UnknownValue { value });
        }
        self.defs[block.0 as usize].insert(var, value);
        Ok(())
    }

    pub fn read_variable(&mut self, var: Sym, block: BlockId) -> Result<Value, Error> {
        self.check_block(block)?;
        if let Some(&v) = self.defs[block.0 as usize].get(&var) {
            return Ok(v);
        }
        self.read_variable_recursive(var, block)
    }

    fn read_variable_recursive(&mut self, var: Sym, block: BlockId) -> Result<Value, Error> {
        if !self.blocks[block.0 as usize].sealed {
            return Ok(self.new_phi(var, block, true));
        }
        // The paper's single-predecessor shortcut, as a loop: a chain of
        // sealed blocks is followed one link at a time, without a phi and
        // without recursion, so deep chains neither build phis only to
        // remove them again nor exhaust the stack. Every block on the way
        // resolves to the answer, and revisiting one is a cycle of
        // single-predecessor blocks, on which the variable is never written.
        let mut path: Vec<BlockId> = Vec::new();
        let mut seen = vec![false; self.blocks.len()];
        seen[block.0 as usize] = true;
        let mut cur = block;
        let value = loop {
            let preds = self.predecessors(cur);
            if preds.is_empty() {
                // Sealed blocks never gain predecessors, so there is no path
                // carrying a definition. Empty phis survive only in blocks
                // that were unsealed when the read happened and seal
                // unreachable; the entry, always reachable, errors at seal.
                return Err(Error::UndefinedVariable { var, block: cur });
            }
            if preds.len() > 1 {
                path.push(cur);
                let phi = self.new_phi(var, cur, false);
                break self.add_phi_operands(var, cur, phi)?;
            }
            let p = preds[0];
            if p == cur || seen[p.0 as usize] {
                return Err(Error::UndefinedVariable { var, block: p });
            }
            path.push(cur);
            // The paper consults the def map at every level of the walk, so
            // a write in an unsealed predecessor is read back instead of
            // being clobbered by an incomplete phi.
            if let Some(&v) = self.defs[p.0 as usize].get(&var) {
                break v;
            }
            if !self.blocks[p.0 as usize].sealed {
                break self.new_phi(var, p, true);
            }
            seen[p.0 as usize] = true;
            cur = p;
        };
        for b in path {
            self.defs[b.0 as usize].insert(var, value);
        }
        Ok(value)
    }

    fn new_phi(&mut self, var: Sym, block: BlockId, incomplete: bool) -> Value {
        let val = self.fresh(Type::Bottom);
        self.def_site[val.0 as usize] = DefSite::Phi {
            block,
            at: self.next_instr,
            incomplete,
        };
        self.blocks[block.0 as usize].phis.push(Phi {
            into: val,
            incomings: Vec::new().into(),
        });
        if incomplete {
            self.incomplete[block.0 as usize].push((var, val));
        }
        self.phi_vars.insert(val, var);
        self.defs[block.0 as usize].insert(var, val);
        val
    }

    fn add_phi_operands(&mut self, var: Sym, block: BlockId, phi: Value) -> Result<Value, Error> {
        let preds = self.predecessors(block);
        let mut incomings = Vec::with_capacity(preds.len());
        for pred in preds {
            incomings.push((self.read_variable(var, pred)?, pred));
        }
        let blk = &mut self.blocks[block.0 as usize];
        let p = blk
            .phis
            .iter_mut()
            .find(|p| p.into == phi)
            .expect("phi just created");
        p.incomings = incomings.into();
        self.retype_phi_tree(phi);
        self.try_remove_trivial_phi(phi)
    }

    /// Record that every predecessor of `block` (terminator edges and
    /// exception edges) is known; fill its incomplete phis. Sealing an
    /// already-sealed block does nothing.
    pub fn seal_block(&mut self, block: BlockId) -> Result<(), Error> {
        self.check_block(block)?;
        if self.blocks[block.0 as usize].sealed {
            return Ok(());
        }
        let incomplete = std::mem::take(&mut self.incomplete[block.0 as usize]);
        self.blocks[block.0 as usize].sealed = true;
        for (var, phi) in incomplete {
            let preds = self.predecessors(block);
            if preds.is_empty() && block == self.entry {
                // The entry is always reachable and nothing can ever reach it
                // from outside, so an empty phi here means the read saw a
                // variable defined on no path.
                return Err(Error::UndefinedVariable { var, block });
            }
            let mut incomings = Vec::with_capacity(preds.len());
            for pred in preds {
                incomings.push((self.read_variable(var, pred)?, pred));
            }
            let p = self.blocks[block.0 as usize]
                .phis
                .iter_mut()
                .find(|p| p.into == phi)
                .expect("incomplete phi kept");
            p.incomings = incomings.into();
            self.retype_phi_tree(phi);
            self.try_remove_trivial_phi(phi)?;
        }
        Ok(())
    }

    /// Seal every block, in creation order.
    pub fn seal_all(&mut self) -> Result<(), Error> {
        for i in 0..self.blocks.len() as u32 {
            self.seal_block(BlockId(i))?;
        }
        Ok(())
    }

    /// Remove `phi` if all its operands are the same value or the phi
    /// itself, replacing its uses with that operand and recursing into the
    /// phis that used it. Returns the value that replaces the phi.
    fn try_remove_trivial_phi(&mut self, phi: Value) -> Result<Value, Error> {
        let (block, pos) = self.find_phi(phi);
        let mut same: Option<Value> = None;
        let mut has_self = false;
        for (op, _) in self.blocks[block.0 as usize].phis[pos].incomings.iter() {
            if *op == phi {
                has_self = true;
                continue;
            }
            if same == Some(*op) {
                continue;
            }
            if same.is_some() {
                return Ok(phi);
            }
            same = Some(*op);
        }
        let Some(same) = same else {
            // No operands at all: the block is unreachable and the phi is
            // consistent with its empty predecessor list. Only self-operands:
            // the variable is undefined on every path.
            if has_self {
                return Err(Error::UndefinedVariable {
                    var: self.phi_vars[&phi],
                    block,
                });
            }
            return Ok(phi);
        };
        // The replacement must dominate the phi's block, else every use the
        // rewrite makes of it is invalid: the entry with a back edge is the
        // visible case, where the operand's definition only runs on the
        // second pass and the read had nothing on the first. A result in the
        // phi's own block must also precede the read that created the phi.
        // Transient memos are exempt: they never outlive the read that made
        // them, and the phi waiting on that read reports the error itself.
        let var = self.phi_vars[&phi];
        if let DefSite::Phi {
            at: phi_at,
            incomplete: true,
            ..
        } = self.def_site[phi.0 as usize]
        {
            match self.def_site.get(same.0 as usize) {
                Some(DefSite::Instr { block: d, at }) => {
                    if !self.dominates(*d, block) || (*d == block && *at >= phi_at) {
                        return Err(Error::UndefinedVariable { var, block });
                    }
                }
                Some(DefSite::Phi { block: d, .. }) if !self.dominates(*d, block) => {
                    return Err(Error::UndefinedVariable { var, block });
                }
                _ => {}
            }
        }
        let users: Vec<Value> = self
            .blocks
            .iter()
            .flat_map(|b| b.phis.iter())
            .filter(|p| p.incomings.iter().any(|(v, _)| *v == phi) && p.into != phi)
            .map(|p| p.into)
            .collect();
        self.replace_all_uses(phi, same);
        self.blocks[block.0 as usize].phis.remove(pos);
        // Later reads must not return the removed phi, whichever variable
        // holds it: `y = x` stores the phi under `y` too. Only entries that
        // still hold the phi change, so a later write keeps precedence.
        self.phi_vars.remove(&phi);
        for d in &mut self.defs {
            for v in d.values_mut() {
                if *v == phi {
                    *v = same;
                }
            }
        }
        for user in users {
            if self.find_phi_opt(user).is_some() {
                // The replacement changed the user's incomings; its declared
                // type must track the new join before the triviality check.
                self.retype_phi_tree(user);
                self.try_remove_trivial_phi(user)?;
            }
        }
        Ok(same)
    }

    fn find_phi(&self, phi: Value) -> (BlockId, usize) {
        self.find_phi_opt(phi).expect("phi exists")
    }

    fn find_phi_opt(&self, phi: Value) -> Option<(BlockId, usize)> {
        for (i, b) in self.blocks.iter().enumerate() {
            if let Some(pos) = b.phis.iter().position(|p| p.into == phi) {
                return Some((BlockId(i as u32), pos));
            }
        }
        None
    }

    fn replace_all_uses(&mut self, from: Value, to: Value) {
        for b in &mut self.blocks {
            for p in &mut b.phis {
                for (v, _) in p.incomings.iter_mut() {
                    if *v == from {
                        *v = to;
                    }
                }
            }
            for instr in &mut b.instrs {
                map_operands_mut(instr, &mut |v| {
                    if *v == from {
                        *v = to;
                    }
                });
            }
            if let Some(t) = &mut b.term {
                map_term_operands_mut(t, &mut |v| {
                    if *v == from {
                        *v = to;
                    }
                });
            }
        }
    }

    /// Set a phi's type to the join of its incoming types.
    fn retype_phi(&mut self, phi: Value) -> bool {
        let Some(p) = self
            .blocks
            .iter()
            .flat_map(|b| b.phis.iter())
            .find(|p| p.into == phi)
        else {
            return false;
        };
        let mut ty = Type::Bottom;
        for (v, _) in p.incomings.iter() {
            ty = join(ty, &self.types[v.0 as usize]);
        }
        if self.types[phi.0 as usize] == ty {
            return false;
        }
        self.types[phi.0 as usize] = ty;
        true
    }

    /// Retype `phi` from its incomings and propagate to every phi that
    /// (transitively) uses it, so a phi's declared type always equals the
    /// join of its incomings even when an operand's own type moves later.
    fn retype_phi_tree(&mut self, phi: Value) {
        let mut work = vec![phi];
        while let Some(p) = work.pop() {
            if !self.retype_phi(p) {
                continue;
            }
            work.extend(
                self.blocks
                    .iter()
                    .flat_map(|b| b.phis.iter())
                    .filter(|q| q.incomings.iter().any(|(v, _)| *v == p))
                    .map(|q| q.into),
            );
        }
    }

    /// Predecessors of `block`: terminator targets and exception edges,
    /// deduplicated, in block order. Maintained by `set_term`/`set_handler`.
    fn predecessors(&self, block: BlockId) -> Vec<BlockId> {
        self.preds[block.0 as usize].clone()
    }

    fn add_edge(&mut self, from: BlockId, to: BlockId) {
        let Some(list) = self.preds.get_mut(to.0 as usize) else {
            return;
        };
        let pos = list.partition_point(|b| b.0 < from.0);
        if list.get(pos) != Some(&from) {
            list.insert(pos, from);
        }
    }

    fn remove_edge(&mut self, from: BlockId, to: BlockId) {
        let Some(list) = self.preds.get_mut(to.0 as usize) else {
            return;
        };
        if let Ok(pos) = list.binary_search_by_key(&from.0, |b| b.0) {
            list.remove(pos);
        }
    }

    /// Successors of `block`: terminator targets and exception edges,
    /// deduplicated.
    fn successors(&self, block: BlockId) -> Vec<BlockId> {
        let mut succs: Vec<BlockId> = Vec::new();
        let add = |to: BlockId, succs: &mut Vec<BlockId>| {
            if (to.0 as usize) < self.blocks.len() && !succs.contains(&to) {
                succs.push(to);
            }
        };
        let b = &self.blocks[block.0 as usize];
        match &b.term {
            Some(Terminator::Jump { to }) => add(*to, &mut succs),
            Some(Terminator::Branch {
                then_to, else_to, ..
            }) => {
                add(*then_to, &mut succs);
                add(*else_to, &mut succs);
            }
            Some(Terminator::Switch { cases, default, .. }) => {
                for (_, to) in cases.iter() {
                    add(*to, &mut succs);
                }
                add(*default, &mut succs);
            }
            None
            | Some(Terminator::Return { .. })
            | Some(Terminator::Raise { .. })
            | Some(Terminator::Reraise)
            | Some(Terminator::Unreachable) => {}
        }
        if let Some(h) = b.handler {
            add(h, &mut succs);
        }
        succs
    }

    /// Whether `d` dominates `b` in the CFG as emitted: `b` is unreachable
    /// from the entry once `d` is removed. A block nothing reaches is
    /// dominated by everything, as in the verifier.
    fn dominates(&self, d: BlockId, b: BlockId) -> bool {
        if d == b {
            return true;
        }
        let mut seen = vec![false; self.blocks.len()];
        let mut stack = vec![self.entry];
        while let Some(cur) = stack.pop() {
            if cur == d {
                continue;
            }
            let i = cur.0 as usize;
            if seen[i] {
                continue;
            }
            seen[i] = true;
            for s in self.successors(cur) {
                if !seen[s.0 as usize] {
                    stack.push(s);
                }
            }
        }
        !seen[b.0 as usize]
    }

    fn check_block(&self, block: BlockId) -> Result<(), Error> {
        if block.0 as usize >= self.blocks.len() {
            return Err(Error::UnknownBlock { block });
        }
        Ok(())
    }

    /// Finish the function. Every block must have a terminator and be
    /// sealed.
    pub fn finish(self) -> Result<Function, Error> {
        let mut blocks = Vec::with_capacity(self.blocks.len());
        for (i, b) in self.blocks.into_iter().enumerate() {
            let Some(term) = b.term else {
                return Err(Error::MissingTerminator {
                    block: BlockId(i as u32),
                });
            };
            if !b.sealed {
                return Err(Error::UnsealedBlock {
                    block: BlockId(i as u32),
                });
            }
            blocks.push(Block {
                phis: b.phis,
                instrs: b.instrs,
                term,
                handler: b.handler,
            });
        }
        // `InstrId` numbers instructions across blocks in block order, so the
        // emission-order ids `push` handed out are translated here, block by
        // block. `record_line` validated every id against the counter.
        let mut final_id = vec![0u32; self.next_instr as usize];
        let mut acc = 0u32;
        for block in &self.emission {
            for &e in block {
                final_id[e as usize] = acc;
                acc += 1;
            }
        }
        let mut lines = self.lines;
        for (id, _, _) in &mut lines {
            id.0 = final_id[id.0 as usize];
        }
        lines.sort_unstable_by_key(|&(id, _, _)| id);
        Ok(Function {
            qualname: self.qualname,
            params: self.params,
            blocks,
            entry: self.entry,
            locals: self.locals,
            flags: self.flags,
            types: self.types,
            lines,
        })
    }
}

/// The blocks a terminator targets.
fn term_targets(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Jump { to } => vec![*to],
        Terminator::Branch {
            then_to, else_to, ..
        } => vec![*then_to, *else_to],
        Terminator::Switch { cases, default, .. } => {
            let mut targets: Vec<BlockId> = cases.iter().map(|(_, to)| *to).collect();
            targets.push(*default);
            targets
        }
        Terminator::Return { .. }
        | Terminator::Raise { .. }
        | Terminator::Reraise
        | Terminator::Unreachable => Vec::new(),
    }
}

/// Rewrite every operand (not result) of `instr` with `f`.
fn map_operands_mut(instr: &mut Instr, f: &mut impl FnMut(&mut Value)) {
    use Instr as I;
    match instr {
        I::Const { .. }
        | I::LoadGlobal { .. }
        | I::LoadName { .. }
        | I::Safepoint
        | I::CatchEnd
        | I::PopExcInfo
        | I::PushExcInfo { .. }
        | I::FinallyEnter { .. }
        | I::AllocInst { .. }
        | I::StackAlloc { .. } => {}
        I::Copy { src, .. }
        | I::INegOvf { src, .. }
        | I::INeg { src, .. }
        | I::BitNot { src, .. }
        | I::FNeg { src, .. }
        | I::Box { src, .. }
        | I::Unbox { src, .. }
        | I::UnaryOp { operand: src, .. }
        | I::GetIter { obj: src, .. }
        | I::Next { iter: src, .. }
        | I::Truth { value: src, .. }
        | I::Len { value: src, .. }
        | I::GetAttr { obj: src, .. }
        | I::DelAttr { obj: src, .. }
        | I::Unpack { src, .. }
        | I::LoadCell { cell: src, .. }
        | I::TypeGuard { value: src, .. }
        | I::ShapeGuard { obj: src, .. }
        | I::TupleGetUnchecked { tuple: src, .. }
        | I::Yield { value: src, .. }
        | I::Await { value: src, .. }
        | I::FinallyExit { reason: src }
        | I::CatchBegin { types: src, .. } => f(src),
        I::IAddOvf { lhs, rhs, .. }
        | I::ISubOvf { lhs, rhs, .. }
        | I::IMulOvf { lhs, rhs, .. }
        | I::IDivOvf { lhs, rhs, .. }
        | I::ShlOvf { lhs, rhs, .. }
        | I::IAdd { lhs, rhs, .. }
        | I::ISub { lhs, rhs, .. }
        | I::IMul { lhs, rhs, .. }
        | I::IDiv { lhs, rhs, .. }
        | I::IMod { lhs, rhs, .. }
        | I::Shl { lhs, rhs, .. }
        | I::Shr { lhs, rhs, .. }
        | I::BitAnd { lhs, rhs, .. }
        | I::BitOr { lhs, rhs, .. }
        | I::BitXor { lhs, rhs, .. }
        | I::ICmp { lhs, rhs, .. }
        | I::FAdd { lhs, rhs, .. }
        | I::FSub { lhs, rhs, .. }
        | I::FMul { lhs, rhs, .. }
        | I::FDiv { lhs, rhs, .. }
        | I::FMod { lhs, rhs, .. }
        | I::FCmp { lhs, rhs, .. }
        | I::BinOp { lhs, rhs, .. }
        | I::Compare { lhs, rhs, .. }
        | I::Contains {
            haystack: lhs,
            needle: rhs,
            ..
        } => {
            f(lhs);
            f(rhs);
        }
        I::SetAttr { obj, value, .. } => {
            f(obj);
            f(value);
        }
        I::SetItem { obj, key, value } => {
            f(obj);
            f(key);
            f(value);
        }
        I::GetItem { obj, key, .. } | I::DelItem { obj, key } => {
            f(obj);
            f(key);
        }
        I::Call {
            callee,
            args,
            kwnames,
            ..
        } => {
            f(callee);
            for a in args.iter_mut() {
                f(a);
            }
            for (_, v) in kwnames.iter_mut() {
                f(v);
            }
        }
        I::CallMethod {
            recv,
            args,
            kwnames,
            ..
        } => {
            f(recv);
            for a in args.iter_mut() {
                f(a);
            }
            for (_, v) in kwnames.iter_mut() {
                f(v);
            }
        }
        I::CallEx {
            callee, args, kwargs, ..
        } => {
            f(callee);
            f(args);
            f(kwargs);
        }
        I::Format { value, spec, .. } => {
            f(value);
            f(spec);
        }
        I::BuildList { items, .. }
        | I::BuildTuple { items, .. }
        | I::BuildSet { items, .. }
        | I::BuildStr { parts: items, .. }
        | I::AllocTuple { items, .. } => {
            for v in items.iter_mut() {
                f(v);
            }
        }
        I::BuildDict { items, .. } => {
            for (k, v) in items.iter_mut() {
                f(k);
                f(v);
            }
        }
        I::BuildSlice {
            start, stop, step, ..
        } => {
            if let Some(v) = start {
                f(v);
            }
            f(stop);
            if let Some(v) = step {
                f(v);
            }
        }
        I::Import {
            fromlist, level, ..
        } => {
            f(fromlist);
            f(level);
        }
        I::ImportStar { module } => f(module),
        I::MakeFunction {
            cells, defaults, ..
        } => {
            for v in cells.iter_mut() {
                f(v);
            }
            for v in defaults.iter_mut() {
                f(v);
            }
        }
        I::MakeClass {
            bases,
            cells,
            kwds,
            ..
        } => {
            for v in bases.iter_mut() {
                f(v);
            }
            for v in cells.iter_mut() {
                f(v);
            }
            f(kwds);
        }
        I::MakeCell { initial, .. } => {
            if let Some(v) = initial {
                f(v);
            }
        }
        I::StoreCell { cell, value } => {
            f(cell);
            f(value);
        }
        I::StoreGlobal { value, .. } | I::StoreName { value, .. } => f(value),
        I::LoadField { obj, .. } => f(obj),
        I::StoreField { obj, value, .. } => {
            f(obj);
            f(value);
        }
        I::CallDirect { args, .. } => {
            for v in args.iter_mut() {
                f(v);
            }
        }
        I::BoundsCheck { index, len, .. } => {
            f(index);
            f(len);
        }
        I::ListGetUnchecked { list, index, .. } => {
            f(list);
            f(index);
        }
        I::YieldFrom { iter, send, .. } => {
            f(iter);
            f(send);
        }
    }
}

fn map_term_operands_mut(t: &mut Terminator, f: &mut impl FnMut(&mut Value)) {
    match t {
        Terminator::Jump { .. } | Terminator::Reraise | Terminator::Unreachable => {}
        Terminator::Branch { cond, .. } => f(cond),
        Terminator::Switch { on, .. } => f(on),
        Terminator::Return { value } => f(value),
        Terminator::Raise { exc, cause } => {
            if let Some(v) = exc {
                f(v);
            }
            if let Some(v) = cause {
                f(v);
            }
        }
    }
}

/// The type a phi can hold for incomings of both types: equal types stay,
/// `Bottom` is the identity, `Top` absorbs, and an unboxed `Int64` widened by
/// its boxed form `BigInt` becomes `BigInt`; anything else is dynamic.
fn join(a: Type, b: &Type) -> Type {
    use Type as T;
    if a == *b || matches!(a, T::Bottom) {
        return b.clone();
    }
    if matches!(b, T::Bottom) {
        return a;
    }
    if matches!(a, T::Top) || matches!(b, T::Top) {
        return T::Top;
    }
    match (&a, b) {
        (T::Int64, T::BigInt) | (T::BigInt, T::Int64) => T::BigInt,
        _ => T::Top,
    }
}

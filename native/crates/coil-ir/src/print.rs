//! Stable human-readable text form of the IR, one instruction per line; used
//! by `--emit ir` dumps and regression tests.

use crate::ir::Module;
use crate::ir::{
    BarrierKind, BinOp, ClassDesc, CmpOp, Const, ConstPool, FCmpOp, FnFlags, Function, ICmpOp,
    Instr, LocalKind, Scalar, Terminator, UnaryOp,
};
use crate::types::{BlockId, InstrId, Value};

/// Print a whole module.
pub fn print_module(m: &Module) -> String {
    let pool = &m.consts;
    let mut out = String::new();
    out.push_str(&format!(
        "coil-ir module {} file {}\n",
        quote(pool.sym_str(m.name)),
        quote(&m.file.to_string_lossy())
    ));
    if !pool.strings.is_empty() {
        out.push_str("syms:\n");
        for (i, s) in pool.strings.iter().enumerate() {
            out.push_str(&format!("  {i}: {}\n", quote(s)));
        }
    }
    if !pool.consts.is_empty() {
        out.push_str("consts:\n");
        for (i, c) in pool.consts.iter().enumerate() {
            out.push_str(&format!("  {i}: {}\n", const_text(pool, c)));
        }
    }
    if !m.classes.is_empty() {
        out.push_str("classes:\n");
        for (i, c) in m.classes.iter().enumerate() {
            out.push_str(&format!("  class {i} {}\n", class_text(pool, c)));
        }
    }
    if !m.functions.is_empty() {
        out.push_str("functions:\n");
        for (i, f) in m.functions.iter().enumerate() {
            out.push_str(&print_function(pool, i as u32, f));
        }
    }
    out.push_str(&format!("body fn {}\n", m.body.0));
    out
}

/// Print a whole module with each value annotated by its analysis facts
/// (`Function::facts`) instead of its representation type: the inference's
/// view, where `Int64` means "an int that fits in 64 bits". A function with
/// no facts (inference has not run) prints its representation types.
pub fn print_module_facts(m: &Module) -> String {
    let mut swapped = m.clone();
    for f in &mut swapped.functions {
        if !f.facts.is_empty() {
            std::mem::swap(&mut f.types, &mut f.facts);
        }
    }
    print_module(&swapped)
}

/// Print one function against the module's constant pool.
pub fn print_function(pool: &ConstPool, index: u32, f: &Function) -> String {
    let mut out = String::new();
    out.push_str(&format!("fn {} {}", index, quote(pool.sym_str(f.qualname))));
    let names = f.params.names();
    if names.is_empty() {
        out.push_str(" params ()");
    } else {
        out.push_str(" params (");
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let v = Value(i as u32);
            out.push_str(&format!("{}: {}:{}", pool.sym_str(*n), val(v), ty(f, v)));
        }
        out.push(')');
    }
    let flags = flags_text(f.flags);
    if !flags.is_empty() {
        out.push_str(&format!(" flags [{flags}]"));
    }
    if !f.locals.is_empty() {
        out.push_str(" locals [");
        for (i, l) in f.locals.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let kind = match l.kind {
                LocalKind::Local => "local",
                LocalKind::Cell => "cell",
                LocalKind::Free => "free",
            };
            out.push_str(&format!("{}: {kind}", pool.sym_str(l.name)));
        }
        out.push(']');
    }
    out.push('\n');
    // Instructions are numbered across blocks in order, phis excluded, so the
    // line table can be walked with a running counter.
    let mut next_id = 0u32;
    for (i, b) in f.blocks.iter().enumerate() {
        next_id = print_block(pool, f, BlockId(i as u32), b, next_id, &mut out);
    }
    out
}

fn print_block(
    pool: &ConstPool,
    f: &Function,
    id: BlockId,
    b: &crate::ir::Block,
    next_id: u32,
    out: &mut String,
) -> u32 {
    out.push_str(&format!("  block {}", id.0));
    if id == f.entry {
        out.push_str(" (entry)");
    }
    if let Some(h) = b.handler {
        out.push_str(&format!(" handler -> block {}", h.0));
    }
    out.push('\n');
    for p in &b.phis {
        out.push_str(&format!("    {}:{} = phi [", val(p.into), ty(f, p.into)));
        for (i, (v, from)) in p.incomings.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{} from block {}", val(*v), from.0));
        }
        out.push_str("]\n");
    }
    let mut id = next_id;
    for ins in &b.instrs {
        print_instr(pool, f, InstrId(id), ins, out);
        id += 1;
    }
    print_term(&b.term, out);
    id
}

fn print_instr(pool: &ConstPool, f: &Function, id: InstrId, ins: &Instr, out: &mut String) {
    let results = instr_results(ins);
    out.push_str("    ");
    if !results.is_empty() {
        for (i, r) in results.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("{}:{}", val(*r), ty(f, *r)));
        }
        out.push_str(" = ");
    }
    out.push_str(&op_text(pool, ins));
    if let Some((l, c)) = f.line_of(id) {
        out.push_str(&format!(" @{l}:{c}"));
    }
    out.push('\n');
}

fn print_term(t: &Terminator, out: &mut String) {
    out.push_str("    -> ");
    match t {
        Terminator::Jump { to } => out.push_str(&format!("jump block {}", to.0)),
        Terminator::Branch {
            cond,
            then_to,
            else_to,
        } => out.push_str(&format!(
            "branch {}, block {}, block {}",
            val(*cond),
            then_to.0,
            else_to.0
        )),
        Terminator::Switch { on, cases, default } => {
            out.push_str(&format!("switch {}, [", val(*on)));
            for (i, (v, b)) in cases.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("{v}: block {}", b.0));
            }
            out.push_str(&format!("], else block {}", default.0));
        }
        Terminator::Return { value } => out.push_str(&format!("return {}", val(*value))),
        Terminator::Raise { exc, cause } => match (exc, cause) {
            (Some(e), Some(c)) => out.push_str(&format!("raise {}, {}", val(*e), val(*c))),
            (Some(e), None) => out.push_str(&format!("raise {}", val(*e))),
            (None, _) => out.push_str("raise"),
        },
        Terminator::Reraise => out.push_str("reraise"),
        Terminator::Unreachable => out.push_str("unreachable"),
    }
    out.push('\n');
}

/// The values an instruction defines, in definition order.
pub fn instr_results(ins: &Instr) -> Vec<Value> {
    use Instr as I;
    match ins {
        I::Const { into, .. }
        | I::Copy { into, .. }
        | I::IAdd { into, .. }
        | I::ISub { into, .. }
        | I::IMul { into, .. }
        | I::IDiv { into, .. }
        | I::IMod { into, .. }
        | I::INeg { into, .. }
        | I::Shl { into, .. }
        | I::Shr { into, .. }
        | I::BitAnd { into, .. }
        | I::BitOr { into, .. }
        | I::BitXor { into, .. }
        | I::BitNot { into, .. }
        | I::ICmp { into, .. }
        | I::FAdd { into, .. }
        | I::FSub { into, .. }
        | I::FMul { into, .. }
        | I::FDiv { into, .. }
        | I::FMod { into, .. }
        | I::FNeg { into, .. }
        | I::FCmp { into, .. }
        | I::Box { into, .. }
        | I::Unbox { into, .. }
        | I::BinOp { into, .. }
        | I::UnaryOp { into, .. }
        | I::Compare { into, .. }
        | I::GetAttr { into, .. }
        | I::GetItem { into, .. }
        | I::GetIter { into, .. }
        | I::Next { into, .. }
        | I::Call { into, .. }
        | I::CallEx { into, .. }
        | I::CallMethod { into, .. }
        | I::Truth { into, .. }
        | I::Len { into, .. }
        | I::Contains { into, .. }
        | I::Format { into, .. }
        | I::BuildList { into, .. }
        | I::BuildTuple { into, .. }
        | I::BuildDict { into, .. }
        | I::BuildSet { into, .. }
        | I::BuildStr { into, .. }
        | I::BuildSlice { into, .. }
        | I::Import { into, .. }
        | I::MakeFunction { into, .. }
        | I::MakeClass { into, .. }
        | I::MakeCell { into, .. }
        | I::LoadCell { into, .. }
        | I::LoadClosure { into, .. }
        | I::LoadGlobal { into, .. }
        | I::LoadName { into, .. }
        | I::CallDirect { into, .. }
        | I::LoadField { into, .. }
        | I::BoundsCheck { into, .. }
        | I::ListGetUnchecked { into, .. }
        | I::TupleGetUnchecked { into, .. }
        | I::AllocInst { into, .. }
        | I::AllocTuple { into, .. }
        | I::StackAlloc { into, .. }
        | I::Yield { into, .. }
        | I::YieldFrom { into, .. }
        | I::Await { into, .. }
        | I::PushExcInfo { into, .. }
        | I::FinallyEnter { into, .. }
        | I::PrepReraiseStar { into, .. } => vec![*into],
        I::IAddOvf { into, ovf, .. }
        | I::ISubOvf { into, ovf, .. }
        | I::IMulOvf { into, ovf, .. }
        | I::IDivOvf { into, ovf, .. }
        | I::INegOvf { into, ovf, .. }
        | I::ShlOvf { into, ovf, .. } => vec![*into, *ovf],
        I::TypeGuard { ok, fail, .. } | I::ShapeGuard { ok, fail, .. } => vec![*ok, *fail],
        I::CatchBegin { matched, exc, .. } => vec![*matched, *exc],
        I::CatchStarBegin {
            matched, exc, rest, ..
        } => vec![*matched, *exc, *rest],
        I::Unpack { into, .. } => into.to_vec(),
        I::SetAttr { .. }
        | I::DelAttr { .. }
        | I::SetItem { .. }
        | I::DelItem { .. }
        | I::StoreCell { .. }
        | I::StoreGlobal { .. }
        | I::StoreName { .. }
        | I::StoreField { .. }
        | I::ImportStar { .. }
        | I::Safepoint
        | I::CatchEnd
        | I::PopExcInfo
        | I::FinallyExit { .. } => vec![],
    }
}

fn op_text(pool: &ConstPool, ins: &Instr) -> String {
    use Instr as I;
    let vals = |vs: &[Value]| vs.iter().map(|v| val(*v)).collect::<Vec<_>>().join(", ");
    let kwargs = |s: &mut String, kwnames: &[(crate::types::Sym, Value)]| {
        if !kwnames.is_empty() {
            s.push_str(", {");
            for (i, (n, v)) in kwnames.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format!("{}: {}", quote(pool.sym_str(*n)), val(*v)));
            }
            s.push('}');
        }
    };
    let mut s = String::new();
    match ins {
        I::Const { idx, .. } => s.push_str(&format!(
            "const #{} {}",
            idx.0,
            const_text(pool, &pool.consts[idx.0 as usize])
        )),
        I::Copy { src, .. } => s.push_str(&format!("copy {}", val(*src))),
        I::IAddOvf { lhs, rhs, .. } => {
            s.push_str(&format!("iadd_ovf {}, {}", val(*lhs), val(*rhs)))
        }
        I::ISubOvf { lhs, rhs, .. } => {
            s.push_str(&format!("isub_ovf {}, {}", val(*lhs), val(*rhs)))
        }
        I::IMulOvf { lhs, rhs, .. } => {
            s.push_str(&format!("imul_ovf {}, {}", val(*lhs), val(*rhs)))
        }
        I::IDivOvf { lhs, rhs, .. } => {
            s.push_str(&format!("idiv_ovf {}, {}", val(*lhs), val(*rhs)))
        }
        I::INegOvf { src, .. } => s.push_str(&format!("ineg_ovf {}", val(*src))),
        I::ShlOvf { lhs, rhs, .. } => s.push_str(&format!("shl_ovf {}, {}", val(*lhs), val(*rhs))),
        I::IAdd { lhs, rhs, .. } => s.push_str(&format!("iadd {}, {}", val(*lhs), val(*rhs))),
        I::ISub { lhs, rhs, .. } => s.push_str(&format!("isub {}, {}", val(*lhs), val(*rhs))),
        I::IMul { lhs, rhs, .. } => s.push_str(&format!("imul {}, {}", val(*lhs), val(*rhs))),
        I::IDiv { lhs, rhs, .. } => s.push_str(&format!("idiv {}, {}", val(*lhs), val(*rhs))),
        I::IMod { lhs, rhs, .. } => s.push_str(&format!("imod {}, {}", val(*lhs), val(*rhs))),
        I::INeg { src, .. } => s.push_str(&format!("ineg {}", val(*src))),
        I::Shl { lhs, rhs, .. } => s.push_str(&format!("shl {}, {}", val(*lhs), val(*rhs))),
        I::Shr { lhs, rhs, .. } => s.push_str(&format!("shr {}, {}", val(*lhs), val(*rhs))),
        I::BitAnd { lhs, rhs, .. } => s.push_str(&format!("bitand {}, {}", val(*lhs), val(*rhs))),
        I::BitOr { lhs, rhs, .. } => s.push_str(&format!("bitor {}, {}", val(*lhs), val(*rhs))),
        I::BitXor { lhs, rhs, .. } => s.push_str(&format!("bitxor {}, {}", val(*lhs), val(*rhs))),
        I::BitNot { src, .. } => s.push_str(&format!("bitnot {}", val(*src))),
        I::ICmp { op, lhs, rhs, .. } => {
            s.push_str(&format!("icmp {}, {}, {}", icmp(op), val(*lhs), val(*rhs)))
        }
        I::FAdd { lhs, rhs, .. } => s.push_str(&format!("fadd {}, {}", val(*lhs), val(*rhs))),
        I::FSub { lhs, rhs, .. } => s.push_str(&format!("fsub {}, {}", val(*lhs), val(*rhs))),
        I::FMul { lhs, rhs, .. } => s.push_str(&format!("fmul {}, {}", val(*lhs), val(*rhs))),
        I::FDiv { lhs, rhs, .. } => s.push_str(&format!("fdiv {}, {}", val(*lhs), val(*rhs))),
        I::FMod { lhs, rhs, .. } => s.push_str(&format!("fmod {}, {}", val(*lhs), val(*rhs))),
        I::FNeg { src, .. } => s.push_str(&format!("fneg {}", val(*src))),
        I::FCmp { op, lhs, rhs, .. } => {
            s.push_str(&format!("fcmp {}, {}, {}", fcmp(op), val(*lhs), val(*rhs)))
        }
        I::Box { src, scalar, .. } => {
            s.push_str(&format!("box {}, {}", scalar_name(*scalar), val(*src)))
        }
        I::Unbox { src, scalar, .. } => {
            s.push_str(&format!("unbox {}, {}", scalar_name(*scalar), val(*src)))
        }
        I::BinOp { op, lhs, rhs, .. } => s.push_str(&format!(
            "binop {}, {}, {}",
            binop(op),
            val(*lhs),
            val(*rhs)
        )),
        I::UnaryOp { op, operand, .. } => {
            s.push_str(&format!("unaryop {}, {}", unaryop(op), val(*operand)))
        }
        I::Compare { op, lhs, rhs, .. } => s.push_str(&format!(
            "compare {}, {}, {}",
            cmpop(op),
            val(*lhs),
            val(*rhs)
        )),
        I::GetAttr { obj, name, .. } => s.push_str(&format!(
            "getattr {}, {}",
            val(*obj),
            quote(pool.sym_str(*name))
        )),
        I::SetAttr { obj, name, value } => s.push_str(&format!(
            "setattr {}, {}, {}",
            val(*obj),
            quote(pool.sym_str(*name)),
            val(*value)
        )),
        I::DelAttr { obj, name } => s.push_str(&format!(
            "delattr {}, {}",
            val(*obj),
            quote(pool.sym_str(*name))
        )),
        I::GetItem { obj, key, .. } => s.push_str(&format!("getitem {}, {}", val(*obj), val(*key))),
        I::SetItem { obj, key, value } => s.push_str(&format!(
            "setitem {}, {}, {}",
            val(*obj),
            val(*key),
            val(*value)
        )),
        I::DelItem { obj, key } => s.push_str(&format!("delitem {}, {}", val(*obj), val(*key))),
        I::GetIter { obj, .. } => s.push_str(&format!("getiter {}", val(*obj))),
        I::Next { iter, .. } => s.push_str(&format!("next {}", val(*iter))),
        I::Call {
            callee,
            args,
            kwnames,
            ..
        } => {
            s.push_str(&format!("call {}, ({})", val(*callee), vals(args)));
            kwargs(&mut s, kwnames);
        }
        I::CallMethod {
            name,
            recv,
            args,
            kwnames,
            ..
        } => {
            s.push_str(&format!(
                "callmethod {}, {}, ({})",
                quote(pool.sym_str(*name)),
                val(*recv),
                vals(args)
            ));
            kwargs(&mut s, kwnames);
        }
        I::CallEx {
            callee,
            args,
            kwargs: kw,
            ..
        } => s.push_str(&format!(
            "callex {}, {}, kwargs {}",
            val(*callee),
            val(*args),
            val(*kw)
        )),
        I::Truth { value, .. } => s.push_str(&format!("truth {}", val(*value))),
        I::Len { value, .. } => s.push_str(&format!("len {}", val(*value))),
        I::Contains {
            haystack, needle, ..
        } => s.push_str(&format!("contains {}, {}", val(*haystack), val(*needle))),
        I::Format { value, spec, .. } => {
            s.push_str(&format!("format {}, {}", val(*value), val(*spec)))
        }
        I::BuildList { items, .. } => s.push_str(&format!("buildlist [{}]", vals(items))),
        I::BuildTuple { items, .. } => s.push_str(&format!("buildtuple [{}]", vals(items))),
        I::BuildDict { items, .. } => {
            s.push_str("builddict [");
            for (i, (k, v)) in items.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&format!("{}: {}", val(*k), val(*v)));
            }
            s.push(']');
        }
        I::BuildSet { items, .. } => s.push_str(&format!("buildset [{}]", vals(items))),
        I::BuildStr { parts, .. } => s.push_str(&format!("buildstr [{}]", vals(parts))),
        I::BuildSlice {
            start, stop, step, ..
        } => s.push_str(&format!(
            "buildslice {}:{}:{}",
            opt(*start),
            val(*stop),
            opt(*step)
        )),
        I::Unpack { src, n, star, .. } => {
            s.push_str(&format!("unpack {}, n {}", val(*src), n));
            if let Some(st) = star {
                s.push_str(&format!(", star {st}"));
            }
        }
        I::Import {
            name,
            fromlist,
            level,
            ..
        } => s.push_str(&format!(
            "import {}, {}, {}",
            quote(pool.sym_str(*name)),
            val(*fromlist),
            val(*level)
        )),
        I::ImportStar { module } => s.push_str(&format!("importstar {}", val(*module))),
        I::MakeFunction {
            func,
            cells,
            defaults,
            ..
        } => s.push_str(&format!(
            "makefunction fn {}, cells ({}), defaults ({})",
            func.0,
            vals(cells),
            vals(defaults)
        )),
        I::MakeClass {
            desc,
            bases,
            cells,
            kwds,
            ..
        } => s.push_str(&format!(
            "makeclass class {}, bases [{}], cells ({}), kwds {}",
            desc.0,
            vals(bases),
            vals(cells),
            val(*kwds)
        )),
        I::MakeCell { initial, .. } => s.push_str(&format!("makecell {}", opt(*initial))),
        I::LoadClosure { index, .. } => s.push_str(&format!("loadclosure {index}")),
        I::LoadCell { cell, .. } => s.push_str(&format!("loadcell {}", val(*cell))),
        I::StoreCell { cell, value } => {
            s.push_str(&format!("storecell {}, {}", val(*cell), val(*value)))
        }
        I::LoadGlobal { name, .. } => {
            s.push_str(&format!("loadglobal {}", quote(pool.sym_str(*name))))
        }
        I::StoreGlobal { name, value } => s.push_str(&format!(
            "storeglobal {}, {}",
            quote(pool.sym_str(*name)),
            val(*value)
        )),
        I::LoadName { name, .. } => s.push_str(&format!("loadname {}", quote(pool.sym_str(*name)))),
        I::StoreName { name, value } => s.push_str(&format!(
            "storename {}, {}",
            quote(pool.sym_str(*name)),
            val(*value)
        )),
        I::CallDirect { func, args, .. } => {
            s.push_str(&format!("calldirect fn {}, ({})", func.0, vals(args)))
        }
        I::LoadField {
            obj, shape, slot, ..
        } => s.push_str(&format!(
            "loadfield {}, shape {}, slot {}",
            val(*obj),
            shape.0,
            slot
        )),
        I::StoreField {
            obj,
            shape,
            slot,
            value,
            barrier,
        } => s.push_str(&format!(
            "storefield {}, shape {}, slot {}, {}, {}",
            val(*obj),
            shape.0,
            slot,
            val(*value),
            barrier_name(*barrier)
        )),
        I::AllocInst { cls, shape, .. } => {
            s.push_str(&format!("allocinst class {}, shape {}", cls.0, shape.0))
        }
        I::AllocTuple { items, .. } => s.push_str(&format!("alloctuple [{}]", vals(items))),
        I::TypeGuard { value, ty, .. } => s.push_str(&format!("typeguard {}, {}", val(*value), ty)),
        I::ShapeGuard { obj, shape, .. } => {
            s.push_str(&format!("shapeguard {}, shape {}", val(*obj), shape.0))
        }
        I::BoundsCheck { index, len, .. } => {
            s.push_str(&format!("boundscheck {}, {}", val(*index), val(*len)))
        }
        I::ListGetUnchecked { list, index, .. } => s.push_str(&format!(
            "listget_unchecked {}, {}",
            val(*list),
            val(*index)
        )),
        I::TupleGetUnchecked { tuple, index, .. } => {
            s.push_str(&format!("tupleget_unchecked {}, {}", val(*tuple), index))
        }
        I::StackAlloc { cls, shape, .. } => {
            s.push_str(&format!("stackalloc class {}, shape {}", cls.0, shape.0))
        }
        I::Yield { state, value, .. } => {
            s.push_str(&format!("yield state {}, {}", state, val(*value)))
        }
        I::YieldFrom {
            state, iter, send, ..
        } => s.push_str(&format!(
            "yieldfrom state {}, {}, {}",
            state,
            val(*iter),
            val(*send)
        )),
        I::Await { state, value, .. } => {
            s.push_str(&format!("await state {}, {}", state, val(*value)))
        }
        I::Safepoint => s.push_str("safepoint"),
        I::CatchBegin { types, .. } => s.push_str(&format!("catchbegin {}", val(*types))),
        I::CatchStarBegin { src, types, .. } => {
            s.push_str(&format!("catchstarbegin {}, {}", val(*src), val(*types)))
        }
        I::PrepReraiseStar { orig, excs, .. } => {
            s.push_str(&format!("prepreraisestar {}, {}", val(*orig), val(*excs)))
        }
        I::CatchEnd => s.push_str("catchend"),
        I::PushExcInfo { .. } => s.push_str("pushexcinfo"),
        I::PopExcInfo => s.push_str("popexcinfo"),
        I::FinallyEnter { .. } => s.push_str("finallyenter"),
        I::FinallyExit { reason } => s.push_str(&format!("finallyexit {}", val(*reason))),
    }
    s
}

fn val(v: Value) -> String {
    format!("v{}", v.0)
}

fn opt(v: Option<Value>) -> String {
    match v {
        Some(v) => val(v),
        None => "-".to_string(),
    }
}

fn ty(f: &Function, v: Value) -> String {
    match f.types.get(v.idx()) {
        Some(t) => t.to_string(),
        None => "?".to_string(),
    }
}

fn binop(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "truediv",
        BinOp::FloorDiv => "floordiv",
        BinOp::Mod => "mod",
        BinOp::Pow => "pow",
        BinOp::LShift => "lshift",
        BinOp::RShift => "rshift",
        BinOp::BitAnd => "and",
        BinOp::BitOr => "or",
        BinOp::BitXor => "xor",
        BinOp::MatMul => "matmul",
    }
}

fn unaryop(op: &UnaryOp) -> &'static str {
    match op {
        UnaryOp::Pos => "pos",
        UnaryOp::Neg => "neg",
        UnaryOp::Invert => "invert",
        UnaryOp::Not => "not",
    }
}

fn cmpop(op: &CmpOp) -> &'static str {
    match op {
        CmpOp::Eq => "eq",
        CmpOp::Ne => "ne",
        CmpOp::Lt => "lt",
        CmpOp::Le => "le",
        CmpOp::Gt => "gt",
        CmpOp::Ge => "ge",
        CmpOp::Is => "is",
        CmpOp::IsNot => "isnot",
        CmpOp::In => "in",
        CmpOp::NotIn => "notin",
    }
}

fn icmp(op: &ICmpOp) -> &'static str {
    match op {
        ICmpOp::Eq => "eq",
        ICmpOp::Ne => "ne",
        ICmpOp::Lt => "lt",
        ICmpOp::Le => "le",
        ICmpOp::Gt => "gt",
        ICmpOp::Ge => "ge",
    }
}

fn fcmp(op: &FCmpOp) -> &'static str {
    match op {
        FCmpOp::Eq => "eq",
        FCmpOp::Ne => "ne",
        FCmpOp::Lt => "lt",
        FCmpOp::Le => "le",
        FCmpOp::Gt => "gt",
        FCmpOp::Ge => "ge",
    }
}

fn scalar_name(s: Scalar) -> &'static str {
    match s {
        Scalar::Int64 => "int64",
        Scalar::Float64 => "float64",
        Scalar::Bool => "bool",
    }
}

fn barrier_name(b: BarrierKind) -> &'static str {
    match b {
        BarrierKind::None => "none",
        BarrierKind::Ext => "ext",
        BarrierKind::Dynamic => "dynamic",
    }
}

fn flags_text(flags: FnFlags) -> String {
    let mut parts = Vec::new();
    if flags.contains(FnFlags::GENERATOR) {
        parts.push("generator");
    }
    if flags.contains(FnFlags::COROUTINE) {
        parts.push("coroutine");
    }
    if flags.contains(FnFlags::ASYNC_GEN) {
        parts.push("async-gen");
    }
    if flags.contains(FnFlags::HAS_CELLS) {
        parts.push("has-cells");
    }
    parts.join(", ")
}

fn class_text(pool: &ConstPool, c: &ClassDesc) -> String {
    let bases = c
        .bases
        .iter()
        .map(|b| b.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let methods = c
        .methods
        .iter()
        .map(|(n, f)| format!("{}: fn {}", quote(pool.sym_str(*n)), f.0))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} bases [{}] body fn {} methods [{}]{}",
        quote(pool.sym_str(c.name)),
        bases,
        c.body.0,
        methods,
        if c.has_ext_base { " ext-base" } else { "" }
    )
}

fn const_text(pool: &ConstPool, c: &Const) -> String {
    match c {
        Const::None => "None".to_string(),
        Const::Bool(b) => format!("Bool({b})"),
        Const::Int(i) => format!("Int({i})"),
        Const::BigInt(s) => format!("BigInt({s})"),
        Const::Float(x) => format!("Float({x:?})"),
        Const::Str(s) => format!("Str({})", quote(pool.sym_str(*s))),
        Const::Bytes(b) => format!(
            "Bytes[{}]",
            b.iter().map(|x| format!("{x:02x}")).collect::<String>()
        ),
        Const::Tuple(items) => format!(
            "Tuple[{}]",
            items
                .iter()
                .map(|i| format!("#{}", i.0))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Const::Ellipsis => "Ellipsis".to_string(),
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        match ch {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

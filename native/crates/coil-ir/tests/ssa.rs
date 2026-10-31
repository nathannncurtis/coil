//! SSA construction (Braun et al., CC 2013) and the verifier: the paper's
//! loop examples, an irreducible-ish CFG, trivial-phi removal, exception
//! edges as handler predecessors, and one negative test per verifier rule.

use coil_ir::builder::Error as BuildError;
use coil_ir::builder::FunctionBuilder;
use coil_ir::ir::{
    BarrierKind, BinOp, Block, ClassDesc, FnFlags, Function, ICmpOp, Instr, Params, Phi, Terminator,
};
use coil_ir::types::{
    BlockId, ClassId, ConstIdx, FnId, InstrId, ShapeId, Sym, Type, TypeRef, Value,
};
use coil_ir::verify::{self, Error as VerifyError};

const X: Sym = Sym(1);
const U: Sym = Sym(2);

fn v(n: u32) -> Value {
    Value(n)
}

fn one_param() -> Params {
    Params {
        positional: vec![Sym(0)],
        vararg: None,
        kwonly: vec![],
        kwarg: None,
    }
}

/// `into = truth(p)` in the current block, returning the Bool result.
fn cond(b: &mut FunctionBuilder, p: Value) -> Value {
    let c = b.fresh(Type::Bool);
    b.push(Instr::Truth { value: p, into: c }).unwrap();
    c
}

/// `into = const #k` in the current block.
fn konst(b: &mut FunctionBuilder, k: u32) -> Value {
    let into = b.fresh(Type::Top);
    b.push(Instr::Const {
        idx: ConstIdx(k),
        into,
    })
    .unwrap();
    into
}

fn add(b: &mut FunctionBuilder, lhs: Value, rhs: Value) -> Value {
    let into = b.fresh(Type::Top);
    b.push(Instr::BinOp {
        op: BinOp::Add,
        lhs,
        rhs,
        into,
    })
    .unwrap();
    into
}

#[test]
fn loop_phi() {
    // i = 0; while truth(n): i = i + 1; return i
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let n = v(0);
    let i0 = konst(&mut b, 0);
    b.write_variable(X, b.entry(), i0).unwrap();
    let hdr = b.new_block();
    let exit = b.new_block();
    let c0 = cond(&mut b, n);
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: hdr,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(hdr).unwrap();
    let c1 = cond(&mut b, n);
    let body = b.new_block();
    b.set_term(
        hdr,
        Terminator::Branch {
            cond: c1,
            then_to: body,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    let i_cur = b.read_variable(X, body).unwrap();
    let one = konst(&mut b, 1);
    let i2 = add(&mut b, i_cur, one);
    b.write_variable(X, body, i2).unwrap();
    b.set_term(body, Terminator::Jump { to: hdr }).unwrap();

    b.switch_to(exit).unwrap();
    let i_end = b.read_variable(X, exit).unwrap();
    b.set_term(exit, Terminator::Return { value: i_end })
        .unwrap();

    b.seal_block(body).unwrap();
    b.seal_block(hdr).unwrap();
    b.seal_block(exit).unwrap();
    b.seal_block(b.entry()).unwrap();
    let f = b.finish().unwrap();

    // Blocks: 0 preheader, 1 header, 2 exit, 3 body (creation order).
    assert_eq!(f.blocks[0].phis.len(), 0);
    assert_eq!(f.blocks[3].phis.len(), 0);
    assert_eq!(
        f.blocks[1].phis,
        vec![Phi {
            into: f.blocks[1].phis[0].into,
            incomings: vec![(i0, BlockId(0)), (i2, BlockId(3))].into(),
        }]
    );
    let p = f.blocks[1].phis[0].into;
    // The loop-carried read in the body resolved to the header phi.
    assert_eq!(
        f.blocks[3].instrs[1],
        Instr::BinOp {
            op: BinOp::Add,
            lhs: p,
            rhs: one,
            into: i2,
        }
    );
    // The exit merges the initial value and the phi.
    assert_eq!(
        f.blocks[2].phis,
        vec![Phi {
            into: f.blocks[2].phis[0].into,
            incomings: vec![(i0, BlockId(0)), (p, BlockId(1))].into(),
        }]
    );
    assert_eq!(
        f.blocks[2].term,
        Terminator::Return {
            value: f.blocks[2].phis[0].into
        }
    );
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn nested_loops_phi() {
    // i = 0; while truth(n): j = 0; while truth(n): j = j + 1; i = i + 1; return i
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let n = v(0);
    let outer = b.new_block();
    let after = b.new_block();
    let inner = b.new_block();
    let inner_body = b.new_block();
    let inner_exit = b.new_block();
    let exit = b.new_block();

    let i0 = konst(&mut b, 0);
    b.write_variable(X, b.entry(), i0).unwrap();
    b.set_term(b.entry(), Terminator::Jump { to: outer })
        .unwrap();

    b.switch_to(outer).unwrap();
    let c1 = cond(&mut b, n);
    b.set_term(
        outer,
        Terminator::Branch {
            cond: c1,
            then_to: after,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(after).unwrap();
    let j0 = konst(&mut b, 1);
    b.write_variable(U, after, j0).unwrap();
    b.set_term(after, Terminator::Jump { to: inner }).unwrap();

    b.switch_to(inner).unwrap();
    let c2 = cond(&mut b, n);
    b.set_term(
        inner,
        Terminator::Branch {
            cond: c2,
            then_to: inner_body,
            else_to: inner_exit,
        },
    )
    .unwrap();

    b.switch_to(inner_body).unwrap();
    let j_cur = b.read_variable(U, inner_body).unwrap();
    let one = konst(&mut b, 2);
    let j2 = add(&mut b, j_cur, one);
    b.write_variable(U, inner_body, j2).unwrap();
    b.set_term(inner_body, Terminator::Jump { to: inner })
        .unwrap();

    b.switch_to(inner_exit).unwrap();
    let i_cur = b.read_variable(X, inner_exit).unwrap();
    let one_outer = konst(&mut b, 3);
    let i2 = add(&mut b, i_cur, one_outer);
    b.write_variable(X, inner_exit, i2).unwrap();
    b.set_term(inner_exit, Terminator::Jump { to: outer })
        .unwrap();

    b.switch_to(exit).unwrap();
    let i_end = b.read_variable(X, exit).unwrap();
    b.set_term(exit, Terminator::Return { value: i_end })
        .unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();

    // Blocks: 0 entry, 1 outer header, 2 between loops, 3 inner header,
    // 4 inner body, 5 inner exit, 6 exit.
    let p_outer = f.blocks[1].phis[0].into;
    let p_inner = f.blocks[3].phis[0].into;
    assert_eq!(
        f.blocks[1].phis,
        vec![Phi {
            into: p_outer,
            incomings: vec![(i0, BlockId(0)), (i2, BlockId(5))].into(),
        }]
    );
    assert_eq!(
        f.blocks[3].phis,
        vec![Phi {
            into: p_inner,
            incomings: vec![(j0, BlockId(2)), (j2, BlockId(4))].into(),
        }]
    );
    // Single-predecessor blocks carry no phis; the reads resolved through
    // them to the loop phis.
    assert!(f.blocks[5].phis.is_empty());
    assert_eq!(
        f.blocks[5].instrs,
        vec![
            Instr::Const {
                idx: ConstIdx(3),
                into: one_outer,
            },
            Instr::BinOp {
                op: BinOp::Add,
                lhs: p_outer,
                rhs: one_outer,
                into: i2,
            },
        ]
    );
    assert_eq!(
        f.blocks[4].instrs[1],
        Instr::BinOp {
            op: BinOp::Add,
            lhs: p_inner,
            rhs: one,
            into: j2,
        }
    );
    assert!(f.blocks[6].phis.is_empty());
    assert_eq!(f.blocks[6].term, Terminator::Return { value: p_outer });
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn irreducible_cfg_phi() {
    // Two headers that jump to each other: a cycle with two entries.
    // x = 0
    // if truth(p): goto L1 else goto L2
    // L1: x = x + 1; if truth(p): goto L2 else goto E
    // L2: x = x + 2; if truth(p): goto L1 else goto E
    // E: return x
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let l1 = b.new_block();
    let l2 = b.new_block();
    let e = b.new_block();

    let x0 = konst(&mut b, 0);
    b.write_variable(X, b.entry(), x0).unwrap();
    let c0 = cond(&mut b, p);
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: l1,
            else_to: l2,
        },
    )
    .unwrap();

    b.switch_to(l1).unwrap();
    let x_in_l1 = b.read_variable(X, l1).unwrap();
    let x1 = add(&mut b, x_in_l1, x0);
    b.write_variable(X, l1, x1).unwrap();
    let c1 = cond(&mut b, p);
    b.set_term(
        l1,
        Terminator::Branch {
            cond: c1,
            then_to: l2,
            else_to: e,
        },
    )
    .unwrap();

    b.switch_to(l2).unwrap();
    let x_in_l2 = b.read_variable(X, l2).unwrap();
    let x2 = add(&mut b, x_in_l2, x0);
    b.write_variable(X, l2, x2).unwrap();
    let c2 = cond(&mut b, p);
    b.set_term(
        l2,
        Terminator::Branch {
            cond: c2,
            then_to: l1,
            else_to: e,
        },
    )
    .unwrap();

    b.switch_to(e).unwrap();
    let x_end = b.read_variable(X, e).unwrap();
    b.set_term(e, Terminator::Return { value: x_end }).unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();

    // Blocks: 0 entry, 1 L1, 2 L2, 3 E.
    assert_eq!(
        f.blocks[1].phis,
        vec![Phi {
            into: f.blocks[1].phis[0].into,
            incomings: vec![(x0, BlockId(0)), (x2, BlockId(2))].into(),
        }]
    );
    assert_eq!(
        f.blocks[2].phis,
        vec![Phi {
            into: f.blocks[2].phis[0].into,
            incomings: vec![(x0, BlockId(0)), (x1, BlockId(1))].into(),
        }]
    );
    assert_eq!(
        f.blocks[3].phis,
        vec![Phi {
            into: f.blocks[3].phis[0].into,
            incomings: vec![(x1, BlockId(1)), (x2, BlockId(2))].into(),
        }]
    );
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn trivial_phi_removed() {
    // A diamond that writes the same value on both arms: the join's phi is
    // trivial and disappears.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let left = b.new_block();
    let right = b.new_block();
    let join = b.new_block();

    let u0 = konst(&mut b, 0);
    b.write_variable(U, b.entry(), u0).unwrap();
    let c = cond(&mut b, p);
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c,
            then_to: left,
            else_to: right,
        },
    )
    .unwrap();
    b.set_term(left, Terminator::Jump { to: join }).unwrap();
    b.set_term(right, Terminator::Jump { to: join }).unwrap();

    b.switch_to(join).unwrap();
    let u = b.read_variable(U, join).unwrap();
    b.set_term(join, Terminator::Return { value: u }).unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();

    assert!(f.blocks.iter().all(|b| b.phis.is_empty()));
    assert_eq!(f.blocks[3].term, Terminator::Return { value: u0 });
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn trivial_phi_removed_recursively() {
    // The variable is never written inside two nested loops: every phi
    // degenerates to a self-reference or a single operand and the removal
    // cascades until no phi is left.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let outer = b.new_block();
    let inner = b.new_block();
    let inner_body = b.new_block();
    let after = b.new_block();
    let exit = b.new_block();

    let x0 = konst(&mut b, 0);
    b.write_variable(X, b.entry(), x0).unwrap();
    b.set_term(b.entry(), Terminator::Jump { to: outer })
        .unwrap();

    b.switch_to(outer).unwrap();
    let c1 = cond(&mut b, p);
    b.set_term(
        outer,
        Terminator::Branch {
            cond: c1,
            then_to: inner,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(inner).unwrap();
    let c2 = cond(&mut b, p);
    b.set_term(
        inner,
        Terminator::Branch {
            cond: c2,
            then_to: inner_body,
            else_to: after,
        },
    )
    .unwrap();

    b.set_term(inner_body, Terminator::Jump { to: inner })
        .unwrap();
    b.set_term(after, Terminator::Jump { to: outer }).unwrap();

    b.switch_to(exit).unwrap();
    let x = b.read_variable(X, exit).unwrap();
    b.set_term(exit, Terminator::Return { value: x }).unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();

    assert!(f.blocks.iter().all(|b| b.phis.is_empty()));
    assert_eq!(f.blocks[5].term, Terminator::Return { value: x0 });
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn exception_edge_is_predecessor() {
    // try: x = a + a except: ... — the handler block merges the value from
    // the raising block through the exception edge.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let a = v(0);
    let handler = b.new_block();
    let other = b.new_block();
    let join = b.new_block();

    let x = add(&mut b, a, a); // may raise
    b.write_variable(X, b.entry(), x).unwrap();
    b.set_handler(b.entry(), Some(handler)).unwrap();
    b.set_term(b.entry(), Terminator::Jump { to: other })
        .unwrap();
    b.set_term(handler, Terminator::Jump { to: join }).unwrap();

    b.switch_to(other).unwrap();
    let x2 = add(&mut b, a, a);
    b.write_variable(X, other, x2).unwrap();
    b.set_term(other, Terminator::Jump { to: join }).unwrap();

    b.switch_to(join).unwrap();
    let x_join = b.read_variable(X, join).unwrap();
    b.set_term(join, Terminator::Return { value: x_join })
        .unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();

    // Blocks: 0 entry (raising), 1 handler, 2 other, 3 join. The handler's
    // only predecessor is the exception edge, so x reaches it unmerged.
    assert_eq!(
        f.blocks[3].phis,
        vec![Phi {
            into: f.blocks[3].phis[0].into,
            incomings: vec![(x, BlockId(1)), (x2, BlockId(2))].into(),
        }]
    );
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn read_before_write_is_undefined() {
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let back = b.new_block();
    let exit = b.new_block();
    let c = cond(&mut b, p);
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c,
            then_to: back,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(back).unwrap();
    b.read_variable(U, back).unwrap(); // never written anywhere
    b.set_term(back, Terminator::Jump { to: b.entry() })
        .unwrap();

    b.switch_to(exit).unwrap();
    b.set_term(exit, Terminator::Return { value: p }).unwrap();

    match b.seal_all() {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(1));
            assert!(
                BuildError::UndefinedVariable { var, block }
                    .to_string()
                    .contains("never written")
            );
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn read_before_write_in_sealed_block_is_undefined() {
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    b.set_term(b.entry(), Terminator::Return { value: v(0) })
        .unwrap();
    b.seal_block(b.entry()).unwrap();
    match b.read_variable(U, b.entry()) {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn long_single_predecessor_chain_reads_iteratively() {
    // A chain of sealed blocks is followed one link at a time, without a
    // phi per link and without recursion; walking 4000 links used to
    // overflow the stack and take quadratic time.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let n = 4000;
    let mut prev = b.entry();
    for _ in 0..n {
        let id = b.new_block();
        b.set_term(prev, Terminator::Jump { to: id }).unwrap();
        b.seal_block(id).unwrap();
        prev = id;
    }
    let last = prev;
    b.seal_block(b.entry()).unwrap();
    let val = b.read_variable(Sym(0), last).unwrap(); // the parameter
    assert_eq!(val, v(0));
    b.set_term(last, Terminator::Return { value: val }).unwrap();
    let f = b.finish().unwrap();
    assert_eq!(f.blocks.len(), n + 1);
    assert!(f.blocks.iter().all(|blk| blk.phis.is_empty()));
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn read_in_single_predecessor_cycle_is_undefined() {
    // `while c: pass; return x`: the entry and the body form a cycle of
    // single-predecessor blocks and x is never written.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let body = b.new_block();
    let exit = b.new_block();
    let c0 = cond(&mut b, p);
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: body,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    b.set_term(body, Terminator::Jump { to: b.entry() })
        .unwrap();

    b.switch_to(exit).unwrap();
    let x = b.read_variable(X, exit).unwrap();
    b.set_term(exit, Terminator::Return { value: x }).unwrap();

    match b.seal_all() {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, X);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn entry_back_edge_removal_requires_dominating_definition() {
    // x is written only in a loop body whose terminator jumps back to the
    // entry: reading x in the entry has no definition on the first pass,
    // and the trivial phi must not silently adopt the body's value.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let body = b.new_block();
    let exit = b.new_block();
    let x = b.read_variable(X, b.entry()).unwrap();
    let c0 = cond(&mut b, p);
    let use_x = b.fresh(Type::Top);
    b.push(Instr::Copy {
        src: x,
        into: use_x,
    })
    .unwrap();
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: body,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    let v_body = konst(&mut b, 0);
    b.write_variable(X, body, v_body).unwrap();
    b.set_term(body, Terminator::Jump { to: b.entry() })
        .unwrap();

    b.switch_to(exit).unwrap();
    b.set_term(exit, Terminator::Return { value: p }).unwrap();

    b.seal_block(body).unwrap();
    b.seal_block(exit).unwrap();
    match b.seal_block(b.entry()) {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, X);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn phi_types_track_incoming_joins() {
    // A join phi is filled while one incoming is still an unresolved loop
    // phi (typed Bottom); the loop phi later merges a BigInt and an Int64.
    // The join's declared type must follow, or an unboxed IAdd on it would
    // slip past the verifier.
    let mut params = one_param();
    params.positional.push(Sym(4));
    let mut b = FunctionBuilder::new(Sym(3), params, vec![Type::Top, Type::Int64]).unwrap();
    let (q, n) = (v(0), v(1));

    let big = b.fresh(Type::BigInt);
    b.push(Instr::Const {
        idx: ConstIdx(0),
        into: big,
    })
    .unwrap();
    b.write_variable(X, b.entry(), big).unwrap();
    let hdr = b.new_block();
    let body = b.new_block();
    let arm2 = b.new_block();
    let join = b.new_block();
    let c0 = cond(&mut b, q);
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: hdr,
            else_to: arm2,
        },
    )
    .unwrap();

    b.switch_to(hdr).unwrap();
    let c1 = cond(&mut b, q);
    b.set_term(
        hdr,
        Terminator::Branch {
            cond: c1,
            then_to: body,
            else_to: join,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    b.read_variable(X, body).unwrap(); // resolves to the loop phi in hdr
    b.set_term(body, Terminator::Jump { to: hdr }).unwrap();

    b.switch_to(arm2).unwrap();
    b.write_variable(X, arm2, n).unwrap();
    b.set_term(arm2, Terminator::Jump { to: join }).unwrap();

    b.switch_to(join).unwrap();
    let x_join = b.read_variable(X, join).unwrap();
    let s = b.fresh(Type::Int64);
    b.push(Instr::IAdd {
        lhs: x_join,
        rhs: n,
        into: s,
    })
    .unwrap();
    b.set_term(join, Terminator::Jump { to: hdr }).unwrap();

    b.seal_block(body).unwrap();
    b.seal_block(join).unwrap(); // join phi = phi[loop phi, n] while the loop phi is Bottom
    b.seal_block(arm2).unwrap();
    b.seal_block(hdr).unwrap(); // loop phi = phi[big, join phi]
    b.seal_block(b.entry()).unwrap();
    let f = b.finish().unwrap();

    let p = f.blocks[1].phis[0].into;
    let join_phi = f.blocks[4].phis[0].into;
    assert_eq!(
        f.blocks[4].phis,
        vec![Phi {
            into: join_phi,
            incomings: vec![(p, BlockId(1)), (n, BlockId(3))].into(),
        }]
    );
    assert_eq!(f.types[p.0 as usize], Type::BigInt);
    assert_eq!(f.types[join_phi.0 as usize], Type::BigInt);

    // The verifier rejects the merge itself: the join phi (BigInt) takes the
    // unboxed Int64 `n` without a Box, before its unboxed use is reached.
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::PhiBoxingMismatch {
            value,
            incoming,
            found,
            ..
        }) => {
            assert_eq!(value, join_phi);
            assert_eq!(incoming, n);
            assert_eq!(found, Type::Int64);
        }
        other => panic!("expected a phi boxing error, got {other:?}"),
    }
}

#[test]
fn finish_requires_term_and_seal() {
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let extra = b.new_block();
    b.set_term(b.entry(), Terminator::Return { value: Value(0) })
        .unwrap();
    b.seal_block(b.entry()).unwrap();
    match b.finish() {
        Err(BuildError::MissingTerminator { block }) => assert_eq!(block, extra),
        other => panic!("expected missing terminator, got {other:?}"),
    }

    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    b.set_term(b.entry(), Terminator::Return { value: Value(0) })
        .unwrap();
    match b.finish() {
        Err(BuildError::UnsealedBlock { block }) => assert_eq!(block, BlockId(0)),
        other => panic!("expected unsealed block, got {other:?}"),
    }

    // Wrong parameter-type count is rejected up front.
    assert!(matches!(
        FunctionBuilder::new(Sym(3), one_param(), vec![]),
        Err(BuildError::ParamCount {
            expected: 1,
            given: 0
        })
    ));
}

fn blk(phis: Vec<Phi>, instrs: Vec<Instr>, term: Terminator) -> Block {
    Block {
        phis,
        instrs,
        term,
        handler: None,
    }
}

fn func(blocks: Vec<Block>, types: Vec<Type>) -> Function {
    func_flags(blocks, types, FnFlags(0))
}

fn func_flags(blocks: Vec<Block>, types: Vec<Type>, flags: FnFlags) -> Function {
    Function {
        qualname: Sym(3),
        params: Params::default(),
        blocks,
        entry: BlockId(0),
        locals: vec![],
        flags,
        types,
        facts: vec![],
        lines: vec![],
    }
}

fn diamond(extra: Option<Instr>) -> Function {
    // 0: v0 = const; v1 = truth(v0); branch v1 -> 1, 2
    // 1: v2 = const; jump 3        2: jump 3        3: [extra copy] return
    let mut b3 = vec![];
    if let Some(i) = extra {
        b3.push(i);
    }
    let ret = v(if b3.is_empty() { 0 } else { 3 });
    func(
        vec![
            blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Truth {
                        value: v(0),
                        into: v(1),
                    },
                ],
                Terminator::Branch {
                    cond: v(1),
                    then_to: BlockId(1),
                    else_to: BlockId(2),
                },
            ),
            blk(
                vec![],
                vec![Instr::Const {
                    idx: ConstIdx(1),
                    into: v(2),
                }],
                Terminator::Jump { to: BlockId(3) },
            ),
            blk(vec![], vec![], Terminator::Jump { to: BlockId(3) }),
            blk(vec![], b3, Terminator::Return { value: ret }),
        ],
        vec![Type::Top, Type::Bool, Type::Top, Type::Top],
    )
}

#[test]
fn verifier_accepts_valid_function() {
    // No extra copy: block 3 returns v0, which dominates everything.
    verify::verify_function(&diamond(None), &[]).unwrap();
    verify::verify_module(&coil_ir::ir::Module {
        name: Sym(0),
        file: "m.py".into(),
        functions: vec![diamond(None)],
        classes: vec![],
        consts: Default::default(),
        body: coil_ir::types::FnId(0),
    })
    .unwrap();
}

#[test]
fn instruction_ids_follow_block_order() {
    // Blocks filled out of creation order: the line table must end up keyed
    // by block-order instruction ids, the id space of `Function`.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let first = b.new_block();
    let second = b.new_block();
    b.set_term(b.entry(), Terminator::Jump { to: first })
        .unwrap();

    b.switch_to(second).unwrap();
    let a = b.fresh(Type::Top);
    let second_id = b
        .push(Instr::Const {
            idx: ConstIdx(0),
            into: a,
        })
        .unwrap();
    b.record_line(second_id, 30, 5).unwrap();
    b.set_term(second, Terminator::Return { value: a }).unwrap();

    b.switch_to(first).unwrap();
    let c = b.fresh(Type::Top);
    let first_id = b
        .push(Instr::Const {
            idx: ConstIdx(1),
            into: c,
        })
        .unwrap();
    b.record_line(first_id, 20, 5).unwrap();
    b.record_line(InstrId(99), 1, 1).unwrap_err(); // not an id `push` returned
    b.set_term(first, Terminator::Jump { to: second }).unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();

    assert_eq!(f.instr_id(BlockId(1), 0), Some(InstrId(0)));
    assert_eq!(f.instr_id(BlockId(2), 0), Some(InstrId(1)));
    assert_eq!(f.line_of(InstrId(0)), Some((20, 5)));
    assert_eq!(f.line_of(InstrId(1)), Some((30, 5)));
    assert_eq!(f.lines, vec![(InstrId(0), 20, 5), (InstrId(1), 30, 5)]);
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn operand_does_not_dominate_use() {
    // The copy in block 3 uses v2, defined in block 1, which does not
    // dominate the join.
    let f = diamond(Some(Instr::Copy {
        src: v(2),
        into: v(3),
    }));
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::OperandNotDominated {
            block,
            instr,
            value,
            def_block,
        }) => {
            assert_eq!(
                (block, instr, value, def_block),
                (BlockId(3), InstrId(3), v(2), BlockId(1))
            );
            assert!(
                VerifyError::OperandNotDominated {
                    block,
                    instr,
                    value,
                    def_block,
                }
                .to_string()
                .contains("block 3, instr 3")
            );
        }
        other => panic!("expected operand not dominated, got {other:?}"),
    }
}

#[test]
fn phi_incoming_does_not_dominate_predecessor() {
    let f = func(
        vec![
            blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Truth {
                        value: v(0),
                        into: v(1),
                    },
                ],
                Terminator::Branch {
                    cond: v(1),
                    then_to: BlockId(1),
                    else_to: BlockId(2),
                },
            ),
            blk(
                vec![],
                vec![Instr::Const {
                    idx: ConstIdx(1),
                    into: v(2),
                }],
                Terminator::Jump { to: BlockId(3) },
            ),
            blk(vec![], vec![], Terminator::Jump { to: BlockId(3) }),
            blk(
                vec![Phi {
                    into: v(3),
                    incomings: vec![(v(2), BlockId(1)), (v(2), BlockId(2))].into(),
                }],
                vec![],
                Terminator::Return { value: v(3) },
            ),
        ],
        vec![Type::Top, Type::Bool, Type::Top, Type::Top],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::PhiIncomingNotDominated {
            block,
            value,
            from,
            incoming,
            def_block,
        }) => {
            assert_eq!(block, BlockId(3));
            assert_eq!(value, v(3));
            assert_eq!((from, incoming, def_block), (BlockId(2), v(2), BlockId(1)));
        }
        other => panic!("expected phi incoming not dominated, got {other:?}"),
    }
}

#[test]
fn phi_incomings_must_match_predecessors() {
    let f = func(
        vec![
            blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Truth {
                        value: v(0),
                        into: v(1),
                    },
                ],
                Terminator::Branch {
                    cond: v(1),
                    then_to: BlockId(1),
                    else_to: BlockId(2),
                },
            ),
            blk(vec![], vec![], Terminator::Jump { to: BlockId(3) }),
            blk(vec![], vec![], Terminator::Jump { to: BlockId(3) }),
            blk(
                vec![Phi {
                    into: v(2),
                    incomings: vec![(v(0), BlockId(1))].into(),
                }],
                vec![],
                Terminator::Return { value: v(2) },
            ),
        ],
        vec![Type::Top, Type::Bool, Type::Top],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::PhiPredecessorMismatch { block, value }) => {
            assert_eq!((block, value), (BlockId(3), v(2)));
        }
        other => panic!("expected phi predecessor mismatch, got {other:?}"),
    }
}

#[test]
fn may_raise_needs_resolved_handler() {
    let mut f = func(
        vec![blk(
            vec![],
            vec![Instr::BinOp {
                op: BinOp::Add,
                lhs: v(0),
                rhs: v(1),
                into: v(2),
            }],
            Terminator::Return { value: v(2) },
        )],
        vec![Type::Top, Type::Top, Type::Top],
    );
    f.blocks[0].handler = Some(BlockId(3));
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::UnresolvedHandler {
            block,
            instr,
            handler,
        }) => {
            assert_eq!((block, instr), (BlockId(0), InstrId(0)));
            assert_eq!(handler, BlockId(3));
            assert!(
                VerifyError::UnresolvedHandler {
                    block,
                    instr,
                    handler,
                }
                .to_string()
                .contains("block 0, instr 0")
            );
        }
        other => panic!("expected unresolved handler, got {other:?}"),
    }

    // A handler that is the raising block itself never resolves either.
    let mut self_handler = diamond(None);
    self_handler.blocks[0].handler = Some(BlockId(0));
    assert!(matches!(
        verify::verify_function(&self_handler, &[]),
        Err(VerifyError::UnresolvedHandler { .. })
    ));

    // A dangling handler edge with nothing that raises is still a broken
    // terminator.
    let mut dangling = func(
        vec![blk(
            vec![],
            vec![Instr::Const {
                idx: ConstIdx(0),
                into: v(0),
            }],
            Terminator::Return { value: v(0) },
        )],
        vec![Type::Top],
    );
    dangling.blocks[0].handler = Some(BlockId(9));
    match verify::verify_function(&dangling, &[]) {
        Err(VerifyError::BadTerminator { block, target }) => {
            assert_eq!((block, target), (BlockId(0), BlockId(9)));
        }
        other => panic!("expected bad terminator, got {other:?}"),
    }
}

#[test]
fn boxed_unboxed_consistency() {
    // Unboxed arithmetic on a boxed int.
    let f = func(
        vec![blk(
            vec![],
            vec![Instr::IAdd {
                lhs: v(0),
                rhs: v(1),
                into: v(2),
            }],
            Terminator::Return { value: v(2) },
        )],
        vec![Type::BigInt, Type::Int64, Type::Int64],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::OperandType {
            value,
            expected,
            found,
            ..
        }) => {
            assert_eq!(value, v(0));
            assert_eq!(expected, Type::Int64);
            assert_eq!(found, Type::BigInt);
        }
        other => panic!("expected operand type mismatch, got {other:?}"),
    }

    // A dynamic operation on an unboxed value.
    let f = func(
        vec![blk(
            vec![],
            vec![Instr::BinOp {
                op: BinOp::Add,
                lhs: v(0),
                rhs: v(1),
                into: v(2),
            }],
            Terminator::Return { value: v(2) },
        )],
        vec![Type::Int64, Type::Top, Type::Top],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::BoxedOperand { value, found, .. }) => {
            assert_eq!(value, v(0));
            assert_eq!(found, Type::Int64);
        }
        other => panic!("expected boxed operand, got {other:?}"),
    }

    // An unboxed result must carry the unboxed type.
    let f = func(
        vec![blk(
            vec![],
            vec![Instr::ICmp {
                op: ICmpOp::Eq,
                lhs: v(0),
                rhs: v(1),
                into: v(2),
            }],
            Terminator::Return { value: v(2) },
        )],
        vec![Type::Int64, Type::Int64, Type::Top],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::ResultType {
            value,
            expected,
            found,
            ..
        }) => {
            assert_eq!(value, v(2));
            assert_eq!(expected, Type::Bool);
            assert_eq!(found, Type::Top);
        }
        other => panic!("expected result type mismatch, got {other:?}"),
    }

    // Unbox produces exactly its scalar.
    let f = func(
        vec![blk(
            vec![],
            vec![Instr::Unbox {
                src: v(0),
                scalar: coil_ir::ir::Scalar::Int64,
                into: v(1),
            }],
            Terminator::Return { value: v(1) },
        )],
        vec![Type::Top, Type::BigInt],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::ResultType { value, .. }) => assert_eq!(value, v(1)),
        other => panic!("expected result type mismatch, got {other:?}"),
    }
}

#[test]
fn top_in_unboxed_operand() {
    let f = func(
        vec![blk(
            vec![],
            vec![Instr::FAdd {
                lhs: v(0),
                rhs: v(1),
                into: v(2),
            }],
            Terminator::Return { value: v(2) },
        )],
        vec![Type::Top, Type::Float64, Type::Float64],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::TopUnboxedOperand {
            block,
            instr,
            value,
        }) => {
            assert_eq!((block, instr, value), (BlockId(0), InstrId(0), v(0)));
            assert!(
                VerifyError::TopUnboxedOperand {
                    block,
                    instr,
                    value
                }
                .to_string()
                .contains("type Top")
            );
        }
        other => panic!("expected top in unboxed operand, got {other:?}"),
    }

    // A boxed operand may be Top; a branch condition may not.
    let f = func(
        vec![blk(
            vec![],
            vec![],
            Terminator::Branch {
                cond: v(0),
                then_to: BlockId(0),
                else_to: BlockId(0),
            },
        )],
        vec![Type::Top],
    );
    assert!(matches!(
        verify::verify_function(&f, &[]),
        Err(VerifyError::TopUnboxedOperand { value, .. }) if value == v(0)
    ));
}

#[test]
fn terminator_targets_real_block() {
    let f = func(
        vec![blk(vec![], vec![], Terminator::Jump { to: BlockId(4) })],
        vec![],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::BadTerminator { block, target }) => {
            assert_eq!((block, target), (BlockId(0), BlockId(4)));
        }
        other => panic!("expected bad terminator, got {other:?}"),
    }

    let f = func(vec![blk(vec![], vec![], Terminator::Unreachable)], vec![]);
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn yield_only_in_generator() {
    let body = |flags| {
        func_flags(
            vec![blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Yield {
                        state: 0,
                        value: v(0),
                        into: v(1),
                    },
                ],
                Terminator::Return { value: v(1) },
            )],
            vec![Type::Top, Type::Top],
            flags,
        )
    };
    match verify::verify_function(&body(FnFlags(0)), &[]) {
        Err(VerifyError::YieldOutsideGenerator { block, instr }) => {
            assert_eq!((block, instr), (BlockId(0), InstrId(1)));
            assert!(
                VerifyError::YieldOutsideGenerator { block, instr }
                    .to_string()
                    .contains("yield")
            );
        }
        other => panic!("expected yield outside generator, got {other:?}"),
    }
    verify::verify_function(&body(FnFlags::GENERATOR), &[]).unwrap();
    verify::verify_function(&body(FnFlags::ASYNC_GEN), &[]).unwrap();
}

#[test]
fn barrier_matches_operand_type() {
    let store = |value_ty: Type, barrier: BarrierKind| {
        func(
            vec![blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Const {
                        idx: ConstIdx(1),
                        into: v(1),
                    },
                    Instr::StoreField {
                        obj: v(0),
                        shape: ShapeId(0),
                        slot: 0,
                        value: v(1),
                        barrier,
                    },
                ],
                Terminator::Return { value: v(0) },
            )],
            vec![Type::Top, value_ty],
        )
    };
    verify::verify_function(&store(Type::Str, BarrierKind::None), &[]).unwrap();
    verify::verify_function(&store(Type::Ext(TypeRef(Sym(4))), BarrierKind::Ext), &[]).unwrap();
    verify::verify_function(&store(Type::Top, BarrierKind::Dynamic), &[]).unwrap();
    // Dynamic tests the regime at run time, so it is accepted for any type.
    verify::verify_function(&store(Type::Str, BarrierKind::Dynamic), &[]).unwrap();
    verify::verify_function(
        &store(Type::Ext(TypeRef(Sym(4))), BarrierKind::Dynamic),
        &[],
    )
    .unwrap();
    verify::verify_function(
        &store(
            Type::Union(vec![Type::Str, Type::Ext(TypeRef(Sym(4)))].into()),
            BarrierKind::Dynamic,
        ),
        &[],
    )
    .unwrap();

    match verify::verify_function(&store(Type::Top, BarrierKind::None), &[]) {
        Err(VerifyError::BarrierMismatch {
            value,
            barrier,
            found,
            ..
        }) => {
            assert_eq!(value, v(1));
            assert_eq!(barrier, BarrierKind::None);
            assert_eq!(found, Type::Top);
        }
        other => panic!("expected barrier mismatch, got {other:?}"),
    }
    assert!(matches!(
        verify::verify_function(&store(Type::Str, BarrierKind::Ext), &[]),
        Err(VerifyError::BarrierMismatch { .. })
    ));
    assert!(matches!(
        verify::verify_function(&store(Type::Ext(TypeRef(Sym(4))), BarrierKind::None), &[]),
        Err(VerifyError::BarrierMismatch { .. })
    ));
}

#[test]
fn barrier_counts_instances_of_ext_base_classes() {
    // Spec 8.3: a compiled class with an extension base has counted-regime
    // instances, so storing one needs an Ext barrier even though the value's
    // type is `Inst`, not `Ext`.
    let class = |ext| ClassDesc {
        name: Sym(5),
        bases: Box::from([]),
        body: FnId(0),
        methods: Box::from([]),
        has_ext_base: ext,
    };
    let classes = vec![class(true), class(false)];
    let store = |cls: ClassId, barrier| {
        func(
            vec![blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Const {
                        idx: ConstIdx(1),
                        into: v(1),
                    },
                    Instr::StoreField {
                        obj: v(0),
                        shape: ShapeId(0),
                        slot: 0,
                        value: v(1),
                        barrier,
                    },
                ],
                Terminator::Return { value: v(0) },
            )],
            vec![Type::Top, Type::Inst { cls, exact: true }],
        )
    };
    verify::verify_function(&store(ClassId(0), BarrierKind::Ext), &classes).unwrap();
    assert!(matches!(
        verify::verify_function(&store(ClassId(0), BarrierKind::None), &classes),
        Err(VerifyError::BarrierMismatch { .. })
    ));
    verify::verify_function(&store(ClassId(1), BarrierKind::None), &classes).unwrap();
    assert!(matches!(
        verify::verify_function(&store(ClassId(1), BarrierKind::Ext), &classes),
        Err(VerifyError::BarrierMismatch { .. })
    ));
}

#[test]
fn values_must_be_defined_once_and_typed() {
    let f = func(
        vec![blk(
            vec![],
            vec![
                Instr::Const {
                    idx: ConstIdx(0),
                    into: v(0),
                },
                Instr::Const {
                    idx: ConstIdx(1),
                    into: v(0),
                },
            ],
            Terminator::Return { value: v(0) },
        )],
        vec![Type::Top],
    );
    assert!(matches!(
        verify::verify_function(&f, &[]),
        Err(VerifyError::ValueDefinedTwice { block: _, instr, value }) if instr == InstrId(1) && value == v(0)
    ));

    let f = func(
        vec![blk(
            vec![],
            vec![Instr::Copy {
                src: v(1),
                into: v(0),
            }],
            Terminator::Return { value: v(0) },
        )],
        vec![Type::Top, Type::Top],
    );
    assert!(matches!(
        verify::verify_function(&f, &[]),
        Err(VerifyError::ValueNotDefined { block: _, instr, value }) if instr == InstrId(0) && value == v(1)
    ));

    let f = func(
        vec![blk(
            vec![],
            vec![Instr::Copy {
                src: v(5),
                into: v(0),
            }],
            Terminator::Return { value: v(0) },
        )],
        vec![Type::Top],
    );
    assert!(matches!(
        verify::verify_function(&f, &[]),
        Err(VerifyError::MissingType { value, .. }) if value == v(5)
    ));
}

#[test]
fn parameter_values_cannot_be_redefined() {
    // v0 is the parameter; an instruction result may not take its value.
    let f = Function {
        qualname: Sym(3),
        params: one_param(),
        blocks: vec![blk(
            vec![],
            vec![Instr::Const {
                idx: ConstIdx(0),
                into: v(0),
            }],
            Terminator::Return { value: v(0) },
        )],
        entry: BlockId(0),
        locals: vec![],
        flags: FnFlags(0),
        types: vec![Type::Top],
        facts: Vec::new(),
        lines: vec![],
    };
    assert!(matches!(
        verify::verify_function(&f, &[]),
        Err(VerifyError::ValueDefinedTwice { block: _, instr, value }) if instr == InstrId(0) && value == v(0)
    ));
}

#[test]
fn unreachable_predecessor_is_not_dominance_checked() {
    // Block 4 is dead but jumps into the join, so the join's phi carries an
    // incoming from it; nothing on that dead edge is dominance-checked.
    let f = func(
        vec![
            blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Truth {
                        value: v(0),
                        into: v(1),
                    },
                ],
                Terminator::Branch {
                    cond: v(1),
                    then_to: BlockId(1),
                    else_to: BlockId(2),
                },
            ),
            blk(
                vec![],
                vec![Instr::Const {
                    idx: ConstIdx(1),
                    into: v(2),
                }],
                Terminator::Jump { to: BlockId(3) },
            ),
            blk(vec![], vec![], Terminator::Jump { to: BlockId(3) }),
            blk(
                vec![Phi {
                    into: v(3),
                    incomings: vec![(v(2), BlockId(1)), (v(0), BlockId(2)), (v(2), BlockId(4))]
                        .into(),
                }],
                vec![],
                Terminator::Return { value: v(3) },
            ),
            blk(
                vec![],
                vec![Instr::Const {
                    idx: ConstIdx(2),
                    into: v(4),
                }],
                Terminator::Jump { to: BlockId(3) },
            ),
        ],
        vec![Type::Top, Type::Bool, Type::Top, Type::Top, Type::Top],
    );
    verify::verify_function(&f, &[]).unwrap();

    // Without the dead edge the same incoming is a dominance violation.
    let mut live = f.clone();
    live.blocks[4].term = Terminator::Unreachable;
    // The phi still claims an incoming from block 4, whose predecessors no
    // longer include it.
    assert!(matches!(
        verify::verify_function(&live, &[]),
        Err(VerifyError::PhiPredecessorMismatch { .. })
    ));
}

#[test]
fn removed_trivial_phi_is_replaced_in_other_variables() {
    // x = 0; while truth(n): y = x  (x is never reassigned); return y
    // Reading x in the unsealed header makes an incomplete phi and y is
    // written with it. Sealing proves the phi trivial; y must then read as
    // the phi's replacement, not the removed phi.
    const Y: Sym = Sym(4);
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let n = v(0);
    let x0 = konst(&mut b, 0);
    b.write_variable(X, b.entry(), x0).unwrap();
    let hdr = b.new_block();
    let body = b.new_block();
    let exit = b.new_block();
    b.set_term(b.entry(), Terminator::Jump { to: hdr }).unwrap();
    b.seal_block(b.entry()).unwrap();

    b.switch_to(hdr).unwrap();
    let x_hdr = b.read_variable(X, hdr).unwrap();
    b.write_variable(Y, hdr, x_hdr).unwrap();
    let c = cond(&mut b, n);
    b.set_term(
        hdr,
        Terminator::Branch {
            cond: c,
            then_to: body,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    b.set_term(body, Terminator::Jump { to: hdr }).unwrap();
    b.seal_block(body).unwrap();
    b.seal_block(hdr).unwrap();
    b.seal_block(exit).unwrap();

    b.switch_to(exit).unwrap();
    let y = b.read_variable(Y, exit).unwrap();
    assert_eq!(y, x0);
    b.set_term(exit, Terminator::Return { value: y }).unwrap();
    let f = b.finish().unwrap();
    assert!(f.blocks.iter().all(|blk| blk.phis.is_empty()));
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn nested_walrus_loop_phis_verify() {
    const Y: Sym = Sym(4);
    // Nested loops with two loop-carried variables, one written by a walrus
    // in the inner test (`while a and (y := ...): x += y`), and a read of
    // the other at the outer latch: the incomplete phis the unsealed headers
    // create must all resolve to live values once sealing completes.
    // Blocks in lowering order: 0 entry, 1 outer header, 2/3 outer arms,
    // 4 exit, 5 inner header, 6 inner-then, 7, 8 and-join, 9 inner body,
    // 10, 11 outer latch.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);

    let x0 = konst(&mut b, 0);
    b.write_variable(X, b.entry(), x0).unwrap();
    let y0 = konst(&mut b, 1);
    b.write_variable(Y, b.entry(), y0).unwrap();
    let hdr = b.new_block(); // 1
    b.set_term(b.entry(), Terminator::Jump { to: hdr }).unwrap();

    b.switch_to(hdr).unwrap();
    let x1 = konst(&mut b, 2); // the outer walrus write
    b.write_variable(X, hdr, x1).unwrap();
    let c1 = cond(&mut b, p);
    let arm = b.new_block(); // 2
    let done = b.new_block(); // 3
    let exit = b.new_block(); // 4
    b.set_term(
        hdr,
        Terminator::Branch {
            cond: c1,
            then_to: arm,
            else_to: done,
        },
    )
    .unwrap();
    b.set_term(arm, Terminator::Jump { to: exit }).unwrap();
    b.set_term(done, Terminator::Jump { to: exit }).unwrap();
    b.set_term(exit, Terminator::Return { value: x0 }).unwrap();

    // Outer body: the arm continues into the inner loop.
    let inner = b.new_block(); // 5
    b.set_term(arm, Terminator::Jump { to: inner }).unwrap();

    b.switch_to(inner).unwrap();
    let x_in = b.read_variable(X, inner).unwrap();
    // The inner condition reads Y too; the read must resolve (the nested
    // walrus writes it in the then-arm and the join reads it again).
    b.read_variable(Y, inner).unwrap();
    let c2 = cond(&mut b, x_in);
    let then_blk = b.new_block(); // 6
    let else_blk = b.new_block(); // 7
    let join = b.new_block(); // 8
    b.set_term(
        inner,
        Terminator::Branch {
            cond: c2,
            then_to: then_blk,
            else_to: else_blk,
        },
    )
    .unwrap();

    b.switch_to(then_blk).unwrap();
    let y_new = konst(&mut b, 3); // the inner walrus write
    b.write_variable(Y, then_blk, y_new).unwrap();
    b.set_term(then_blk, Terminator::Jump { to: join }).unwrap();
    b.set_term(else_blk, Terminator::Jump { to: join }).unwrap();

    b.switch_to(join).unwrap();
    let y_join = b.read_variable(Y, join).unwrap();
    let c3 = cond(&mut b, y_join);
    let body = b.new_block(); // 9
    let after = b.new_block(); // 10
    b.set_term(
        join,
        Terminator::Branch {
            cond: c3,
            then_to: body,
            else_to: after,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    let x_b = b.read_variable(X, body).unwrap();
    let y_b = b.read_variable(Y, body).unwrap();
    let x2 = add(&mut b, x_b, y_b);
    b.write_variable(X, body, x2).unwrap();
    b.set_term(body, Terminator::Jump { to: inner }).unwrap();
    let latch = b.new_block(); // 11
    b.set_term(after, Terminator::Jump { to: latch }).unwrap();
    b.set_term(done, Terminator::Jump { to: latch }).unwrap();
    b.switch_to(latch).unwrap();
    let x_l = b.read_variable(X, latch).unwrap();
    let use_x = b.fresh(Type::Top);
    b.push(Instr::Copy {
        src: x_l,
        into: use_x,
    })
    .unwrap();
    b.set_term(latch, Terminator::Jump { to: hdr }).unwrap();

    b.seal_all().unwrap();
    let f = b.finish().unwrap();
    verify::verify_function(&f, &[]).unwrap();
}

#[test]
fn phi_must_not_merge_boxed_and_unboxed() {
    // 0: v0 = const; v1 = truth(v0); branch -> 1, 2
    // 1: v2 = const (Int64); jump 3     2: jump 3     3: v3 = phi(v2, v0); return v3
    let f = func(
        vec![
            blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Truth {
                        value: v(0),
                        into: v(1),
                    },
                ],
                Terminator::Branch {
                    cond: v(1),
                    then_to: BlockId(1),
                    else_to: BlockId(2),
                },
            ),
            blk(
                vec![],
                vec![Instr::Const {
                    idx: ConstIdx(1),
                    into: v(2),
                }],
                Terminator::Jump { to: BlockId(3) },
            ),
            blk(vec![], vec![], Terminator::Jump { to: BlockId(3) }),
            blk(
                vec![Phi {
                    into: v(3),
                    incomings: vec![(v(2), BlockId(1)), (v(0), BlockId(2))].into(),
                }],
                vec![],
                Terminator::Return { value: v(3) },
            ),
        ],
        vec![Type::Top, Type::Bool, Type::Int64, Type::Top],
    );
    match verify::verify_function(&f, &[]) {
        Err(VerifyError::PhiBoxingMismatch {
            value,
            incoming,
            found,
            ..
        }) => {
            assert_eq!(value, v(3));
            assert_eq!(incoming, v(2));
            assert_eq!(found, Type::Int64);
        }
        other => panic!("expected a phi boxing error, got {other:?}"),
    }
}

/// 0 (handler 2): v0 = const; v1 = truth(v0) [may raise]; v2 = const; jump 1
/// 1: return v0        2: return `handler_returns`
fn raising_block(handler_returns: Value) -> Function {
    let mut f = func(
        vec![
            blk(
                vec![],
                vec![
                    Instr::Const {
                        idx: ConstIdx(0),
                        into: v(0),
                    },
                    Instr::Truth {
                        value: v(0),
                        into: v(1),
                    },
                    Instr::Const {
                        idx: ConstIdx(1),
                        into: v(2),
                    },
                ],
                Terminator::Jump { to: BlockId(1) },
            ),
            blk(vec![], vec![], Terminator::Return { value: v(0) }),
            blk(
                vec![],
                vec![],
                Terminator::Return {
                    value: handler_returns,
                },
            ),
        ],
        vec![Type::Top, Type::Bool, Type::Top],
    );
    f.blocks[0].handler = Some(BlockId(2));
    f
}

#[test]
fn handler_cannot_use_values_defined_after_a_raise() {
    // v0 exists before truth() can raise, so the handler may use it.
    verify::verify_function(&raising_block(v(0)), &[]).unwrap();
    // v2 is computed only if truth() did not raise.
    match verify::verify_function(&raising_block(v(2)), &[]) {
        Err(VerifyError::UsedAfterRaise {
            block,
            value,
            def_block,
        }) => {
            assert_eq!(block, BlockId(2));
            assert_eq!(value, v(2));
            assert_eq!(def_block, BlockId(0));
        }
        other => panic!("expected used-after-raise, got {other:?}"),
    }
}

#[test]
fn unreachable_terminators_are_type_checked() {
    // Block 1 is unreachable and branches on a Top value, not a Bool.
    let f = func(
        vec![
            blk(
                vec![],
                vec![Instr::Const {
                    idx: ConstIdx(0),
                    into: v(0),
                }],
                Terminator::Return { value: v(0) },
            ),
            blk(
                vec![],
                vec![],
                Terminator::Branch {
                    cond: v(0),
                    then_to: BlockId(0),
                    else_to: BlockId(0),
                },
            ),
        ],
        vec![Type::Top],
    );
    assert!(verify::verify_function(&f, &[]).is_err());
}

#[test]
fn walk_honors_write_in_unsealed_predecessor() {
    // E -> B -> C with X written in B. A read of X from sealed C walks into
    // unsealed B and must see the write, not a fresh phi that clobbers it.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let blk_b = b.new_block();
    let blk_c = b.new_block();
    b.set_term(b.entry(), Terminator::Jump { to: blk_b })
        .unwrap();

    b.switch_to(blk_b).unwrap();
    let v1 = konst(&mut b, 7);
    b.write_variable(X, blk_b, v1).unwrap();
    b.set_term(blk_b, Terminator::Jump { to: blk_c }).unwrap();

    b.switch_to(blk_c).unwrap();
    b.seal_block(blk_c).unwrap();
    assert_eq!(b.read_variable(X, blk_c).unwrap(), v1);
    assert_eq!(b.read_variable(X, blk_b).unwrap(), v1);
    b.set_term(blk_c, Terminator::Return { value: v1 }).unwrap();

    b.seal_block(blk_b).unwrap();
    b.seal_block(b.entry()).unwrap();
    let f = b.finish().unwrap();
    assert!(f.blocks.iter().all(|blk| blk.phis.is_empty()));
    assert_eq!(
        f.blocks[blk_c.0 as usize].term,
        Terminator::Return { value: v1 }
    );
    assert!(verify::verify_function(&f, &[]).is_ok());
}

#[test]
fn undefined_read_in_unsealed_entry_is_undefined_at_seal() {
    // The entry has no predecessors and is always reachable, so a phi left
    // empty there by an early read can never gain a definition.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let early = b.read_variable(U, b.entry()).unwrap();
    let into = b.fresh(Type::Bottom);
    b.push(Instr::Copy { src: early, into }).unwrap();
    b.set_term(b.entry(), Terminator::Return { value: into })
        .unwrap();
    match b.seal_block(b.entry()) {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn undefined_read_in_unsealed_entry_cannot_be_finished() {
    // Sealing reports the read but leaves the entry sealed with the empty
    // phi in place, and a retried seal returns Ok; finish must still refuse
    // to hand out a function the verifier would accept.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let early = b.read_variable(U, b.entry()).unwrap();
    let into = b.fresh(Type::Bottom);
    b.push(Instr::Copy { src: early, into }).unwrap();
    b.set_term(b.entry(), Terminator::Return { value: into })
        .unwrap();
    match b.seal_block(b.entry()) {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
    b.seal_block(b.entry()).unwrap();
    match b.finish() {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn self_loop_entry_read_cannot_be_finished() {
    // The entry's only predecessor is itself, so the incomplete phi fills
    // with itself alone; sealing reports the self-cycle but leaves the phi
    // with its self-referential incoming, and a retried seal returns Ok.
    // The entry runs before any edge into it, so finish must reject the
    // phi even though its incoming list is not empty.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let early = b.read_variable(U, b.entry()).unwrap();
    let into = b.fresh(Type::Bottom);
    b.push(Instr::Copy { src: early, into }).unwrap();
    b.set_term(b.entry(), Terminator::Jump { to: b.entry() })
        .unwrap();
    match b.seal_block(b.entry()) {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
    b.seal_block(b.entry()).unwrap();
    match b.finish() {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, U);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn back_edge_entry_leftover_phi_cannot_be_finished() {
    // X is written only in a body that jumps back to the entry; sealing
    // reports the non-dominating replacement but leaves the filled phi,
    // and a retried seal returns Ok. The body runs after the entry, so the
    // read still has no definition on the first pass and finish rejects
    // the phi.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    b.read_variable(X, b.entry()).unwrap();
    let c0 = cond(&mut b, p);
    let body = b.new_block();
    let exit = b.new_block();
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: body,
            else_to: exit,
        },
    )
    .unwrap();

    b.switch_to(body).unwrap();
    let v_body = konst(&mut b, 0);
    b.write_variable(X, body, v_body).unwrap();
    b.set_term(body, Terminator::Jump { to: b.entry() })
        .unwrap();

    b.switch_to(exit).unwrap();
    b.set_term(exit, Terminator::Return { value: p }).unwrap();

    b.seal_block(body).unwrap();
    b.seal_block(exit).unwrap();
    match b.seal_block(b.entry()) {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, X);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
    b.seal_block(b.entry()).unwrap();
    match b.finish() {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, X);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn nontrivial_entry_back_edge_phi_cannot_be_finished() {
    // Two bodies write different values and jump back to the entry: the
    // phi is non-trivial, so sealing succeeds and reports nothing, yet the
    // read in the entry still had no definition on the first pass. Finish
    // rejects the phi outright.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    b.read_variable(X, b.entry()).unwrap();
    let c0 = cond(&mut b, p);
    let b1 = b.new_block();
    let b2 = b.new_block();
    b.set_term(
        b.entry(),
        Terminator::Branch {
            cond: c0,
            then_to: b1,
            else_to: b2,
        },
    )
    .unwrap();

    b.switch_to(b1).unwrap();
    let v1 = konst(&mut b, 0);
    b.write_variable(X, b1, v1).unwrap();
    b.set_term(b1, Terminator::Jump { to: b.entry() }).unwrap();

    b.switch_to(b2).unwrap();
    let v2 = konst(&mut b, 1);
    b.write_variable(X, b2, v2).unwrap();
    b.set_term(b2, Terminator::Jump { to: b.entry() }).unwrap();

    b.seal_all().unwrap();
    match b.finish() {
        Err(BuildError::UndefinedVariable { var, block }) => {
            assert_eq!(var, X);
            assert_eq!(block, BlockId(0));
        }
        other => panic!("expected undefined variable, got {other:?}"),
    }
}

#[test]
fn dead_block_read_keeps_empty_phi_at_finish() {
    // A block with no predecessors that is not the entry seals unreachable;
    // its empty phi is consistent with the empty predecessor list and still
    // finishes and verifies.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    b.set_term(b.entry(), Terminator::Return { value: v(0) })
        .unwrap();
    b.seal_block(b.entry()).unwrap();

    let dead = b.new_block();
    b.switch_to(dead).unwrap();
    let u = b.read_variable(U, dead).unwrap();
    let into = b.fresh(Type::Bottom);
    b.push(Instr::Copy { src: u, into }).unwrap();
    b.set_term(dead, Terminator::Return { value: into })
        .unwrap();
    b.seal_block(dead).unwrap();

    let f = b.finish().unwrap();
    assert_eq!(
        f.blocks[dead.0 as usize].phis,
        vec![Phi {
            into: f.blocks[dead.0 as usize].phis[0].into,
            incomings: Vec::new().into(),
        }]
    );
    assert!(verify::verify_function(&f, &[]).is_ok());
}

#[test]
fn late_edge_into_sealed_block_is_rejected() {
    // Sealing fixes a block's predecessor set. B2 is sealed with B1 as its
    // only predecessor, then B3 tries to add itself via a handler edge and a
    // terminator edge; both must fail without changing the graph.
    let mut b = FunctionBuilder::new(Sym(3), one_param(), vec![Type::Top]).unwrap();
    let p = v(0);
    let b1 = b.new_block();
    let b2 = b.new_block();
    let b3 = b.new_block();
    let v1 = konst(&mut b, 1);
    b.write_variable(X, b.entry(), v1).unwrap();
    b.set_term(b.entry(), Terminator::Jump { to: b1 }).unwrap();
    b.seal_block(b.entry()).unwrap();

    b.switch_to(b1).unwrap();
    b.set_term(b1, Terminator::Jump { to: b2 }).unwrap();
    b.seal_block(b1).unwrap();

    b.switch_to(b2).unwrap();
    b.set_term(b2, Terminator::Return { value: p }).unwrap();
    b.seal_block(b2).unwrap();
    assert_eq!(b.read_variable(X, b2).unwrap(), v1);

    b.switch_to(b3).unwrap();
    let v2 = konst(&mut b, 2);
    b.write_variable(X, b3, v2).unwrap();
    match b.set_handler(b3, Some(b2)) {
        Err(BuildError::EdgeIntoSealed { from, to }) => {
            assert_eq!(from, b3);
            assert_eq!(to, b2);
        }
        other => panic!("expected edge into sealed block, got {other:?}"),
    }
    match b.set_term(b3, Terminator::Jump { to: b2 }) {
        Err(BuildError::EdgeIntoSealed { from, to }) => {
            assert_eq!(from, b3);
            assert_eq!(to, b2);
        }
        other => panic!("expected edge into sealed block, got {other:?}"),
    }

    // The rejected calls left the builder usable: B3 terminates elsewhere
    // and the sealed read of X still sees v1.
    b.set_term(b3, Terminator::Return { value: v2 }).unwrap();
    b.seal_block(b3).unwrap();
    assert_eq!(b.read_variable(X, b2).unwrap(), v1);
    let f = b.finish().unwrap();
    assert!(f.blocks[b2.0 as usize].phis.is_empty());
    assert!(verify::verify_function(&f, &[]).is_ok());
}

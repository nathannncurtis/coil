//! Hand-built IR functions: printing snapshots, serialisation roundtrips and
//! header rejection (spec section 5).

use coil_ir::ir::BarrierKind;
use coil_ir::ir::Module;
use coil_ir::ir::{
    BinOp, Block, Const, ConstPool, FnFlags, Function, Instr, LocalKind, LocalMeta, Params, Scalar,
    Terminator,
};
use coil_ir::print::print_module;
use coil_ir::serialize::{self, Error};
use coil_ir::types::{
    BlockId, ClassId, ConstIdx, FnId, InstrId, ModId, ShapeId, SigId, Sym, Type, TypeRef, Value,
};
use std::path::PathBuf;

fn v(n: u32) -> Value {
    Value(n)
}

fn module() -> Module {
    let mut pool = ConstPool::default();
    let s_example = pool.intern("example");
    let s_add = pool.intern("add");
    let s_a = pool.intern("a");
    let s_b = pool.intern("b");
    let s_safe_div = pool.intern("safe_div");
    let s_zde = pool.intern("ZeroDivisionError");
    let s_counter = pool.intern("counter");
    let s_n = pool.intern("n");
    let s_err = pool.intern("err");
    let s_mod = pool.intern("<module>");
    let c_none = pool.const_idx(Const::None);
    let c_zero = pool.const_idx(Const::Int(0));
    let c_err = pool.const_idx(Const::Str(s_err));

    // <module>: return None
    let body = Function {
        qualname: s_mod,
        params: Params::default(),
        blocks: vec![Block {
            phis: vec![],
            instrs: vec![Instr::Const {
                idx: c_none,
                into: v(0),
            }],
            term: Terminator::Return { value: v(0) },
            handler: None,
        }],
        entry: BlockId(0),
        locals: vec![],
        flags: FnFlags(0),
        types: vec![Type::None],
        facts: Vec::new(),
        lines: vec![(InstrId(0), 1, 1)],
    };

    // add(a, b): a + b with the overflow branch boxing to a BigInt add.
    let add = Function {
        qualname: s_add,
        params: Params {
            positional: vec![s_a, s_b],
            vararg: None,
            kwonly: vec![],
            kwarg: None,
        },
        blocks: vec![
            Block {
                phis: vec![],
                instrs: vec![
                    Instr::Unbox {
                        src: v(0),
                        scalar: Scalar::Int64,
                        into: v(2),
                    },
                    Instr::Unbox {
                        src: v(1),
                        scalar: Scalar::Int64,
                        into: v(3),
                    },
                    Instr::IAddOvf {
                        lhs: v(2),
                        rhs: v(3),
                        into: v(4),
                        ovf: v(5),
                    },
                ],
                term: Terminator::Branch {
                    cond: v(5),
                    then_to: BlockId(2),
                    else_to: BlockId(1),
                },
                handler: None,
            },
            Block {
                phis: vec![],
                instrs: vec![Instr::Box {
                    src: v(4),
                    scalar: Scalar::Int64,
                    into: v(6),
                }],
                term: Terminator::Return { value: v(6) },
                handler: None,
            },
            Block {
                phis: vec![],
                instrs: vec![
                    Instr::Box {
                        src: v(2),
                        scalar: Scalar::Int64,
                        into: v(7),
                    },
                    Instr::Box {
                        src: v(3),
                        scalar: Scalar::Int64,
                        into: v(8),
                    },
                    Instr::BinOp {
                        op: BinOp::Add,
                        lhs: v(7),
                        rhs: v(8),
                        into: v(9),
                    },
                ],
                term: Terminator::Return { value: v(9) },
                handler: None,
            },
        ],
        entry: BlockId(0),
        locals: vec![],
        flags: FnFlags(0),
        types: vec![
            Type::Top,
            Type::Top,
            Type::Int64,
            Type::Int64,
            Type::Int64,
            Type::Bool,
            Type::BigInt,
            Type::BigInt,
            Type::BigInt,
            Type::BigInt,
        ],
        facts: Vec::new(),
        lines: vec![
            (InstrId(0), 1, 11),
            (InstrId(1), 1, 14),
            (InstrId(2), 1, 8),
            (InstrId(3), 1, 8),
            (InstrId(4), 1, 8),
            (InstrId(5), 1, 8),
            (InstrId(6), 1, 8),
        ],
    };

    // safe_div(a, b): try a / b except ZeroDivisionError: 'err'
    let safe_div = Function {
        qualname: s_safe_div,
        params: Params {
            positional: vec![s_a, s_b],
            vararg: None,
            kwonly: vec![],
            kwarg: None,
        },
        blocks: vec![
            Block {
                phis: vec![],
                instrs: vec![
                    Instr::LoadGlobal {
                        name: s_zde,
                        into: v(2),
                    },
                    Instr::BuildTuple {
                        items: vec![v(2)].into(),
                        into: v(3),
                    },
                    Instr::BinOp {
                        op: BinOp::Div,
                        lhs: v(0),
                        rhs: v(1),
                        into: v(4),
                    },
                ],
                term: Terminator::Return { value: v(4) },
                handler: Some(BlockId(1)),
            },
            Block {
                phis: vec![],
                instrs: vec![Instr::CatchBegin {
                    types: v(3),
                    matched: v(5),
                    exc: v(6),
                }],
                term: Terminator::Branch {
                    cond: v(5),
                    then_to: BlockId(2),
                    else_to: BlockId(3),
                },
                handler: None,
            },
            Block {
                phis: vec![],
                instrs: vec![Instr::Const {
                    idx: c_err,
                    into: v(7),
                }],
                term: Terminator::Return { value: v(7) },
                handler: None,
            },
            Block {
                phis: vec![],
                instrs: vec![],
                term: Terminator::Reraise,
                handler: None,
            },
        ],
        entry: BlockId(0),
        locals: vec![],
        flags: FnFlags(0),
        types: vec![
            Type::Top,
            Type::Top,
            Type::Top,
            Type::Tuple(vec![Type::Top].into()),
            Type::Top,
            Type::Bool,
            Type::Top,
            Type::Str,
        ],
        facts: Vec::new(),
        lines: vec![
            (InstrId(0), 1, 4),
            (InstrId(1), 1, 4),
            (InstrId(2), 2, 15),
            (InstrId(3), 2, 11),
            (InstrId(4), 3, 19),
        ],
    };

    // counter(n): a generator with two yields.
    let counter = Function {
        qualname: s_counter,
        params: Params {
            positional: vec![s_n],
            vararg: None,
            kwonly: vec![],
            kwarg: None,
        },
        blocks: vec![Block {
            phis: vec![],
            instrs: vec![
                Instr::Const {
                    idx: c_zero,
                    into: v(1),
                },
                Instr::Yield {
                    state: 0,
                    value: v(1),
                    into: v(2),
                },
                Instr::BinOp {
                    op: BinOp::Add,
                    lhs: v(0),
                    rhs: v(2),
                    into: v(3),
                },
                Instr::Yield {
                    state: 1,
                    value: v(3),
                    into: v(4),
                },
            ],
            term: Terminator::Return { value: v(4) },
            handler: None,
        }],
        entry: BlockId(0),
        locals: vec![LocalMeta {
            name: s_n,
            kind: LocalKind::Cell,
        }],
        flags: FnFlags::GENERATOR.union(FnFlags::HAS_CELLS),
        types: vec![Type::Top, Type::BigInt, Type::Top, Type::Top, Type::Top],
        facts: Vec::new(),
        lines: vec![
            (InstrId(0), 1, 15),
            (InstrId(1), 1, 15),
            (InstrId(2), 2, 11),
            (InstrId(3), 2, 11),
        ],
    };

    Module {
        name: s_example,
        file: PathBuf::from("example.py"),
        functions: vec![body, add, safe_div, counter],
        classes: vec![],
        consts: pool,
        body: FnId(0),
    }
}

#[test]
fn print_snapshot() {
    let m = module();
    let text = print_module(&m);
    let expected = "\
coil-ir module 'example' file 'example.py'
syms:
  0: 'example'
  1: 'add'
  2: 'a'
  3: 'b'
  4: 'safe_div'
  5: 'ZeroDivisionError'
  6: 'counter'
  7: 'n'
  8: 'err'
  9: '<module>'
consts:
  0: None
  1: Int(0)
  2: Str('err')
functions:
fn 0 '<module>' params ()
  block 0 (entry)
    v0:None = const #0 None @1:1
    -> return v0
fn 1 'add' params (a: v0:Top, b: v1:Top)
  block 0 (entry)
    v2:Int64 = unbox int64, v0 @1:11
    v3:Int64 = unbox int64, v1 @1:14
    v4:Int64, v5:Bool = iadd_ovf v2, v3 @1:8
    -> branch v5, block 2, block 1
  block 1
    v6:BigInt = box int64, v4 @1:8
    -> return v6
  block 2
    v7:BigInt = box int64, v2 @1:8
    v8:BigInt = box int64, v3 @1:8
    v9:BigInt = binop add, v7, v8 @1:8
    -> return v9
fn 2 'safe_div' params (a: v0:Top, b: v1:Top)
  block 0 (entry) handler -> block 1
    v2:Top = loadglobal 'ZeroDivisionError' @1:4
    v3:Tuple[Top] = buildtuple [v2] @1:4
    v4:Top = binop truediv, v0, v1 @2:15
    -> return v4
  block 1
    v5:Bool, v6:Top = catchbegin v3 @2:11
    -> branch v5, block 2, block 3
  block 2
    v7:Str = const #2 Str('err') @3:19
    -> return v7
  block 3
    -> reraise
fn 3 'counter' params (n: v0:Top) flags [generator, has-cells] locals [n: cell]
  block 0 (entry)
    v1:BigInt = const #1 Int(0) @1:15
    v2:Top = yield state 0, v1 @1:15
    v3:Top = binop add, v0, v2 @2:11
    v4:Top = yield state 1, v3 @2:11
    -> return v4
body fn 0
";
    assert_eq!(text, expected);
}

#[test]
fn serialize_roundtrip() {
    let m = module();
    let bytes = serialize::serialize(&m);
    let back = serialize::deserialize(&bytes).expect("deserialize");
    assert_eq!(back, m);
    let printed = print_module(&back);
    assert_eq!(printed, print_module(&m));
}

#[test]
fn header_rejection() {
    let m = module();
    let bytes = serialize::serialize(&m);

    let mut wrong_hash = bytes.clone();
    wrong_hash[6] ^= 0x01;
    match serialize::deserialize(&wrong_hash) {
        Err(Error::SchemaHashMismatch { found, expected }) => {
            assert_ne!(found, expected);
            assert_eq!(expected, coil_ir::IR_SCHEMA_HASH);
            assert!(
                Error::SchemaHashMismatch { found, expected }
                    .to_string()
                    .contains("schema hash mismatch")
            );
        }
        other => panic!("expected schema hash mismatch, got {other:?}"),
    }

    let mut wrong_version = bytes.clone();
    wrong_version[4] ^= 0x01;
    assert!(matches!(
        serialize::deserialize(&wrong_version),
        Err(Error::BadVersion { .. })
    ));

    let mut wrong_magic = bytes.clone();
    wrong_magic[0] ^= 0xff;
    assert!(matches!(
        serialize::deserialize(&wrong_magic),
        Err(Error::BadMagic { .. })
    ));

    assert!(matches!(
        serialize::deserialize(&bytes[..bytes.len() - 1]),
        Err(Error::Truncated)
    ));
}

fn sink_instrs() -> Vec<Instr> {
    use coil_ir::ir::{BarrierKind, CmpOp, FCmpOp, ICmpOp, Scalar, UnaryOp};
    use coil_ir::types::{ClassId, ConstIdx, ShapeId, Sym, TypeRef};
    let s = Sym(0);
    let c = ConstIdx(0);
    let f = FnId(0);
    let cls = ClassId(0);
    let shape = ShapeId(0);
    let vv = |n: u32| Value(n);
    vec![
        Instr::Const {
            idx: c,
            into: vv(1),
        },
        Instr::Copy {
            src: vv(0),
            into: vv(1),
        },
        Instr::IAddOvf {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
            ovf: vv(3),
        },
        Instr::ISubOvf {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
            ovf: vv(3),
        },
        Instr::IMulOvf {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
            ovf: vv(3),
        },
        Instr::IDivOvf {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
            ovf: vv(3),
        },
        Instr::INegOvf {
            src: vv(0),
            into: vv(1),
            ovf: vv(2),
        },
        Instr::ShlOvf {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
            ovf: vv(3),
        },
        Instr::IAdd {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::ISub {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::IMul {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::IDiv {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::IMod {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::INeg {
            src: vv(0),
            into: vv(1),
        },
        Instr::Shl {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::Shr {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::BitAnd {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::BitOr {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::BitXor {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::BitNot {
            src: vv(0),
            into: vv(1),
        },
        Instr::ICmp {
            op: ICmpOp::Le,
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::FAdd {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::FSub {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::FMul {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::FDiv {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::FMod {
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::FNeg {
            src: vv(0),
            into: vv(1),
        },
        Instr::FCmp {
            op: FCmpOp::Ge,
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::Box {
            src: vv(0),
            scalar: Scalar::Float64,
            into: vv(1),
        },
        Instr::Unbox {
            src: vv(0),
            scalar: Scalar::Bool,
            into: vv(1),
        },
        Instr::BinOp {
            op: BinOp::MatMul,
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::UnaryOp {
            op: UnaryOp::Invert,
            operand: vv(0),
            into: vv(1),
        },
        Instr::Compare {
            op: CmpOp::NotIn,
            lhs: vv(0),
            rhs: vv(1),
            into: vv(2),
        },
        Instr::GetAttr {
            obj: vv(0),
            name: s,
            into: vv(1),
        },
        Instr::SetAttr {
            obj: vv(0),
            name: s,
            value: vv(1),
        },
        Instr::DelAttr {
            obj: vv(0),
            name: s,
        },
        Instr::GetItem {
            obj: vv(0),
            key: vv(1),
            into: vv(2),
        },
        Instr::SetItem {
            obj: vv(0),
            key: vv(1),
            value: vv(2),
        },
        Instr::DelItem {
            obj: vv(0),
            key: vv(1),
        },
        Instr::GetIter {
            obj: vv(0),
            into: vv(1),
        },
        Instr::Next {
            iter: vv(0),
            into: vv(1),
        },
        Instr::Call {
            callee: vv(0),
            args: vec![vv(1)].into(),
            kwnames: vec![(s, vv(2))].into(),
            into: vv(3),
        },
        Instr::CallEx {
            callee: vv(0),
            args: vv(1),
            kwargs: vv(2),
            into: vv(3),
        },
        Instr::CallMethod {
            name: s,
            recv: vv(0),
            args: vec![].into(),
            kwnames: vec![].into(),
            into: vv(1),
        },
        Instr::Truth {
            value: vv(0),
            into: vv(1),
        },
        Instr::Len {
            value: vv(0),
            into: vv(1),
        },
        Instr::Contains {
            haystack: vv(0),
            needle: vv(1),
            into: vv(2),
        },
        Instr::Format {
            value: vv(0),
            spec: vv(1),
            into: vv(2),
        },
        Instr::BuildList {
            items: vec![vv(0)].into(),
            into: vv(1),
        },
        Instr::BuildTuple {
            items: vec![vv(0)].into(),
            into: vv(1),
        },
        Instr::BuildDict {
            items: vec![(vv(0), vv(1))].into(),
            into: vv(2),
        },
        Instr::BuildSet {
            items: vec![vv(0)].into(),
            into: vv(1),
        },
        Instr::BuildStr {
            parts: vec![vv(0)].into(),
            into: vv(1),
        },
        Instr::BuildSlice {
            start: Some(vv(0)),
            stop: vv(1),
            step: None,
            into: vv(2),
        },
        Instr::Unpack {
            src: vv(0),
            n: 3,
            star: Some(1),
            into: vec![vv(1), vv(2), vv(3)].into(),
        },
        Instr::Import {
            name: s,
            fromlist: vv(0),
            level: vv(1),
            into: vv(2),
        },
        Instr::ImportStar { module: vv(0) },
        Instr::MakeFunction {
            func: f,
            cells: vec![vv(0)].into(),
            defaults: vec![vv(1)].into(),
            into: vv(2),
        },
        Instr::MakeClass {
            desc: cls,
            bases: vec![vv(0)].into(),
            cells: vec![].into(),
            kwds: vv(2),
            into: vv(1),
        },
        Instr::MakeCell {
            initial: Some(vv(0)),
            into: vv(1),
        },
        Instr::LoadClosure {
            index: 3,
            into: vv(1),
        },
        Instr::LoadCell {
            cell: vv(0),
            into: vv(1),
        },
        Instr::StoreCell {
            cell: vv(0),
            value: vv(1),
        },
        Instr::LoadGlobal {
            name: s,
            into: vv(1),
        },
        Instr::StoreGlobal {
            name: s,
            value: vv(0),
        },
        Instr::LoadName {
            name: s,
            into: vv(1),
        },
        Instr::StoreName {
            name: s,
            value: vv(0),
        },
        Instr::CallDirect {
            func: f,
            args: vec![vv(0)].into(),
            into: vv(1),
        },
        Instr::LoadField {
            obj: vv(0),
            shape,
            slot: 7,
            into: vv(1),
        },
        Instr::StoreField {
            obj: vv(0),
            shape,
            slot: 7,
            value: vv(1),
            barrier: BarrierKind::Dynamic,
        },
        Instr::AllocInst {
            cls,
            shape,
            into: vv(1),
        },
        Instr::AllocTuple {
            items: vec![vv(0)].into(),
            into: vv(1),
        },
        Instr::TypeGuard {
            value: vv(0),
            ty: Type::Ext(TypeRef(s)),
            ok: vv(1),
            fail: vv(2),
        },
        Instr::ShapeGuard {
            obj: vv(0),
            shape,
            ok: vv(1),
            fail: vv(2),
        },
        Instr::BoundsCheck {
            index: vv(0),
            len: vv(1),
            into: vv(2),
        },
        Instr::ListGetUnchecked {
            list: vv(0),
            index: vv(1),
            into: vv(2),
        },
        Instr::TupleGetUnchecked {
            tuple: vv(0),
            index: 2,
            into: vv(1),
        },
        Instr::StackAlloc {
            cls,
            shape,
            into: vv(1),
        },
        Instr::Yield {
            state: 4,
            value: vv(0),
            into: vv(1),
        },
        Instr::YieldFrom {
            state: 4,
            iter: vv(0),
            send: vv(1),
            into: vv(2),
        },
        Instr::Await {
            state: 4,
            value: vv(0),
            into: vv(1),
        },
        Instr::Safepoint,
        Instr::CatchBegin {
            types: vv(0),
            matched: vv(1),
            exc: vv(2),
        },
        Instr::CatchEnd,
        Instr::PushExcInfo { into: vv(1) },
        Instr::PopExcInfo,
        Instr::FinallyEnter { into: vv(1) },
        Instr::FinallyExit { reason: vv(0) },
        Instr::CatchStarBegin {
            src: vv(0),
            types: vv(1),
            matched: vv(2),
            exc: vv(3),
            rest: vv(4),
        },
        Instr::PrepReraiseStar {
            orig: vv(0),
            excs: vv(1),
            into: vv(2),
        },
    ]
}

#[test]
fn every_instruction_roundtrips() {
    let instrs = sink_instrs();
    let terminators = vec![
        Terminator::Jump { to: BlockId(0) },
        Terminator::Branch {
            cond: v(0),
            then_to: BlockId(1),
            else_to: BlockId(0),
        },
        Terminator::Switch {
            on: v(0),
            cases: vec![(i64::MIN, BlockId(1)), (42, BlockId(0))].into(),
            default: BlockId(2),
        },
        Terminator::Return { value: v(0) },
        Terminator::Raise {
            exc: Some(v(0)),
            cause: None,
        },
        Terminator::Raise {
            exc: None,
            cause: Some(v(0)),
        },
        Terminator::Reraise,
        Terminator::Unreachable,
    ];
    let phis = vec![coil_ir::ir::Phi {
        into: v(9),
        incomings: vec![(v(1), BlockId(0)), (v(2), BlockId(1))].into(),
    }];
    for term in terminators {
        let m = Module {
            name: Sym(0),
            file: PathBuf::from("sink.py"),
            functions: vec![Function {
                qualname: Sym(0),
                params: Params::default(),
                blocks: vec![Block {
                    phis: phis.clone(),
                    instrs: instrs.clone(),
                    term: term.clone(),
                    handler: Some(BlockId(0)),
                }],
                entry: BlockId(0),
                locals: vec![LocalMeta {
                    name: Sym(0),
                    kind: LocalKind::Free,
                }],
                flags: FnFlags::ASYNC_GEN,
                types: vec![
                    Type::Union(vec![Type::Top, Type::Bottom].into()),
                    Type::Func(SigId(3)),
                    Type::Module(ModId(4)),
                    Type::Class(ClassId(5)),
                    Type::Inst {
                        cls: ClassId(5),
                        exact: true,
                    },
                    Type::Bytes,
                    Type::Set,
                    Type::Dict,
                    Type::List,
                    Type::Str,
                    Type::Tuple(vec![].into()),
                ],
                facts: Vec::new(),
                lines: vec![(InstrId(0), 1, 1), (InstrId(82), 9, 99)],
            }],
            classes: vec![coil_ir::ir::ClassDesc {
                name: Sym(0),
                bases: vec![Type::Ext(TypeRef(Sym(1)))].into(),
                body: FnId(0),
                methods: vec![(Sym(2), FnId(0))].into(),
                has_ext_base: true,
            }],
            consts: ConstPool {
                strings: vec!["sink".into(), "ndarray".into(), "run".into()],
                consts: vec![
                    Const::Bool(true),
                    Const::BigInt("99999999999999999999999999".into()),
                    Const::Float(1.5),
                    Const::Bytes(vec![1, 2, 255].into()),
                    Const::Tuple(vec![ConstIdx(0)].into()),
                    Const::Ellipsis,
                ],
            },
            body: FnId(0),
        };
        let bytes = serialize::serialize(&m);
        assert_eq!(serialize::deserialize(&bytes).expect("deserialize"), m);
    }
}

#[test]
fn may_raise_marks() {
    let instrs = sink_instrs();
    // Every variant appears once; the marks must survive a roundtrip too.
    assert_eq!(instrs.len(), 90);
    assert!(instrs.iter().filter(|i| i.may_raise()).count() > 0);
    assert!(
        Instr::BinOp {
            op: BinOp::Add,
            lhs: v(0),
            rhs: v(1),
            into: v(2)
        }
        .may_raise()
    );
    assert!(Instr::Safepoint.may_raise());
    assert!(Instr::FinallyExit { reason: v(0) }.may_raise());
    assert!(
        Instr::CatchStarBegin {
            src: v(0),
            types: v(1),
            matched: v(2),
            exc: v(3),
            rest: v(4)
        }
        .may_raise()
    );
    assert!(
        Instr::PrepReraiseStar {
            orig: v(0),
            excs: v(1),
            into: v(2)
        }
        .may_raise()
    );
    assert!(
        !Instr::IAddOvf {
            lhs: v(0),
            rhs: v(1),
            into: v(2),
            ovf: v(3)
        }
        .may_raise()
    );
    assert!(
        !Instr::INegOvf {
            src: v(0),
            into: v(1),
            ovf: v(2)
        }
        .may_raise()
    );
    assert!(
        Instr::IDivOvf {
            lhs: v(0),
            rhs: v(1),
            into: v(2),
            ovf: v(3)
        }
        .may_raise()
    );
    assert!(
        Instr::ShlOvf {
            lhs: v(0),
            rhs: v(1),
            into: v(2),
            ovf: v(3)
        }
        .may_raise()
    );
    // Python float division and modulo by zero raise ZeroDivisionError.
    assert!(
        Instr::FDiv {
            lhs: v(0),
            rhs: v(1),
            into: v(2)
        }
        .may_raise()
    );
    assert!(
        Instr::FMod {
            lhs: v(0),
            rhs: v(1),
            into: v(2)
        }
        .may_raise()
    );
    assert!(
        !Instr::Copy {
            src: v(0),
            into: v(1)
        }
        .may_raise()
    );
    assert!(
        !Instr::StoreField {
            obj: v(0),
            shape: ShapeId(0),
            slot: 0,
            value: v(1),
            barrier: BarrierKind::Ext
        }
        .may_raise()
    );
    let bytes = serialize::serialize(&module());
    let back = serialize::deserialize(&bytes).unwrap();
    let before: Vec<bool> = module()
        .functions
        .iter()
        .flat_map(|f| {
            f.blocks
                .iter()
                .flat_map(|b| b.instrs.iter().map(|i| i.may_raise()))
        })
        .collect();
    let after: Vec<bool> = back
        .functions
        .iter()
        .flat_map(|f| {
            f.blocks
                .iter()
                .flat_map(|b| b.instrs.iter().map(|i| i.may_raise()))
        })
        .collect();
    assert_eq!(before, after);
}

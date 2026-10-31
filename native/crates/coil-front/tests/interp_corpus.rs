//! IR interpreter acceptance tests (spec section 5): every corpus program
//! runs identically under CPython 3.12 and the interpreter over the test
//! object model, and the corpus together with one hand-built module
//! executes every instruction and terminator the IR defines.

use coil_ir::interp::model::ObjectModel;
use coil_ir::interp::testmodel::TestModel;
use coil_ir::interp::{Flow, Interp, Resume, tag_count, tag_name};
use coil_ir::ir::{
    BarrierKind, Block, ClassDesc, Const, ConstPool, FnFlags, Function, Instr, Module, Params, Phi,
    Scalar, Terminator,
};
use coil_ir::types::{BlockId, ClassId, FnId, InstrId, ShapeId, Sym, Type, Value};
use std::path::{Path, PathBuf};

const CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../tests/native/ir_programs"
);

/// The corpus recurses to CPython's 1000-frame limit and the interpreter
/// keeps native frames for each; run on a worker thread with the stack the
/// dumper uses.
fn big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(f)
        .expect("spawn big-stack worker")
        .join()
        .expect("big-stack worker panicked")
}

/// A CPython 3.12 executable. CI images provide `python` (setup-python
/// 3.12), developer machines `python3.12`; anything else fails loudly
/// rather than skipping the differential.
fn python() -> &'static str {
    for name in ["python3.12", "python"] {
        if let Ok(out) = std::process::Command::new(name).arg("--version").output() {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if text.contains("Python 3.12.") {
                return name;
            }
        }
    }
    panic!(
        "no CPython 3.12 on PATH (tried python3.12, python); the corpus differential requires it"
    )
}

/// The type name of an uncaught exception, from a CPython-format report:
/// the last non-indented line, before the message colon.
fn exc_type(report: &str) -> Option<String> {
    for line in report.lines().rev() {
        if line.is_empty() || line.starts_with(' ') {
            continue;
        }
        if line.starts_with("Traceback") {
            return None;
        }
        return Some(line.split(':').next().unwrap_or("").trim().to_string());
    }
    None
}

fn parse_lower(path: &Path, bytes: &[u8]) -> Module {
    let (module, mut interner) = coil_front::parser::parse_module(bytes, Some(path))
        .unwrap_or_else(|e| panic!("{}: parse: {} {}", path.display(), e.kind.name(), e.message));
    let table = coil_front::symtable::symtable(&module, &mut interner).unwrap_or_else(|e| {
        panic!(
            "{}: symtable: {} {}",
            path.display(),
            e.kind.name(),
            e.message
        )
    });
    let name = path.file_stem().map_or_else(
        || path.display().to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    coil_front::lower::lower_module(
        &name,
        path.to_path_buf(),
        &module,
        bytes,
        &interner,
        &table,
        Default::default(),
    )
    .unwrap_or_else(|e| panic!("{}: lower: {}", path.display(), e))
}

/// Assembles one function: sequential value numbering, per-block
/// instruction lists and line records in global instruction order.
struct FnBuild<'p> {
    pool: &'p mut ConstPool,
    blocks: Vec<Block>,
    instrs: Vec<Instr>,
    lines: Vec<(InstrId, u32, u32)>,
    next: u32,
}

impl<'p> FnBuild<'p> {
    fn new(pool: &'p mut ConstPool) -> Self {
        FnBuild {
            pool,
            blocks: Vec::new(),
            instrs: Vec::new(),
            lines: Vec::new(),
            next: 0,
        }
    }

    fn v(&mut self) -> Value {
        let v = Value(self.next);
        self.next += 1;
        v
    }

    fn push(&mut self, i: Instr) {
        self.lines.push((InstrId(self.instrs.len() as u32), 1, 1));
        self.instrs.push(i);
    }

    /// Closes the current block. Line records keep the running instruction
    /// numbering, so `line_of` stays a global lookup.
    fn flush(&mut self, phis: Vec<Phi>, term: Terminator) {
        self.blocks.push(Block {
            phis,
            instrs: std::mem::take(&mut self.instrs),
            term,
            handler: None,
        });
    }

    fn finish(self, qualname: Sym, flags: FnFlags) -> Function {
        assert!(
            self.instrs.is_empty(),
            "trailing instructions never flushed"
        );
        Function {
            qualname,
            params: Params::default(),
            blocks: self.blocks,
            entry: BlockId(0),
            locals: vec![],
            flags,
            types: vec![Type::Top; self.next as usize],
            lines: self.lines,
        }
    }

    fn sym(&mut self, s: &str) -> Sym {
        self.pool.intern(s)
    }

    /// A boxed constant (the `Const` result itself).
    fn cbox(&mut self, c: Const) -> Value {
        let idx = self.pool.const_idx(c);
        let into = self.v();
        self.push(Instr::Const { idx, into });
        into
    }

    fn cstr(&mut self, s: &str) -> Value {
        let sym = self.sym(s);
        self.cbox(Const::Str(sym))
    }

    /// A constant already unboxed to an `Int64` slot.
    fn cint(&mut self, n: i64) -> Value {
        let c = self.cbox(Const::Int(n));
        let into = self.v();
        self.push(Instr::Unbox {
            src: c,
            scalar: Scalar::Int64,
            into,
        });
        into
    }

    fn cfloat(&mut self, f: f64) -> Value {
        let c = self.cbox(Const::Float(f));
        let into = self.v();
        self.push(Instr::Unbox {
            src: c,
            scalar: Scalar::Float64,
            into,
        });
        into
    }

    fn box_scalar(&mut self, src: Value, scalar: Scalar) -> Value {
        let into = self.v();
        self.push(Instr::Box { src, scalar, into });
        into
    }

    /// `print(args...)` through the global builtin.
    fn print(&mut self, args: &[Value]) {
        let print = self.sym("print");
        let callee = self.v();
        self.push(Instr::LoadGlobal {
            name: print,
            into: callee,
        });
        let into = self.v();
        self.push(Instr::Call {
            callee,
            args: args.to_vec().into_boxed_slice(),
            kwnames: Box::new([]),
            into,
        });
    }
}

fn const_none(b: &mut FnBuild) -> Value {
    let idx = b.pool.const_idx(Const::None);
    let into = b.v();
    b.push(Instr::Const { idx, into });
    into
}

/// A module the front end never produces: the unboxed arithmetic and the
/// optimiser-introduced instructions have no source-level spelling, so
/// they are built directly and executed for coverage and semantics.
///
/// fn 0 `<module>` static-shape instructions ending in a switch;
/// fn 1 `Thing_body` class body;
/// fn 2 `arith` every unboxed integer and float operation;
/// fn 3 `coro` (coroutine) makes fn 4, awaits it, prints the value;
/// fn 4 `g41` (coroutine) returns 41.
fn static_ops_module() -> Module {
    let mut pool = ConstPool::default();

    let mut b = FnBuild::new(&mut pool);
    let s_arith = b.sym("arith");
    let u7 = b.cint(7);
    let u5 = b.cint(5);
    let u2 = b.cint(2);
    let u1 = b.cint(1);
    let u3 = b.cint(3);
    let u17 = b.cint(17);
    let u4 = b.cint(4);
    let u48 = b.cint(48);
    let u12 = b.cint(12);
    let u10 = b.cint(10);
    let (sum, o_add) = (b.v(), b.v());
    b.push(Instr::IAddOvf {
        lhs: u7,
        rhs: u5,
        into: sum,
        ovf: o_add,
    });
    let (diff, o_sub) = (b.v(), b.v());
    b.push(Instr::ISubOvf {
        lhs: u7,
        rhs: u5,
        into: diff,
        ovf: o_sub,
    });
    let (prod, o_mul) = (b.v(), b.v());
    b.push(Instr::IMulOvf {
        lhs: u7,
        rhs: u5,
        into: prod,
        ovf: o_mul,
    });
    let (quo, o_div) = (b.v(), b.v());
    b.push(Instr::IDivOvf {
        lhs: u7,
        rhs: u2,
        into: quo,
        ovf: o_div,
    });
    let (neg, o_neg) = (b.v(), b.v());
    b.push(Instr::INegOvf {
        src: u5,
        into: neg,
        ovf: o_neg,
    });
    let (shl, o_shl) = (b.v(), b.v());
    b.push(Instr::ShlOvf {
        lhs: u1,
        rhs: u3,
        into: shl,
        ovf: o_shl,
    });
    let line1: Vec<Value> = [sum, diff, prod, quo, neg, shl]
        .into_iter()
        .map(|v| b.box_scalar(v, Scalar::Int64))
        .collect();
    let (a1, a2, a3, a4, a5, a6, a7, a8) = (b.v(), b.v(), b.v(), b.v(), b.v(), b.v(), b.v(), b.v());
    b.push(Instr::IAdd {
        lhs: u17,
        rhs: u5,
        into: a1,
    });
    b.push(Instr::ISub {
        lhs: u17,
        rhs: u5,
        into: a2,
    });
    b.push(Instr::IMul {
        lhs: u17,
        rhs: u5,
        into: a3,
    });
    b.push(Instr::IDiv {
        lhs: u17,
        rhs: u5,
        into: a4,
    });
    b.push(Instr::IMod {
        lhs: u17,
        rhs: u5,
        into: a5,
    });
    b.push(Instr::INeg { src: u17, into: a6 });
    b.push(Instr::Shl {
        lhs: u3,
        rhs: u4,
        into: a7,
    });
    b.push(Instr::Shr {
        lhs: u48,
        rhs: u2,
        into: a8,
    });
    let line2: Vec<Value> = [a1, a2, a3, a4, a5, a6, a7, a8]
        .into_iter()
        .map(|v| b.box_scalar(v, Scalar::Int64))
        .collect();
    let (x1, x2, x3, x4) = (b.v(), b.v(), b.v(), b.v());
    b.push(Instr::BitAnd {
        lhs: u12,
        rhs: u10,
        into: x1,
    });
    b.push(Instr::BitOr {
        lhs: u12,
        rhs: u10,
        into: x2,
    });
    b.push(Instr::BitXor {
        lhs: u12,
        rhs: u10,
        into: x3,
    });
    b.push(Instr::BitNot { src: u12, into: x4 });
    let (c_lt, c_ge) = (b.v(), b.v());
    b.push(Instr::ICmp {
        op: coil_ir::ir::ICmpOp::Lt,
        lhs: u3,
        rhs: u5,
        into: c_lt,
    });
    b.push(Instr::ICmp {
        op: coil_ir::ir::ICmpOp::Ge,
        lhs: u3,
        rhs: u5,
        into: c_ge,
    });
    let mut line3: Vec<Value> = [x1, x2, x3, x4]
        .into_iter()
        .map(|v| b.box_scalar(v, Scalar::Int64))
        .collect();
    line3.push(b.box_scalar(c_lt, Scalar::Bool));
    line3.push(b.box_scalar(c_ge, Scalar::Bool));
    let f15 = b.cfloat(1.5);
    let f225 = b.cfloat(2.25);
    let f05 = b.cfloat(0.5);
    let f20 = b.cfloat(2.0);
    let f30 = b.cfloat(3.0);
    let f40 = b.cfloat(4.0);
    let f75 = b.cfloat(7.5);
    let f25 = b.cfloat(2.5);
    let (g1, g2, g3, g4, g5, g6) = (b.v(), b.v(), b.v(), b.v(), b.v(), b.v());
    b.push(Instr::FAdd {
        lhs: f15,
        rhs: f225,
        into: g1,
    });
    b.push(Instr::FSub {
        lhs: g1,
        rhs: f05,
        into: g2,
    });
    b.push(Instr::FMul {
        lhs: f15,
        rhs: f20,
        into: g3,
    });
    b.push(Instr::FDiv {
        lhs: f30,
        rhs: f40,
        into: g4,
    });
    b.push(Instr::FMod {
        lhs: f75,
        rhs: f20,
        into: g5,
    });
    b.push(Instr::FNeg { src: f15, into: g6 });
    let (h_lt, h_le) = (b.v(), b.v());
    b.push(Instr::FCmp {
        op: coil_ir::ir::FCmpOp::Lt,
        lhs: f15,
        rhs: f25,
        into: h_lt,
    });
    b.push(Instr::FCmp {
        op: coil_ir::ir::FCmpOp::Le,
        lhs: f25,
        rhs: f15,
        into: h_le,
    });
    let mut line4: Vec<Value> = [g1, g2, g3, g4, g5, g6]
        .into_iter()
        .map(|v| b.box_scalar(v, Scalar::Float64))
        .collect();
    line4.push(b.box_scalar(h_lt, Scalar::Bool));
    line4.push(b.box_scalar(h_le, Scalar::Bool));
    b.print(&line1);
    b.print(&line2);
    b.print(&line3);
    b.print(&line4);
    let ret_none = const_none(&mut b);
    b.flush(vec![], Terminator::Return { value: ret_none });
    let arith = b.finish(s_arith, FnFlags(0));

    let mut b = FnBuild::new(&mut pool);
    let s_body = b.sym("Thing_body");
    let none = const_none(&mut b);
    b.flush(vec![], Terminator::Return { value: none });
    let thing_body = b.finish(s_body, FnFlags(0));

    let mut b = FnBuild::new(&mut pool);
    let s_g41 = b.sym("g41");
    let v41 = b.cint(41);
    let boxed41 = b.box_scalar(v41, Scalar::Int64);
    b.flush(vec![], Terminator::Return { value: boxed41 });
    let g41 = b.finish(s_g41, FnFlags::COROUTINE);

    let mut b = FnBuild::new(&mut pool);
    let s_coro = b.sym("coro");
    let c_none = const_none(&mut b);
    let fval = b.v();
    b.push(Instr::MakeFunction {
        func: FnId(4),
        cells: Box::new([]),
        defaults: vec![c_none, c_none].into_boxed_slice(),
        into: fval,
    });
    let inner = b.v();
    b.push(Instr::Call {
        callee: fval,
        args: Box::new([]),
        kwnames: Box::new([]),
        into: inner,
    });
    let awaited = b.v();
    b.push(Instr::Await {
        state: 0,
        value: inner,
        into: awaited,
    });
    b.print(&[awaited]);
    let ret_none = const_none(&mut b);
    b.flush(vec![], Terminator::Return { value: ret_none });
    let coro = b.finish(s_coro, FnFlags::COROUTINE);

    let mut b = FnBuild::new(&mut pool);
    let s_mod = b.sym("<module>");
    let s_thing = b.sym("Thing");
    let s_upper = b.sym("upper");
    let c_none = const_none(&mut b);
    let direct = b.v();
    b.push(Instr::CallDirect {
        func: FnId(2),
        args: Box::new([]),
        into: direct,
    });
    let cls = b.v();
    b.push(Instr::MakeClass {
        desc: ClassId(0),
        bases: Box::new([]),
        cells: Box::new([]),
        kwds: c_none,
        into: cls,
    });
    let inst = b.v();
    b.push(Instr::AllocInst {
        cls: ClassId(0),
        shape: ShapeId(0),
        into: inst,
    });
    let stack_inst = b.v();
    b.push(Instr::StackAlloc {
        cls: ClassId(0),
        shape: ShapeId(0),
        into: stack_inst,
    });
    let recv = b.cstr("ab");
    let upper = b.v();
    b.push(Instr::CallMethod {
        name: s_upper,
        recv,
        args: Box::new([]),
        kwnames: Box::new([]),
        into: upper,
    });
    b.push(Instr::StoreField {
        obj: inst,
        shape: ShapeId(0),
        slot: 0,
        value: upper,
        barrier: BarrierKind::None,
    });
    let field = b.v();
    b.push(Instr::LoadField {
        obj: inst,
        shape: ShapeId(0),
        slot: 0,
        into: field,
    });
    let copied = b.v();
    b.push(Instr::Copy {
        src: field,
        into: copied,
    });
    let list = b.v();
    b.push(Instr::BuildList {
        items: vec![copied].into_boxed_slice(),
        into: list,
    });
    let needle = b.cstr("AB");
    let has = b.v();
    b.push(Instr::Contains {
        haystack: list,
        needle,
        into: has,
    });
    let length = b.v();
    b.push(Instr::Len {
        value: list,
        into: length,
    });
    let zero_boxed = {
        let idx = b.pool.const_idx(Const::Int(0));
        let into = b.v();
        b.push(Instr::Const { idx, into });
        into
    };
    let zero = b.v();
    b.push(Instr::Unbox {
        src: zero_boxed,
        scalar: Scalar::Int64,
        into: zero,
    });
    let in_bounds = b.v();
    b.push(Instr::BoundsCheck {
        index: zero,
        len: length,
        into: in_bounds,
    });
    let item = b.v();
    b.push(Instr::ListGetUnchecked {
        list,
        index: zero,
        into: item,
    });
    let tuple = b.v();
    b.push(Instr::BuildTuple {
        items: vec![list, item].into_boxed_slice(),
        into: tuple,
    });
    let tuple_item = b.v();
    b.push(Instr::TupleGetUnchecked {
        tuple,
        index: 1,
        into: tuple_item,
    });
    let atuple = b.v();
    b.push(Instr::AllocTuple {
        items: vec![tuple_item].into_boxed_slice(),
        into: atuple,
    });
    let (tg_ok, tg_fail) = (b.v(), b.v());
    b.push(Instr::TypeGuard {
        value: zero_boxed,
        ty: Type::BigInt,
        ok: tg_ok,
        fail: tg_fail,
    });
    let (sg_ok, sg_fail) = (b.v(), b.v());
    b.push(Instr::ShapeGuard {
        obj: inst,
        shape: ShapeId(0),
        ok: sg_ok,
        fail: sg_fail,
    });
    b.push(Instr::Safepoint);
    let one = b.cint(1);
    let b_in = b.box_scalar(in_bounds, Scalar::Bool);
    let b_tg = b.box_scalar(tg_fail, Scalar::Bool);
    let b_sg = b.box_scalar(sg_fail, Scalar::Bool);
    let switch_val = b.v();
    b.push(Instr::Copy {
        src: one,
        into: switch_val,
    });
    b.flush(
        vec![],
        Terminator::Switch {
            on: switch_val,
            cases: vec![(0, BlockId(1)), (2, BlockId(2))].into_boxed_slice(),
            default: BlockId(3),
        },
    );
    // blocks 1-3: one candidate per switch case; block 4 merges them with
    // a phi and prints the results.
    let case0 = b.cstr("case0");
    b.flush(vec![], Terminator::Jump { to: BlockId(4) });
    let case2 = b.cstr("case2");
    b.flush(vec![], Terminator::Jump { to: BlockId(4) });
    let default = b.cstr("default");
    b.flush(vec![], Terminator::Jump { to: BlockId(4) });
    let merge = b.v();
    b.print(&[merge, tuple_item, atuple, b_in, b_tg, b_sg]);
    let ret_none = const_none(&mut b);
    b.flush(
        vec![Phi {
            into: merge,
            incomings: vec![
                (case0, BlockId(1)),
                (case2, BlockId(2)),
                (default, BlockId(3)),
            ]
            .into_boxed_slice(),
        }],
        Terminator::Return { value: ret_none },
    );
    let mod_fn = b.finish(s_mod, FnFlags(0));
    let _ = s_thing;

    Module {
        name: pool.intern("<static-ops>"),
        file: PathBuf::from("static-ops.ir"),
        functions: vec![mod_fn, thing_body, arith, coro, g41],
        classes: vec![ClassDesc {
            name: pool.intern("Thing"),
            bases: Box::new([]),
            body: FnId(1),
            methods: Box::new([]),
            has_ext_base: false,
        }],
        consts: pool,
        body: FnId(0),
    }
}

#[test]
fn static_ops_module_runs() {
    big_stack(|| {
        let module = static_ops_module();
        let mut model = TestModel::new();
        let globals = model.new_dict();
        let mut vm = Interp::new(&module, globals);
        model.capture_output();
        if let Err(exc) = vm.run_main(&mut model) {
            panic!("static-ops module raised:\n{}", model.format_uncaught(&exc));
        }
        // `coro` (fn 3) holds the Await; drive it like `send(None)` --
        // CallDirect cannot start a suspending function.
        let none = model.none();
        let mut frame = vm
            .new_frame(&mut model, FnId(3), vec![], &[], &[], &none, &none, None)
            .unwrap_or_else(|exc| panic!("coro frame:\n{}", model.format_uncaught(&exc)));
        match vm.resume_frame(&mut model, &mut frame, Resume::Start) {
            Ok(Flow::Done(_)) => {}
            Ok(Flow::Suspended(_)) => panic!("coro suspended unexpectedly"),
            Err(exc) => panic!("coro raised:\n{}", model.format_uncaught(&exc)),
        }
        let out = model.take_output();
        let expected = "\
12 2 35 3 -5 8
22 12 85 3 2 -17 48 12
8 14 6 -13 True False
3.75 3.25 3.0 0.75 1.5 -1.5 True False
default AB ('AB',) True False False
41
";
        assert_eq!(out, expected);
    });
}

#[test]
fn corpus_matches_cpython_and_covers_every_instruction() {
    big_stack(|| {
        let py = python();
        let dir = Path::new(CORPUS);
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read corpus dir {}: {e}", dir.display()))
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "py"))
            .collect();
        files.sort();
        assert!(!files.is_empty(), "no .py programs in {}", dir.display());
        let mut covered = vec![false; tag_count()];
        let mut failures: Vec<String> = Vec::new();
        for path in &files {
            let bytes = std::fs::read(path).unwrap();
            let module = parse_lower(path, &bytes);
            let mut model = TestModel::new();
            let globals = model.new_dict();
            let mut vm = Interp::new(&module, globals);
            model.capture_output();
            let uncaught = vm.run_main(&mut model).err();
            for tag in vm.coverage.covered() {
                covered[tag] = true;
            }
            let coil_out = model.take_output();
            let coil_exc = uncaught.and_then(|e| exc_type(&model.format_uncaught(&e)));
            let py_run = std::process::Command::new(py)
                .arg(path)
                .output()
                .unwrap_or_else(|e| panic!("run {}: {e}", path.display()));
            let py_out = String::from_utf8_lossy(&py_run.stdout).into_owned();
            let py_exc = exc_type(&String::from_utf8_lossy(&py_run.stderr));
            let name = path.file_name().unwrap().to_string_lossy();
            let mut why: Vec<String> = Vec::new();
            if coil_out != py_out {
                let po: Vec<&str> = py_out.lines().collect();
                let co: Vec<&str> = coil_out.lines().collect();
                let i = (0..po.len().max(co.len()))
                    .find(|&i| po.get(i) != co.get(i))
                    .unwrap_or(po.len().max(co.len()));
                why.push(format!(
                    "stdout line {}: python={:?} coil={:?}",
                    i,
                    po.get(i).unwrap_or(&"<missing>"),
                    co.get(i).unwrap_or(&"<missing>")
                ));
            }
            if coil_exc != py_exc {
                why.push(format!(
                    "uncaught exception: python={py_exc:?} coil={coil_exc:?}"
                ));
            }
            if !why.is_empty() {
                failures.push(format!("{name}: {}", why.join("; ")));
            }
        }
        assert!(
            failures.is_empty(),
            "{} of {} corpus programs mismatch:\n{}",
            failures.len(),
            files.len(),
            failures.join("\n")
        );

        // The corpus cannot reach the unboxed arithmetic or the
        // optimiser-introduced instructions; the hand-built module covers
        // those. Together they must execute every tag except Unreachable,
        // which marks control flow no execution can reach.
        let static_module = static_ops_module();
        let mut model = TestModel::new();
        let globals = model.new_dict();
        let mut vm = Interp::new(&static_module, globals);
        model.capture_output();
        let _ = vm.run_main(&mut model);
        let none = model.none();
        let mut frame = vm
            .new_frame(&mut model, FnId(3), vec![], &[], &[], &none, &none, None)
            .unwrap_or_else(|exc| panic!("coro frame:\n{}", model.format_uncaught(&exc)));
        let _ = vm.resume_frame(&mut model, &mut frame, Resume::Start);
        for tag in vm.coverage.covered() {
            covered[tag] = true;
        }
        let missing: Vec<&str> = (0..tag_count())
            .filter(|t| !covered[*t])
            .map(tag_name)
            .collect();
        assert_eq!(
            missing,
            vec!["Unreachable"],
            "instruction tags never executed"
        );
    });
}

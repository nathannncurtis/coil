//! Versioned binary serialisation of IR modules into the build cache:
//! hand-written, little-endian, length-prefixed. The header carries the IR
//! schema hash computed at build time from the source of `ir.rs` and
//! `types.rs`; `deserialize` rejects a different hash.

use std::fmt;
use std::path::PathBuf;

use crate::ir::Module;
use crate::ir::{
    BarrierKind, BinOp, Block, ClassDesc, CmpOp, Const, ConstPool, FCmpOp, FnFlags, Function,
    ICmpOp, Instr, LocalKind, LocalMeta, Params, Phi, Scalar, Terminator, UnaryOp,
};
use crate::types::{
    BlockId, ClassId, ConstIdx, FnId, InstrId, ModId, ShapeId, SigId, Sym, Type, TypeRef, Value,
};

pub const MAGIC: u32 = 0x5249_4C43; // "CLIR"
pub const VERSION: u16 = 1;

#[derive(Clone, PartialEq, Debug)]
pub enum Error {
    BadMagic { found: u32 },
    BadVersion { found: u16 },
    SchemaHashMismatch { found: u64, expected: u64 },
    Truncated,
    BadTag { kind: &'static str, tag: u16 },
    BadBool { found: u8 },
    BadUtf8,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::BadMagic { found } => {
                write!(
                    f,
                    "bad magic 0x{found:08x}, expected 0x{MAGIC:08x} (\"CLIR\")"
                )
            }
            Error::BadVersion { found } => {
                write!(f, "unsupported version {found}, expected {VERSION}")
            }
            Error::SchemaHashMismatch { found, expected } => write!(
                f,
                "IR schema hash mismatch: found 0x{found:016x}, this coil-ir is 0x{expected:016x}; rebuild the cached IR"
            ),
            Error::Truncated => write!(f, "truncated IR image"),
            Error::BadTag { kind, tag } => write!(f, "invalid {kind} tag {tag}"),
            Error::BadBool { found } => write!(f, "invalid bool byte {found}"),
            Error::BadUtf8 => write!(f, "invalid UTF-8 in string"),
        }
    }
}

impl std::error::Error for Error {}

pub fn serialize(m: &Module) -> Vec<u8> {
    let mut w = W { buf: Vec::new() };
    w.u32(MAGIC);
    w.u16(VERSION);
    w.u64(crate::IR_SCHEMA_HASH);
    module(&mut w, m);
    w.buf
}

pub fn deserialize(bytes: &[u8]) -> Result<Module, Error> {
    let mut r = R { b: bytes, p: 0 };
    let found_magic = r.u32()?;
    if found_magic != MAGIC {
        return Err(Error::BadMagic { found: found_magic });
    }
    let found_version = r.u16()?;
    if found_version != VERSION {
        return Err(Error::BadVersion {
            found: found_version,
        });
    }
    let found_hash = r.u64()?;
    if found_hash != crate::IR_SCHEMA_HASH {
        return Err(Error::SchemaHashMismatch {
            found: found_hash,
            expected: crate::IR_SCHEMA_HASH,
        });
    }
    module_r(&mut r)
}

struct W {
    buf: Vec<u8>,
}

impl W {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }
    fn string(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn boolean(&mut self, b: bool) {
        self.u8(u8::from(b));
    }
    fn opt<T>(&mut self, v: &Option<T>, f: impl FnOnce(&mut Self, &T)) {
        match v {
            None => self.u8(0),
            Some(x) => {
                self.u8(1);
                f(self, x);
            }
        }
    }
    fn seq<T>(&mut self, vs: &[T], f: impl Fn(&mut Self, &T)) {
        self.u32(vs.len() as u32);
        for v in vs {
            f(self, v);
        }
    }
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], Error> {
        if self.p + n > self.b.len() {
            return Err(Error::Truncated);
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Error> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn i64(&mut self) -> Result<i64, Error> {
        Ok(self.u64()? as i64)
    }
    fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_bits(self.u64()?))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> Result<String, Error> {
        let b = self.bytes()?;
        String::from_utf8(b).map_err(|_| Error::BadUtf8)
    }
    fn boolean(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            found => Err(Error::BadBool { found }),
        }
    }
    fn opt<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(f(self)?)),
            tag => Err(Error::BadTag {
                kind: "option",
                tag: tag as u16,
            }),
        }
    }
    fn seq<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<Vec<T>, Error> {
        let n = self.u32()?;
        let mut vs = Vec::new();
        for _ in 0..n {
            vs.push(f(self)?);
        }
        Ok(vs)
    }
}

fn module(w: &mut W, m: &Module) {
    w.u32(m.name.0);
    w.string(&m.file.to_string_lossy());
    w.seq(&m.functions, function);
    w.seq(&m.classes, class_desc);
    const_pool(w, &m.consts);
    w.u32(m.body.0);
}

fn module_r(r: &mut R) -> Result<Module, Error> {
    Ok(Module {
        name: Sym(r.u32()?),
        file: PathBuf::from(r.string()?),
        functions: r.seq(function_r)?,
        classes: r.seq(class_desc_r)?,
        consts: const_pool_r(r)?,
        body: FnId(r.u32()?),
    })
}

fn const_pool(w: &mut W, p: &ConstPool) {
    w.seq(&p.strings, |w, s| w.string(s));
    w.seq(&p.consts, konst);
}

fn const_pool_r(r: &mut R) -> Result<ConstPool, Error> {
    Ok(ConstPool {
        strings: r.seq(|r| r.string())?,
        consts: r.seq(konst_r)?,
    })
}

fn konst(w: &mut W, c: &Const) {
    match c {
        Const::None => w.u8(1),
        Const::Bool(b) => {
            w.u8(2);
            w.boolean(*b);
        }
        Const::Int(i) => {
            w.u8(3);
            w.i64(*i);
        }
        Const::BigInt(s) => {
            w.u8(4);
            w.string(s);
        }
        Const::Float(x) => {
            w.u8(5);
            w.f64(*x);
        }
        Const::Str(s) => {
            w.u8(6);
            w.u32(s.0);
        }
        Const::Bytes(b) => {
            w.u8(7);
            w.bytes(b);
        }
        Const::Tuple(items) => {
            w.u8(8);
            w.seq(items, |w, i| w.u32(i.0));
        }
        Const::Ellipsis => w.u8(9),
    }
}

fn konst_r(r: &mut R) -> Result<Const, Error> {
    Ok(match r.u8()? {
        1 => Const::None,
        2 => Const::Bool(r.boolean()?),
        3 => Const::Int(r.i64()?),
        4 => Const::BigInt(r.string()?),
        5 => Const::Float(r.f64()?),
        6 => Const::Str(Sym(r.u32()?)),
        7 => Const::Bytes(r.bytes()?.into()),
        8 => Const::Tuple(r.seq(|r| Ok(ConstIdx(r.u32()?)))?.into()),
        9 => Const::Ellipsis,
        tag => {
            return Err(Error::BadTag {
                kind: "const",
                tag: tag as u16,
            });
        }
    })
}

fn class_desc(w: &mut W, c: &ClassDesc) {
    w.u32(c.name.0);
    w.seq(&c.bases, put_ty);
    w.u32(c.body.0);
    w.seq(&c.methods, |w, (n, f)| {
        w.u32(n.0);
        w.u32(f.0);
    });
    w.boolean(c.has_ext_base);
}

fn class_desc_r(r: &mut R) -> Result<ClassDesc, Error> {
    Ok(ClassDesc {
        name: Sym(r.u32()?),
        bases: r.seq(ty_r)?.into(),
        body: FnId(r.u32()?),
        methods: r.seq(|r| Ok((Sym(r.u32()?), FnId(r.u32()?))))?.into(),
        has_ext_base: r.boolean()?,
    })
}

fn function(w: &mut W, f: &Function) {
    w.u32(f.qualname.0);
    params(w, &f.params);
    w.seq(&f.blocks, block);
    w.u32(f.entry.0);
    w.seq(&f.locals, |w, l| {
        w.u32(l.name.0);
        local_kind(w, l.kind);
    });
    w.u16(f.flags.0);
    w.seq(&f.types, put_ty);
    w.seq(&f.facts, put_ty);
    w.seq(&f.lines, |w, (i, l, c)| {
        w.u32(i.0);
        w.u32(*l);
        w.u32(*c);
    });
}

fn function_r(r: &mut R) -> Result<Function, Error> {
    Ok(Function {
        qualname: Sym(r.u32()?),
        params: params_r(r)?,
        blocks: r.seq(block_r)?,
        entry: BlockId(r.u32()?),
        locals: r.seq(|r| {
            Ok(LocalMeta {
                name: Sym(r.u32()?),
                kind: local_kind_r(r)?,
            })
        })?,
        flags: FnFlags(r.u16()?),
        types: r.seq(ty_r)?,
        facts: r.seq(ty_r)?,
        lines: r.seq(|r| Ok((InstrId(r.u32()?), r.u32()?, r.u32()?)))?,
    })
}

fn params(w: &mut W, p: &Params) {
    w.seq(&p.positional, |w, s| w.u32(s.0));
    w.opt(&p.vararg, |w, s| w.u32(s.0));
    w.seq(&p.kwonly, |w, s| w.u32(s.0));
    w.opt(&p.kwarg, |w, s| w.u32(s.0));
}

fn params_r(r: &mut R) -> Result<Params, Error> {
    Ok(Params {
        positional: r.seq(|r| Ok(Sym(r.u32()?)))?,
        vararg: r.opt(|r| Ok(Sym(r.u32()?)))?,
        kwonly: r.seq(|r| Ok(Sym(r.u32()?)))?,
        kwarg: r.opt(|r| Ok(Sym(r.u32()?)))?,
    })
}

fn local_kind(w: &mut W, k: LocalKind) {
    w.u8(match k {
        LocalKind::Local => 1,
        LocalKind::Cell => 2,
        LocalKind::Free => 3,
    });
}

fn local_kind_r(r: &mut R) -> Result<LocalKind, Error> {
    Ok(match r.u8()? {
        1 => LocalKind::Local,
        2 => LocalKind::Cell,
        3 => LocalKind::Free,
        tag => {
            return Err(Error::BadTag {
                kind: "local kind",
                tag: tag as u16,
            });
        }
    })
}

fn block(w: &mut W, b: &Block) {
    w.seq(&b.phis, |w, p| {
        w.u32(p.into.0);
        w.seq(&p.incomings, |w, (v, from)| {
            w.u32(v.0);
            w.u32(from.0);
        });
    });
    w.seq(&b.instrs, instr);
    terminator(w, &b.term);
    w.opt(&b.handler, |w, h| w.u32(h.0));
}

fn block_r(r: &mut R) -> Result<Block, Error> {
    Ok(Block {
        phis: r.seq(|r| {
            Ok(Phi {
                into: Value(r.u32()?),
                incomings: r.seq(|r| Ok((Value(r.u32()?), BlockId(r.u32()?))))?.into(),
            })
        })?,
        instrs: r.seq(instr_r)?,
        term: terminator_r(r)?,
        handler: r.opt(|r| Ok(BlockId(r.u32()?)))?,
    })
}

fn put_ty(w: &mut W, t: &Type) {
    match t {
        Type::Top => w.u8(1),
        Type::Union(parts) => {
            w.u8(2);
            w.seq(parts, put_ty);
        }
        Type::Inst { cls, exact } => {
            w.u8(3);
            w.u32(cls.0);
            w.boolean(*exact);
        }
        Type::Int64 => w.u8(4),
        Type::Float64 => w.u8(5),
        Type::Bool => w.u8(6),
        Type::None => w.u8(7),
        Type::BigInt => w.u8(8),
        Type::Str => w.u8(9),
        Type::Bytes => w.u8(10),
        Type::Tuple(parts) => {
            w.u8(11);
            w.seq(parts, put_ty);
        }
        Type::List => w.u8(12),
        Type::Dict => w.u8(13),
        Type::Set => w.u8(14),
        Type::Func(sig) => {
            w.u8(15);
            w.u32(sig.0);
        }
        Type::Module(m) => {
            w.u8(16);
            w.u32(m.0);
        }
        Type::Class(c) => {
            w.u8(17);
            w.u32(c.0);
        }
        Type::Ext(t) => {
            w.u8(18);
            w.u32(t.0.0);
        }
        Type::Bottom => w.u8(19),
    }
}

fn ty_r(r: &mut R) -> Result<Type, Error> {
    Ok(match r.u8()? {
        1 => Type::Top,
        2 => Type::Union(r.seq(ty_r)?.into()),
        3 => Type::Inst {
            cls: ClassId(r.u32()?),
            exact: r.boolean()?,
        },
        4 => Type::Int64,
        5 => Type::Float64,
        6 => Type::Bool,
        7 => Type::None,
        8 => Type::BigInt,
        9 => Type::Str,
        10 => Type::Bytes,
        11 => Type::Tuple(r.seq(ty_r)?.into()),
        12 => Type::List,
        13 => Type::Dict,
        14 => Type::Set,
        15 => Type::Func(SigId(r.u32()?)),
        16 => Type::Module(ModId(r.u32()?)),
        17 => Type::Class(ClassId(r.u32()?)),
        18 => Type::Ext(TypeRef(Sym(r.u32()?))),
        19 => Type::Bottom,
        tag => {
            return Err(Error::BadTag {
                kind: "type",
                tag: tag as u16,
            });
        }
    })
}

fn bin_op(w: &mut W, op: BinOp) {
    w.u8(match op {
        BinOp::Add => 1,
        BinOp::Sub => 2,
        BinOp::Mul => 3,
        BinOp::Div => 4,
        BinOp::FloorDiv => 5,
        BinOp::Mod => 6,
        BinOp::Pow => 7,
        BinOp::LShift => 8,
        BinOp::RShift => 9,
        BinOp::BitAnd => 10,
        BinOp::BitOr => 11,
        BinOp::BitXor => 12,
        BinOp::MatMul => 13,
    });
}

fn bin_op_r(r: &mut R) -> Result<BinOp, Error> {
    Ok(match r.u8()? {
        1 => BinOp::Add,
        2 => BinOp::Sub,
        3 => BinOp::Mul,
        4 => BinOp::Div,
        5 => BinOp::FloorDiv,
        6 => BinOp::Mod,
        7 => BinOp::Pow,
        8 => BinOp::LShift,
        9 => BinOp::RShift,
        10 => BinOp::BitAnd,
        11 => BinOp::BitOr,
        12 => BinOp::BitXor,
        13 => BinOp::MatMul,
        tag => {
            return Err(Error::BadTag {
                kind: "binop",
                tag: tag as u16,
            });
        }
    })
}

fn unary_op(w: &mut W, op: UnaryOp) {
    w.u8(match op {
        UnaryOp::Pos => 1,
        UnaryOp::Neg => 2,
        UnaryOp::Invert => 3,
        UnaryOp::Not => 4,
    });
}

fn unary_op_r(r: &mut R) -> Result<UnaryOp, Error> {
    Ok(match r.u8()? {
        1 => UnaryOp::Pos,
        2 => UnaryOp::Neg,
        3 => UnaryOp::Invert,
        4 => UnaryOp::Not,
        tag => {
            return Err(Error::BadTag {
                kind: "unaryop",
                tag: tag as u16,
            });
        }
    })
}

fn cmp_op(w: &mut W, op: CmpOp) {
    w.u8(match op {
        CmpOp::Eq => 1,
        CmpOp::Ne => 2,
        CmpOp::Lt => 3,
        CmpOp::Le => 4,
        CmpOp::Gt => 5,
        CmpOp::Ge => 6,
        CmpOp::Is => 7,
        CmpOp::IsNot => 8,
        CmpOp::In => 9,
        CmpOp::NotIn => 10,
    });
}

fn cmp_op_r(r: &mut R) -> Result<CmpOp, Error> {
    Ok(match r.u8()? {
        1 => CmpOp::Eq,
        2 => CmpOp::Ne,
        3 => CmpOp::Lt,
        4 => CmpOp::Le,
        5 => CmpOp::Gt,
        6 => CmpOp::Ge,
        7 => CmpOp::Is,
        8 => CmpOp::IsNot,
        9 => CmpOp::In,
        10 => CmpOp::NotIn,
        tag => {
            return Err(Error::BadTag {
                kind: "cmpop",
                tag: tag as u16,
            });
        }
    })
}

fn icmp_op(w: &mut W, op: ICmpOp) {
    w.u8(match op {
        ICmpOp::Eq => 1,
        ICmpOp::Ne => 2,
        ICmpOp::Lt => 3,
        ICmpOp::Le => 4,
        ICmpOp::Gt => 5,
        ICmpOp::Ge => 6,
    });
}

fn icmp_op_r(r: &mut R) -> Result<ICmpOp, Error> {
    Ok(match r.u8()? {
        1 => ICmpOp::Eq,
        2 => ICmpOp::Ne,
        3 => ICmpOp::Lt,
        4 => ICmpOp::Le,
        5 => ICmpOp::Gt,
        6 => ICmpOp::Ge,
        tag => {
            return Err(Error::BadTag {
                kind: "icmpop",
                tag: tag as u16,
            });
        }
    })
}

fn fcmp_op(w: &mut W, op: FCmpOp) {
    w.u8(match op {
        FCmpOp::Eq => 1,
        FCmpOp::Ne => 2,
        FCmpOp::Lt => 3,
        FCmpOp::Le => 4,
        FCmpOp::Gt => 5,
        FCmpOp::Ge => 6,
    });
}

fn fcmp_op_r(r: &mut R) -> Result<FCmpOp, Error> {
    Ok(match r.u8()? {
        1 => FCmpOp::Eq,
        2 => FCmpOp::Ne,
        3 => FCmpOp::Lt,
        4 => FCmpOp::Le,
        5 => FCmpOp::Gt,
        6 => FCmpOp::Ge,
        tag => {
            return Err(Error::BadTag {
                kind: "fcmpop",
                tag: tag as u16,
            });
        }
    })
}

fn put_scalar(w: &mut W, s: Scalar) {
    w.u8(match s {
        Scalar::Int64 => 1,
        Scalar::Float64 => 2,
        Scalar::Bool => 3,
    });
}

fn scalar_r(r: &mut R) -> Result<Scalar, Error> {
    Ok(match r.u8()? {
        1 => Scalar::Int64,
        2 => Scalar::Float64,
        3 => Scalar::Bool,
        tag => {
            return Err(Error::BadTag {
                kind: "scalar",
                tag: tag as u16,
            });
        }
    })
}

fn put_barrier(w: &mut W, b: BarrierKind) {
    w.u8(match b {
        BarrierKind::None => 1,
        BarrierKind::Ext => 2,
        BarrierKind::Dynamic => 3,
    });
}

fn barrier_r(r: &mut R) -> Result<BarrierKind, Error> {
    Ok(match r.u8()? {
        1 => BarrierKind::None,
        2 => BarrierKind::Ext,
        3 => BarrierKind::Dynamic,
        tag => {
            return Err(Error::BadTag {
                kind: "barrier",
                tag: tag as u16,
            });
        }
    })
}

fn instr(w: &mut W, i: &Instr) {
    use Instr as I;
    match i {
        I::Const { idx, into } => {
            w.u16(1);
            w.u32(idx.0);
            w.u32(into.0);
        }
        I::Copy { src, into } => {
            w.u16(2);
            w.u32(src.0);
            w.u32(into.0);
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
        } => {
            w.u16(match i {
                I::IAddOvf { .. } => 3,
                I::ISubOvf { .. } => 4,
                _ => 5,
            });
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
            w.u32(ovf.0);
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
            w.u16(match i {
                I::IAdd { .. } => 6,
                I::ISub { .. } => 7,
                I::IMul { .. } => 8,
                I::IDiv { .. } => 9,
                I::IMod { .. } => 10,
                I::Shl { .. } => 12,
                I::Shr { .. } => 13,
                I::BitAnd { .. } => 14,
                I::BitOr { .. } => 15,
                _ => 16,
            });
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
        }
        I::INeg { src, into } | I::BitNot { src, into } => {
            w.u16(if matches!(i, I::INeg { .. }) { 11 } else { 17 });
            w.u32(src.0);
            w.u32(into.0);
        }
        I::ICmp { op, lhs, rhs, into } => {
            w.u16(18);
            icmp_op(w, *op);
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
        }
        I::FAdd { lhs, rhs, into }
        | I::FSub { lhs, rhs, into }
        | I::FMul { lhs, rhs, into }
        | I::FDiv { lhs, rhs, into }
        | I::FMod { lhs, rhs, into } => {
            w.u16(match i {
                I::FAdd { .. } => 19,
                I::FSub { .. } => 20,
                I::FMul { .. } => 21,
                I::FDiv { .. } => 22,
                _ => 23,
            });
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
        }
        I::FNeg { src, into } => {
            w.u16(24);
            w.u32(src.0);
            w.u32(into.0);
        }
        I::FCmp { op, lhs, rhs, into } => {
            w.u16(25);
            fcmp_op(w, *op);
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
        }
        I::Box { src, scalar, into } => {
            w.u16(26);
            w.u32(src.0);
            put_scalar(w, *scalar);
            w.u32(into.0);
        }
        I::Unbox { src, scalar, into } => {
            w.u16(27);
            w.u32(src.0);
            put_scalar(w, *scalar);
            w.u32(into.0);
        }
        I::BinOp { op, lhs, rhs, into } => {
            w.u16(28);
            bin_op(w, *op);
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
        }
        I::UnaryOp { op, operand, into } => {
            w.u16(29);
            unary_op(w, *op);
            w.u32(operand.0);
            w.u32(into.0);
        }
        I::Compare { op, lhs, rhs, into } => {
            w.u16(30);
            cmp_op(w, *op);
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
        }
        I::GetAttr { obj, name, into } => {
            w.u16(31);
            w.u32(obj.0);
            w.u32(name.0);
            w.u32(into.0);
        }
        I::SetAttr { obj, name, value } => {
            w.u16(32);
            w.u32(obj.0);
            w.u32(name.0);
            w.u32(value.0);
        }
        I::DelAttr { obj, name } => {
            w.u16(33);
            w.u32(obj.0);
            w.u32(name.0);
        }
        I::GetItem { obj, key, into } => {
            w.u16(34);
            w.u32(obj.0);
            w.u32(key.0);
            w.u32(into.0);
        }
        I::SetItem { obj, key, value } => {
            w.u16(35);
            w.u32(obj.0);
            w.u32(key.0);
            w.u32(value.0);
        }
        I::DelItem { obj, key } => {
            w.u16(36);
            w.u32(obj.0);
            w.u32(key.0);
        }
        I::GetIter { obj, into } => {
            w.u16(37);
            w.u32(obj.0);
            w.u32(into.0);
        }
        I::Next { iter, into } => {
            w.u16(38);
            w.u32(iter.0);
            w.u32(into.0);
        }
        I::Call {
            callee,
            args,
            kwnames,
            into,
        } => {
            w.u16(39);
            w.u32(callee.0);
            w.seq(args, |w, v| w.u32(v.0));
            w.seq(kwnames, |w, (n, v)| {
                w.u32(n.0);
                w.u32(v.0);
            });
            w.u32(into.0);
        }
        I::CallMethod {
            name,
            recv,
            args,
            kwnames,
            into,
        } => {
            w.u16(40);
            w.u32(name.0);
            w.u32(recv.0);
            w.seq(args, |w, v| w.u32(v.0));
            w.seq(kwnames, |w, (n, v)| {
                w.u32(n.0);
                w.u32(v.0);
            });
            w.u32(into.0);
        }
        I::Truth { value, into } => {
            w.u16(41);
            w.u32(value.0);
            w.u32(into.0);
        }
        I::Len { value, into } => {
            w.u16(42);
            w.u32(value.0);
            w.u32(into.0);
        }
        I::Contains {
            haystack,
            needle,
            into,
        } => {
            w.u16(43);
            w.u32(haystack.0);
            w.u32(needle.0);
            w.u32(into.0);
        }
        I::Format { value, spec, into } => {
            w.u16(44);
            w.u32(value.0);
            w.u32(spec.0);
            w.u32(into.0);
        }
        I::BuildList { items, into } => {
            w.u16(45);
            w.seq(items, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::BuildTuple { items, into } => {
            w.u16(46);
            w.seq(items, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::BuildDict { items, into } => {
            w.u16(47);
            w.seq(items, |w, (k, v)| {
                w.u32(k.0);
                w.u32(v.0);
            });
            w.u32(into.0);
        }
        I::BuildSet { items, into } => {
            w.u16(48);
            w.seq(items, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::BuildStr { parts, into } => {
            w.u16(49);
            w.seq(parts, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::BuildSlice {
            start,
            stop,
            step,
            into,
        } => {
            w.u16(50);
            w.opt(start, |w, v| w.u32(v.0));
            w.u32(stop.0);
            w.opt(step, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::Unpack { src, n, star, into } => {
            w.u16(51);
            w.u32(src.0);
            w.u32(*n);
            w.opt(star, |w, s| w.u32(*s));
            w.seq(into, |w, v| w.u32(v.0));
        }
        I::Import {
            name,
            fromlist,
            level,
            into,
        } => {
            w.u16(52);
            w.u32(name.0);
            w.u32(fromlist.0);
            w.u32(level.0);
            w.u32(into.0);
        }
        I::ImportStar { module } => {
            w.u16(87);
            w.u32(module.0);
        }
        I::CallEx {
            callee,
            args,
            kwargs,
            into,
        } => {
            w.u16(88);
            w.u32(callee.0);
            w.u32(args.0);
            w.u32(kwargs.0);
            w.u32(into.0);
        }
        I::MakeFunction {
            func,
            cells,
            defaults,
            into,
        } => {
            w.u16(53);
            w.u32(func.0);
            w.seq(cells, |w, v| w.u32(v.0));
            w.seq(defaults, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::MakeClass {
            desc,
            bases,
            cells,
            kwds,
            into,
        } => {
            w.u16(54);
            w.u32(desc.0);
            w.seq(bases, |w, v| w.u32(v.0));
            w.seq(cells, |w, v| w.u32(v.0));
            w.u32(kwds.0);
            w.u32(into.0);
        }
        I::MakeCell { initial, into } => {
            w.u16(55);
            w.opt(initial, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::LoadClosure { index, into } => {
            w.u16(89);
            w.u32(*index);
            w.u32(into.0);
        }
        I::LoadCell { cell, into } => {
            w.u16(56);
            w.u32(cell.0);
            w.u32(into.0);
        }
        I::StoreCell { cell, value } => {
            w.u16(57);
            w.u32(cell.0);
            w.u32(value.0);
        }
        I::LoadGlobal { name, into } => {
            w.u16(58);
            w.u32(name.0);
            w.u32(into.0);
        }
        I::StoreGlobal { name, value } => {
            w.u16(59);
            w.u32(name.0);
            w.u32(value.0);
        }
        I::LoadName { name, into } => {
            w.u16(60);
            w.u32(name.0);
            w.u32(into.0);
        }
        I::StoreName { name, value } => {
            w.u16(61);
            w.u32(name.0);
            w.u32(value.0);
        }
        I::CallDirect { func, args, into } => {
            w.u16(62);
            w.u32(func.0);
            w.seq(args, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::LoadField {
            obj,
            shape,
            slot,
            into,
        } => {
            w.u16(63);
            w.u32(obj.0);
            w.u32(shape.0);
            w.u32(*slot);
            w.u32(into.0);
        }
        I::StoreField {
            obj,
            shape,
            slot,
            value,
            barrier,
        } => {
            w.u16(64);
            w.u32(obj.0);
            w.u32(shape.0);
            w.u32(*slot);
            w.u32(value.0);
            put_barrier(w, *barrier);
        }
        I::AllocInst { cls, shape, into } => {
            w.u16(65);
            w.u32(cls.0);
            w.u32(shape.0);
            w.u32(into.0);
        }
        I::AllocTuple { items, into } => {
            w.u16(66);
            w.seq(items, |w, v| w.u32(v.0));
            w.u32(into.0);
        }
        I::TypeGuard {
            value,
            ty,
            ok,
            fail,
        } => {
            w.u16(67);
            w.u32(value.0);
            put_ty(w, ty);
            w.u32(ok.0);
            w.u32(fail.0);
        }
        I::ShapeGuard {
            obj,
            shape,
            ok,
            fail,
        } => {
            w.u16(68);
            w.u32(obj.0);
            w.u32(shape.0);
            w.u32(ok.0);
            w.u32(fail.0);
        }
        I::BoundsCheck { index, len, into } => {
            w.u16(69);
            w.u32(index.0);
            w.u32(len.0);
            w.u32(into.0);
        }
        I::ListGetUnchecked { list, index, into } => {
            w.u16(70);
            w.u32(list.0);
            w.u32(index.0);
            w.u32(into.0);
        }
        I::TupleGetUnchecked { tuple, index, into } => {
            w.u16(71);
            w.u32(tuple.0);
            w.u32(*index);
            w.u32(into.0);
        }
        I::StackAlloc { cls, shape, into } => {
            w.u16(72);
            w.u32(cls.0);
            w.u32(shape.0);
            w.u32(into.0);
        }
        I::Yield { state, value, into } => {
            w.u16(73);
            w.u32(*state);
            w.u32(value.0);
            w.u32(into.0);
        }
        I::YieldFrom {
            state,
            iter,
            send,
            into,
        } => {
            w.u16(74);
            w.u32(*state);
            w.u32(iter.0);
            w.u32(send.0);
            w.u32(into.0);
        }
        I::Await { state, value, into } => {
            w.u16(75);
            w.u32(*state);
            w.u32(value.0);
            w.u32(into.0);
        }
        I::Safepoint => w.u16(76),
        I::CatchBegin {
            types,
            matched,
            exc,
        } => {
            w.u16(77);
            w.u32(types.0);
            w.u32(matched.0);
            w.u32(exc.0);
        }
        I::CatchEnd => w.u16(78),
        I::PushExcInfo { into } => {
            w.u16(79);
            w.u32(into.0);
        }
        I::PopExcInfo => w.u16(80),
        I::FinallyEnter { into } => {
            w.u16(81);
            w.u32(into.0);
        }
        I::FinallyExit { reason } => {
            w.u16(82);
            w.u32(reason.0);
        }
        I::CatchStarBegin {
            src,
            types,
            matched,
            exc,
            rest,
        } => {
            w.u16(90);
            w.u32(src.0);
            w.u32(types.0);
            w.u32(matched.0);
            w.u32(exc.0);
            w.u32(rest.0);
        }
        I::PrepReraiseStar { orig, excs, into } => {
            w.u16(91);
            w.u32(orig.0);
            w.u32(excs.0);
            w.u32(into.0);
        }
        I::IDivOvf {
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
            w.u16(if matches!(i, I::IDivOvf { .. }) {
                83
            } else {
                85
            });
            w.u32(lhs.0);
            w.u32(rhs.0);
            w.u32(into.0);
            w.u32(ovf.0);
        }
        I::INegOvf { src, into, ovf } => {
            w.u16(84);
            w.u32(src.0);
            w.u32(into.0);
            w.u32(ovf.0);
        }
    }
}

fn instr_r(r: &mut R) -> Result<Instr, Error> {
    use Instr as I;
    let tag = r.u16()?;
    Ok(match tag {
        1 => I::Const {
            idx: ConstIdx(r.u32()?),
            into: Value(r.u32()?),
        },
        2 => I::Copy {
            src: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        3..=5 => {
            let lhs = Value(r.u32()?);
            let rhs = Value(r.u32()?);
            let into = Value(r.u32()?);
            let ovf = Value(r.u32()?);
            match tag {
                3 => I::IAddOvf {
                    lhs,
                    rhs,
                    into,
                    ovf,
                },
                4 => I::ISubOvf {
                    lhs,
                    rhs,
                    into,
                    ovf,
                },
                _ => I::IMulOvf {
                    lhs,
                    rhs,
                    into,
                    ovf,
                },
            }
        }
        6..=10 | 12..=16 => {
            let lhs = Value(r.u32()?);
            let rhs = Value(r.u32()?);
            let into = Value(r.u32()?);
            match tag {
                6 => I::IAdd { lhs, rhs, into },
                7 => I::ISub { lhs, rhs, into },
                8 => I::IMul { lhs, rhs, into },
                9 => I::IDiv { lhs, rhs, into },
                10 => I::IMod { lhs, rhs, into },
                12 => I::Shl { lhs, rhs, into },
                13 => I::Shr { lhs, rhs, into },
                14 => I::BitAnd { lhs, rhs, into },
                15 => I::BitOr { lhs, rhs, into },
                _ => I::BitXor { lhs, rhs, into },
            }
        }
        11 | 17 => {
            let src = Value(r.u32()?);
            let into = Value(r.u32()?);
            if tag == 11 {
                I::INeg { src, into }
            } else {
                I::BitNot { src, into }
            }
        }
        18 => I::ICmp {
            op: icmp_op_r(r)?,
            lhs: Value(r.u32()?),
            rhs: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        19..=23 => {
            let lhs = Value(r.u32()?);
            let rhs = Value(r.u32()?);
            let into = Value(r.u32()?);
            match tag {
                19 => I::FAdd { lhs, rhs, into },
                20 => I::FSub { lhs, rhs, into },
                21 => I::FMul { lhs, rhs, into },
                22 => I::FDiv { lhs, rhs, into },
                _ => I::FMod { lhs, rhs, into },
            }
        }
        24 => I::FNeg {
            src: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        25 => I::FCmp {
            op: fcmp_op_r(r)?,
            lhs: Value(r.u32()?),
            rhs: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        26 => I::Box {
            src: Value(r.u32()?),
            scalar: scalar_r(r)?,
            into: Value(r.u32()?),
        },
        27 => I::Unbox {
            src: Value(r.u32()?),
            scalar: scalar_r(r)?,
            into: Value(r.u32()?),
        },
        28 => I::BinOp {
            op: bin_op_r(r)?,
            lhs: Value(r.u32()?),
            rhs: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        29 => I::UnaryOp {
            op: unary_op_r(r)?,
            operand: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        30 => I::Compare {
            op: cmp_op_r(r)?,
            lhs: Value(r.u32()?),
            rhs: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        31 => I::GetAttr {
            obj: Value(r.u32()?),
            name: Sym(r.u32()?),
            into: Value(r.u32()?),
        },
        32 => I::SetAttr {
            obj: Value(r.u32()?),
            name: Sym(r.u32()?),
            value: Value(r.u32()?),
        },
        33 => I::DelAttr {
            obj: Value(r.u32()?),
            name: Sym(r.u32()?),
        },
        34 => I::GetItem {
            obj: Value(r.u32()?),
            key: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        35 => I::SetItem {
            obj: Value(r.u32()?),
            key: Value(r.u32()?),
            value: Value(r.u32()?),
        },
        36 => I::DelItem {
            obj: Value(r.u32()?),
            key: Value(r.u32()?),
        },
        37 => I::GetIter {
            obj: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        38 => I::Next {
            iter: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        39 => I::Call {
            callee: Value(r.u32()?),
            args: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            kwnames: r.seq(|r| Ok((Sym(r.u32()?), Value(r.u32()?))))?.into(),
            into: Value(r.u32()?),
        },
        40 => I::CallMethod {
            name: Sym(r.u32()?),
            recv: Value(r.u32()?),
            args: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            kwnames: r.seq(|r| Ok((Sym(r.u32()?), Value(r.u32()?))))?.into(),
            into: Value(r.u32()?),
        },
        41 => I::Truth {
            value: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        42 => I::Len {
            value: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        43 => I::Contains {
            haystack: Value(r.u32()?),
            needle: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        44 => I::Format {
            value: Value(r.u32()?),
            spec: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        45 => I::BuildList {
            items: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        46 => I::BuildTuple {
            items: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        47 => I::BuildDict {
            items: r.seq(|r| Ok((Value(r.u32()?), Value(r.u32()?))))?.into(),
            into: Value(r.u32()?),
        },
        48 => I::BuildSet {
            items: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        49 => I::BuildStr {
            parts: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        50 => I::BuildSlice {
            start: r.opt(|r| Ok(Value(r.u32()?)))?,
            stop: Value(r.u32()?),
            step: r.opt(|r| Ok(Value(r.u32()?)))?,
            into: Value(r.u32()?),
        },
        51 => I::Unpack {
            src: Value(r.u32()?),
            n: r.u32()?,
            star: r.opt(|r| r.u32())?,
            into: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
        },
        52 => I::Import {
            name: Sym(r.u32()?),
            fromlist: Value(r.u32()?),
            level: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        87 => I::ImportStar {
            module: Value(r.u32()?),
        },
        88 => I::CallEx {
            callee: Value(r.u32()?),
            args: Value(r.u32()?),
            kwargs: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        89 => I::LoadClosure {
            index: r.u32()?,
            into: Value(r.u32()?),
        },
        53 => I::MakeFunction {
            func: FnId(r.u32()?),
            cells: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            defaults: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        54 => I::MakeClass {
            desc: ClassId(r.u32()?),
            bases: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            cells: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            kwds: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        55 => I::MakeCell {
            initial: r.opt(|r| Ok(Value(r.u32()?)))?,
            into: Value(r.u32()?),
        },
        56 => I::LoadCell {
            cell: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        57 => I::StoreCell {
            cell: Value(r.u32()?),
            value: Value(r.u32()?),
        },
        58 => I::LoadGlobal {
            name: Sym(r.u32()?),
            into: Value(r.u32()?),
        },
        59 => I::StoreGlobal {
            name: Sym(r.u32()?),
            value: Value(r.u32()?),
        },
        60 => I::LoadName {
            name: Sym(r.u32()?),
            into: Value(r.u32()?),
        },
        61 => I::StoreName {
            name: Sym(r.u32()?),
            value: Value(r.u32()?),
        },
        62 => I::CallDirect {
            func: FnId(r.u32()?),
            args: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        63 => I::LoadField {
            obj: Value(r.u32()?),
            shape: ShapeId(r.u32()?),
            slot: r.u32()?,
            into: Value(r.u32()?),
        },
        64 => I::StoreField {
            obj: Value(r.u32()?),
            shape: ShapeId(r.u32()?),
            slot: r.u32()?,
            value: Value(r.u32()?),
            barrier: barrier_r(r)?,
        },
        65 => I::AllocInst {
            cls: ClassId(r.u32()?),
            shape: ShapeId(r.u32()?),
            into: Value(r.u32()?),
        },
        66 => I::AllocTuple {
            items: r.seq(|r| Ok(Value(r.u32()?)))?.into(),
            into: Value(r.u32()?),
        },
        67 => I::TypeGuard {
            value: Value(r.u32()?),
            ty: ty_r(r)?,
            ok: Value(r.u32()?),
            fail: Value(r.u32()?),
        },
        68 => I::ShapeGuard {
            obj: Value(r.u32()?),
            shape: ShapeId(r.u32()?),
            ok: Value(r.u32()?),
            fail: Value(r.u32()?),
        },
        69 => I::BoundsCheck {
            index: Value(r.u32()?),
            len: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        70 => I::ListGetUnchecked {
            list: Value(r.u32()?),
            index: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        71 => I::TupleGetUnchecked {
            tuple: Value(r.u32()?),
            index: r.u32()?,
            into: Value(r.u32()?),
        },
        72 => I::StackAlloc {
            cls: ClassId(r.u32()?),
            shape: ShapeId(r.u32()?),
            into: Value(r.u32()?),
        },
        73 => I::Yield {
            state: r.u32()?,
            value: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        74 => I::YieldFrom {
            state: r.u32()?,
            iter: Value(r.u32()?),
            send: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        75 => I::Await {
            state: r.u32()?,
            value: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        76 => I::Safepoint,
        77 => I::CatchBegin {
            types: Value(r.u32()?),
            matched: Value(r.u32()?),
            exc: Value(r.u32()?),
        },
        78 => I::CatchEnd,
        79 => I::PushExcInfo {
            into: Value(r.u32()?),
        },
        80 => I::PopExcInfo,
        81 => I::FinallyEnter {
            into: Value(r.u32()?),
        },
        82 => I::FinallyExit {
            reason: Value(r.u32()?),
        },
        90 => I::CatchStarBegin {
            src: Value(r.u32()?),
            types: Value(r.u32()?),
            matched: Value(r.u32()?),
            exc: Value(r.u32()?),
            rest: Value(r.u32()?),
        },
        91 => I::PrepReraiseStar {
            orig: Value(r.u32()?),
            excs: Value(r.u32()?),
            into: Value(r.u32()?),
        },
        83 | 85 => {
            let lhs = Value(r.u32()?);
            let rhs = Value(r.u32()?);
            let into = Value(r.u32()?);
            let ovf = Value(r.u32()?);
            if tag == 83 {
                I::IDivOvf {
                    lhs,
                    rhs,
                    into,
                    ovf,
                }
            } else {
                I::ShlOvf {
                    lhs,
                    rhs,
                    into,
                    ovf,
                }
            }
        }
        84 => I::INegOvf {
            src: Value(r.u32()?),
            into: Value(r.u32()?),
            ovf: Value(r.u32()?),
        },
        tag => return Err(Error::BadTag { kind: "instr", tag }),
    })
}

fn terminator(w: &mut W, t: &Terminator) {
    match t {
        Terminator::Jump { to } => {
            w.u16(1);
            w.u32(to.0);
        }
        Terminator::Branch {
            cond,
            then_to,
            else_to,
        } => {
            w.u16(2);
            w.u32(cond.0);
            w.u32(then_to.0);
            w.u32(else_to.0);
        }
        Terminator::Switch { on, cases, default } => {
            w.u16(3);
            w.u32(on.0);
            w.seq(cases, |w, (v, b)| {
                w.i64(*v);
                w.u32(b.0);
            });
            w.u32(default.0);
        }
        Terminator::Return { value } => {
            w.u16(4);
            w.u32(value.0);
        }
        Terminator::Raise { exc, cause } => {
            w.u16(5);
            w.opt(exc, |w, v| w.u32(v.0));
            w.opt(cause, |w, v| w.u32(v.0));
        }
        Terminator::Reraise => w.u16(6),
        Terminator::Unreachable => w.u16(7),
    }
}

fn terminator_r(r: &mut R) -> Result<Terminator, Error> {
    Ok(match r.u16()? {
        1 => Terminator::Jump {
            to: BlockId(r.u32()?),
        },
        2 => Terminator::Branch {
            cond: Value(r.u32()?),
            then_to: BlockId(r.u32()?),
            else_to: BlockId(r.u32()?),
        },
        3 => Terminator::Switch {
            on: Value(r.u32()?),
            cases: r.seq(|r| Ok((r.i64()?, BlockId(r.u32()?))))?.into(),
            default: BlockId(r.u32()?),
        },
        4 => Terminator::Return {
            value: Value(r.u32()?),
        },
        5 => Terminator::Raise {
            exc: r.opt(|r| Ok(Value(r.u32()?)))?,
            cause: r.opt(|r| Ok(Value(r.u32()?)))?,
        },
        6 => Terminator::Reraise,
        7 => Terminator::Unreachable,
        tag => {
            return Err(Error::BadTag {
                kind: "terminator",
                tag,
            });
        }
    })
}

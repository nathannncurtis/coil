//! Coil IR types and identifiers (spec section 5).

use std::fmt;

/// Interned string. Indices the string table of the module's [`crate::ir::ConstPool`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Sym(pub u32);

/// SSA value. Indices `Function::types`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Value(pub u32);

impl Value {
    pub const fn idx(self) -> usize {
        self.0 as usize
    }
}

/// Basic-block index within a function.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlockId(pub u32);

/// Instruction index within a function: instructions are numbered across
/// blocks in block order, phis excluded. Keys `Function::lines`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct InstrId(pub u32);

/// Function index within `Module::functions`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FnId(pub u32);

/// Class index within `Module::classes`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClassId(pub u32);

/// Index into a function-signature table; the table itself is built by
/// `coilc`'s signature analysis.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SigId(pub u32);

/// Hidden-class (shape) index; the shape table is built by shape analysis.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ShapeId(pub u32);

/// Index into `ConstPool::consts`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConstIdx(pub u32);

/// Index into the build's module table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ModId(pub u32);

/// Identity of an extension-defined type: its qualified name as an interned
/// symbol (`Type::Ext`). Extension types are runtime objects, so the IR names
/// them rather than embedding them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeRef(pub Sym);

/// Python-level type of a value. `Int64`/`Float64`/`Bool` are the unboxable
/// scalars; `Top` is the fully dynamic boxed lattice point; `Bottom` is the
/// unreachable type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    Top,
    Union(Box<[Type]>),
    /// Instance of a compiled class; `exact` when no subclass is possible.
    Inst {
        cls: ClassId,
        exact: bool,
    },
    /// An int whose value fits in 64 bits.
    Int64,
    Float64,
    Bool,
    None,
    /// Arbitrary-precision int; also the type of a boxed `Int64`.
    BigInt,
    Str,
    Bytes,
    Tuple(Box<[Type]>),
    List,
    Dict,
    Set,
    Func(SigId),
    Module(ModId),
    Class(ClassId),
    /// Instance of an extension-defined type (its own memory regime, section 9).
    Ext(TypeRef),
    Bottom,
}

impl Type {
    pub const fn is_top(&self) -> bool {
        matches!(self, Type::Top)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Top => write!(f, "Top"),
            Type::Union(parts) => {
                write!(f, "Union[")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, "]")
            }
            Type::Inst { cls, exact } => write!(
                f,
                "Inst(C{}, {})",
                cls.0,
                if *exact { "exact" } else { "sub" }
            ),
            Type::Int64 => write!(f, "Int64"),
            Type::Float64 => write!(f, "Float64"),
            Type::Bool => write!(f, "Bool"),
            Type::None => write!(f, "None"),
            Type::BigInt => write!(f, "BigInt"),
            Type::Str => write!(f, "Str"),
            Type::Bytes => write!(f, "Bytes"),
            Type::Tuple(parts) => {
                write!(f, "Tuple[")?;
                for (i, p) in parts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, "]")
            }
            Type::List => write!(f, "List"),
            Type::Dict => write!(f, "Dict"),
            Type::Set => write!(f, "Set"),
            Type::Func(sig) => write!(f, "Func(S{})", sig.0),
            Type::Module(m) => write!(f, "Module(M{})", m.0),
            Type::Class(c) => write!(f, "Class(C{})", c.0),
            Type::Ext(t) => write!(f, "Ext(S{})", t.0.0),
            Type::Bottom => write!(f, "Bottom"),
        }
    }
}

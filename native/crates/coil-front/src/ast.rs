//! The Python 3.12 AST: node types generated from the pinned Parser/Python.asdl
//! (see `ast_generated`) plus the hand-written constant values.

pub use crate::ast_generated::*;

/// A constant value, as stored in `Expr::Constant` and `Pattern::MatchSingleton`.
#[derive(Clone, Debug)]
pub enum Constant {
    /// The string's code points: Python strings may contain lone surrogates,
    /// which UTF-8 cannot represent.
    Str(Vec<u32>),
    Bytes(Vec<u8>),
    /// Normalised decimal digits, an optional leading `-` and no leading
    /// zeros: arbitrary precision without a bignum crate.
    Int(String),
    Float(f64),
    Complex {
        real: f64,
        imag: f64,
    },
    Bool(bool),
    None,
    Ellipsis,
}

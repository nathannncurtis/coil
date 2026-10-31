//! Hand-written signatures for builtins and runtime types, feeding inference
//! (spec section 7). Every entry is data: the parameter types (the receiver
//! first for a method, keyed `<recv>.<name>`), the result type, and whether
//! the overflow rule applies - an `Int64` result the operation may have to
//! box to `BigInt`. Lookup walks a key's entries, most specific first, and
//! takes the first that fits the argument types.

use coil_ir::types::Type;

/// The type vocabulary of the signature table: the IR's lattice points that
/// exist without a module context, plus `Any` for "every value". `Tuple`
/// reads as `Top` in the IR: there is no unknown-element tuple point, and
/// propagating container element types is inference's rule (spec section 7),
/// not a signature's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    Any,
    Bool,
    None,
    Int64,
    BigInt,
    Float64,
    Str,
    Bytes,
    List,
    Dict,
    Set,
    Tuple,
    Bottom,
}

impl Ty {
    /// The IR type a signature's `Ty` narrows a value to.
    pub fn to_type(self) -> Type {
        match self {
            Ty::Any => Type::Top,
            Ty::Bool => Type::Bool,
            Ty::None => Type::None,
            Ty::Int64 => Type::Int64,
            Ty::BigInt => Type::BigInt,
            Ty::Float64 => Type::Float64,
            Ty::Str => Type::Str,
            Ty::Bytes => Type::Bytes,
            Ty::List => Type::List,
            Ty::Dict => Type::Dict,
            Ty::Set => Type::Set,
            Ty::Tuple => Type::Top,
            Ty::Bottom => Type::Bottom,
        }
    }
}

/// One parameter: its type, and whether a call may omit it. Optional
/// parameters are trailing in every signature.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub ty: Ty,
    pub optional: bool,
}

const fn p(ty: Ty) -> Param {
    Param {
        ty,
        optional: false,
    }
}

const fn o(ty: Ty) -> Param {
    Param { ty, optional: true }
}

/// One hand-written signature.
#[derive(Debug)]
pub struct Sig {
    pub params: &'static [Param],
    /// The element type of variadic arguments past `params`.
    pub vararg: Option<Ty>,
    pub result: Ty,
    /// The result is an `Int64` the overflow rule may box to `BigInt`
    /// (spec section 7): the value depends on the operands' magnitude.
    pub may_overflow: bool,
}

const fn sig(params: &'static [Param], result: Ty) -> Sig {
    Sig {
        params,
        vararg: None,
        result,
        may_overflow: false,
    }
}

const fn vsig(params: &'static [Param], vararg: Ty, result: Ty) -> Sig {
    Sig {
        params,
        vararg: Some(vararg),
        result,
        may_overflow: false,
    }
}

const fn ovf(params: &'static [Param], result: Ty) -> Sig {
    Sig {
        params,
        vararg: None,
        result,
        may_overflow: true,
    }
}

/// The result of a signature lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Call {
    pub result: Ty,
    pub may_overflow: bool,
}

/// The signature table, sorted by key. Entries within a key run most
/// specific first; a generic `Any`-parameter entry answers what the
/// specific ones decline.
static SIGS: &[(&str, &[Sig])] = &[
    (
        "abs",
        &[
            sig(&[p(Ty::Int64)], Ty::Int64).with_overflow(),
            sig(&[p(Ty::Float64)], Ty::Float64),
            sig(&[p(Ty::Any)], Ty::Any),
        ],
    ),
    ("bool", &[sig(&[], Ty::Bool), sig(&[p(Ty::Any)], Ty::Bool)]),
    ("dict", &[sig(&[], Ty::Dict), sig(&[o(Ty::Any)], Ty::Dict)]),
    ("dict.clear", &[sig(&[p(Ty::Dict)], Ty::None)]),
    ("dict.copy", &[sig(&[p(Ty::Dict)], Ty::Dict)]),
    (
        "dict.fromkeys",
        &[sig(&[p(Ty::Dict), p(Ty::Any), o(Ty::Any)], Ty::Dict)],
    ),
    (
        "dict.get",
        &[sig(&[p(Ty::Dict), p(Ty::Any), o(Ty::Any)], Ty::Any)],
    ),
    ("dict.items", &[sig(&[p(Ty::Dict)], Ty::Any)]),
    ("dict.keys", &[sig(&[p(Ty::Dict)], Ty::Any)]),
    (
        "dict.pop",
        &[sig(&[p(Ty::Dict), p(Ty::Any), o(Ty::Any)], Ty::Any)],
    ),
    ("dict.popitem", &[sig(&[p(Ty::Dict)], Ty::Any)]),
    (
        "dict.setdefault",
        &[sig(&[p(Ty::Dict), p(Ty::Any), o(Ty::Any)], Ty::Any)],
    ),
    ("dict.update", &[sig(&[p(Ty::Dict), o(Ty::Any)], Ty::None)]),
    ("dict.values", &[sig(&[p(Ty::Dict)], Ty::Any)]),
    ("enumerate", &[sig(&[p(Ty::Any), o(Ty::Int64)], Ty::Any)]),
    ("filter", &[sig(&[p(Ty::Any), p(Ty::Any)], Ty::Any)]),
    (
        "float",
        &[sig(&[], Ty::Float64), sig(&[p(Ty::Any)], Ty::Float64)],
    ),
    (
        "int",
        &[
            sig(&[], Ty::Int64),
            sig(&[p(Ty::Int64)], Ty::Int64),
            sig(&[p(Ty::Any)], Ty::BigInt),
            sig(&[p(Ty::Str), o(Ty::Int64)], Ty::BigInt),
        ],
    ),
    ("isinstance", &[sig(&[p(Ty::Any), p(Ty::Any)], Ty::Bool)]),
    ("iter", &[sig(&[p(Ty::Any), o(Ty::Any)], Ty::Any)]),
    ("len", &[sig(&[p(Ty::Any)], Ty::Int64)]),
    ("list", &[sig(&[], Ty::List), sig(&[p(Ty::Any)], Ty::List)]),
    ("list.append", &[sig(&[p(Ty::List), p(Ty::Any)], Ty::None)]),
    ("list.clear", &[sig(&[p(Ty::List)], Ty::None)]),
    ("list.copy", &[sig(&[p(Ty::List)], Ty::List)]),
    ("list.count", &[sig(&[p(Ty::List), p(Ty::Any)], Ty::Int64)]),
    ("list.extend", &[sig(&[p(Ty::List), p(Ty::Any)], Ty::None)]),
    (
        "list.index",
        &[sig(
            &[p(Ty::List), p(Ty::Any), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    (
        "list.insert",
        &[sig(&[p(Ty::List), p(Ty::Int64), p(Ty::Any)], Ty::None)],
    ),
    ("list.pop", &[sig(&[p(Ty::List), o(Ty::Int64)], Ty::Any)]),
    ("list.remove", &[sig(&[p(Ty::List), p(Ty::Any)], Ty::None)]),
    ("list.reverse", &[sig(&[p(Ty::List)], Ty::None)]),
    ("list.sort", &[sig(&[p(Ty::List)], Ty::None)]),
    ("map", &[vsig(&[p(Ty::Any), p(Ty::Any)], Ty::Any, Ty::Any)]),
    ("math.acos", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.acosh", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.asin", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.asinh", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.atan", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    (
        "math.atan2",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Float64)],
    ),
    ("math.atanh", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.cbrt", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.ceil", &[ovf(&[p(Ty::Float64)], Ty::Int64)]),
    (
        "math.comb",
        &[ovf(&[p(Ty::Int64), p(Ty::Int64)], Ty::Int64)],
    ),
    (
        "math.copysign",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Float64)],
    ),
    ("math.cos", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.cosh", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.degrees", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.dist", &[sig(&[p(Ty::Any), p(Ty::Any)], Ty::Float64)]),
    ("math.erf", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.erfc", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.exp", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.expm1", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.fabs", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.factorial", &[ovf(&[p(Ty::Int64)], Ty::Int64)]),
    ("math.floor", &[ovf(&[p(Ty::Float64)], Ty::Int64)]),
    (
        "math.fmod",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Float64)],
    ),
    ("math.frexp", &[sig(&[p(Ty::Float64)], Ty::Tuple)]),
    ("math.fsum", &[sig(&[p(Ty::Any)], Ty::Float64)]),
    ("math.gamma", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.gcd", &[vsig(&[o(Ty::Int64)], Ty::Int64, Ty::Int64)]),
    (
        "math.hypot",
        &[vsig(&[o(Ty::Float64)], Ty::Float64, Ty::Float64)],
    ),
    (
        "math.isclose",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Bool)],
    ),
    ("math.isfinite", &[sig(&[p(Ty::Any)], Ty::Bool)]),
    ("math.isinf", &[sig(&[p(Ty::Any)], Ty::Bool)]),
    ("math.isnan", &[sig(&[p(Ty::Any)], Ty::Bool)]),
    ("math.isqrt", &[sig(&[p(Ty::Int64)], Ty::Int64)]),
    (
        "math.lcm",
        &[ovf_var(&[o(Ty::Int64)], Ty::Int64, Ty::Int64)],
    ),
    (
        "math.ldexp",
        &[sig(&[p(Ty::Float64), p(Ty::Int64)], Ty::Float64)],
    ),
    ("math.lgamma", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    (
        "math.log",
        &[sig(&[p(Ty::Float64), o(Ty::Float64)], Ty::Float64)],
    ),
    ("math.log10", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.log1p", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.log2", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.modf", &[sig(&[p(Ty::Float64)], Ty::Tuple)]),
    (
        "math.nextafter",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Float64)],
    ),
    (
        "math.perm",
        &[ovf(&[p(Ty::Int64), o(Ty::Int64)], Ty::Int64)],
    ),
    (
        "math.pow",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Float64)],
    ),
    ("math.prod", &[sig(&[p(Ty::Any), o(Ty::Any)], Ty::Any)]),
    ("math.radians", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    (
        "math.remainder",
        &[sig(&[p(Ty::Float64), p(Ty::Float64)], Ty::Float64)],
    ),
    ("math.sin", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.sinh", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.sqrt", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.tan", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.tanh", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("math.trunc", &[ovf(&[p(Ty::Float64)], Ty::Int64)]),
    ("math.ulp", &[sig(&[p(Ty::Float64)], Ty::Float64)]),
    ("max", &[vsig(&[p(Ty::Any)], Ty::Any, Ty::Any)]),
    ("min", &[vsig(&[p(Ty::Any)], Ty::Any, Ty::Any)]),
    ("next", &[sig(&[p(Ty::Any), o(Ty::Any)], Ty::Any)]),
    ("print", &[vsig(&[], Ty::Any, Ty::None)]),
    (
        "range",
        &[sig(&[p(Ty::Int64), o(Ty::Int64), o(Ty::Int64)], Ty::Any)],
    ),
    ("set", &[sig(&[], Ty::Set), sig(&[o(Ty::Any)], Ty::Set)]),
    ("set.add", &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::None)]),
    ("set.clear", &[sig(&[p(Ty::Set)], Ty::None)]),
    ("set.copy", &[sig(&[p(Ty::Set)], Ty::Set)]),
    (
        "set.difference",
        &[vsig(&[p(Ty::Set), o(Ty::Any)], Ty::Any, Ty::Set)],
    ),
    (
        "set.difference_update",
        &[vsig(&[p(Ty::Set), o(Ty::Any)], Ty::Any, Ty::None)],
    ),
    ("set.discard", &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::None)]),
    (
        "set.intersection",
        &[vsig(&[p(Ty::Set), o(Ty::Any)], Ty::Any, Ty::Set)],
    ),
    (
        "set.intersection_update",
        &[vsig(&[p(Ty::Set), o(Ty::Any)], Ty::Any, Ty::None)],
    ),
    (
        "set.isdisjoint",
        &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::Bool)],
    ),
    ("set.issubset", &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::Bool)]),
    (
        "set.issuperset",
        &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::Bool)],
    ),
    ("set.pop", &[sig(&[p(Ty::Set)], Ty::Any)]),
    ("set.remove", &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::None)]),
    (
        "set.symmetric_difference",
        &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::Set)],
    ),
    (
        "set.symmetric_difference_update",
        &[sig(&[p(Ty::Set), p(Ty::Any)], Ty::None)],
    ),
    (
        "set.union",
        &[vsig(&[p(Ty::Set), o(Ty::Any)], Ty::Any, Ty::Set)],
    ),
    (
        "set.update",
        &[vsig(&[p(Ty::Set), o(Ty::Any)], Ty::Any, Ty::None)],
    ),
    ("sorted", &[sig(&[p(Ty::Any)], Ty::List)]),
    ("str", &[sig(&[], Ty::Str), sig(&[p(Ty::Any)], Ty::Str)]),
    ("str.capitalize", &[sig(&[p(Ty::Str)], Ty::Str)]),
    ("str.casefold", &[sig(&[p(Ty::Str)], Ty::Str)]),
    (
        "str.center",
        &[sig(&[p(Ty::Str), p(Ty::Int64), o(Ty::Str)], Ty::Str)],
    ),
    (
        "str.count",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    (
        "str.encode",
        &[sig(&[p(Ty::Str), o(Ty::Str), o(Ty::Str)], Ty::Bytes)],
    ),
    (
        "str.endswith",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Bool,
        )],
    ),
    (
        "str.expandtabs",
        &[sig(&[p(Ty::Str), o(Ty::Int64)], Ty::Str)],
    ),
    (
        "str.find",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    ("str.format", &[vsig(&[p(Ty::Str)], Ty::Any, Ty::Str)]),
    ("str.format_map", &[sig(&[p(Ty::Str), p(Ty::Any)], Ty::Str)]),
    (
        "str.index",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    ("str.isalnum", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isalpha", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isascii", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isdecimal", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isdigit", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isidentifier", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.islower", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isnumeric", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isprintable", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isspace", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.istitle", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.isupper", &[sig(&[p(Ty::Str)], Ty::Bool)]),
    ("str.join", &[sig(&[p(Ty::Str), p(Ty::Any)], Ty::Str)]),
    (
        "str.ljust",
        &[sig(&[p(Ty::Str), p(Ty::Int64), o(Ty::Str)], Ty::Str)],
    ),
    ("str.lower", &[sig(&[p(Ty::Str)], Ty::Str)]),
    ("str.lstrip", &[sig(&[p(Ty::Str), o(Ty::Str)], Ty::Str)]),
    (
        "str.maketrans",
        &[sig(&[p(Ty::Str), p(Ty::Any), o(Ty::Any)], Ty::Dict)],
    ),
    (
        "str.partition",
        &[sig(&[p(Ty::Str), p(Ty::Str)], Ty::Tuple)],
    ),
    (
        "str.removeprefix",
        &[sig(&[p(Ty::Str), p(Ty::Str)], Ty::Str)],
    ),
    (
        "str.removesuffix",
        &[sig(&[p(Ty::Str), p(Ty::Str)], Ty::Str)],
    ),
    (
        "str.replace",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), p(Ty::Str), o(Ty::Int64)],
            Ty::Str,
        )],
    ),
    (
        "str.rfind",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    (
        "str.rindex",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    (
        "str.rjust",
        &[sig(&[p(Ty::Str), p(Ty::Int64), o(Ty::Str)], Ty::Str)],
    ),
    (
        "str.rpartition",
        &[sig(&[p(Ty::Str), p(Ty::Str)], Ty::Tuple)],
    ),
    (
        "str.rsplit",
        &[sig(&[p(Ty::Str), o(Ty::Str), o(Ty::Int64)], Ty::List)],
    ),
    ("str.rstrip", &[sig(&[p(Ty::Str), o(Ty::Str)], Ty::Str)]),
    (
        "str.split",
        &[sig(&[p(Ty::Str), o(Ty::Str), o(Ty::Int64)], Ty::List)],
    ),
    (
        "str.splitlines",
        &[sig(&[p(Ty::Str), o(Ty::Bool)], Ty::List)],
    ),
    (
        "str.startswith",
        &[sig(
            &[p(Ty::Str), p(Ty::Str), o(Ty::Int64), o(Ty::Int64)],
            Ty::Bool,
        )],
    ),
    ("str.strip", &[sig(&[p(Ty::Str), o(Ty::Str)], Ty::Str)]),
    ("str.swapcase", &[sig(&[p(Ty::Str)], Ty::Str)]),
    ("str.title", &[sig(&[p(Ty::Str)], Ty::Str)]),
    ("str.upper", &[sig(&[p(Ty::Str)], Ty::Str)]),
    ("str.zfill", &[sig(&[p(Ty::Str), p(Ty::Int64)], Ty::Str)]),
    ("sum", &[sig(&[p(Ty::Any), o(Ty::Any)], Ty::Any)]),
    (
        "tuple",
        &[sig(&[], Ty::Tuple), sig(&[o(Ty::Any)], Ty::Tuple)],
    ),
    (
        "tuple.count",
        &[sig(&[p(Ty::Tuple), p(Ty::Any)], Ty::Int64)],
    ),
    (
        "tuple.index",
        &[sig(
            &[p(Ty::Tuple), p(Ty::Any), o(Ty::Int64), o(Ty::Int64)],
            Ty::Int64,
        )],
    ),
    ("zip", &[vsig(&[p(Ty::Any)], Ty::Any, Ty::Any)]),
];

/// The runtime singletons by name: the value's exact type where the IR
/// lattice has a point for it. `NotImplemented` and `Ellipsis` have none, so
/// they read as `Any` (they never flow into an unboxed operation).
static SINGLETONS: &[(&str, Ty)] = &[
    ("Ellipsis", Ty::Any),
    ("False", Ty::Bool),
    ("None", Ty::None),
    ("NotImplemented", Ty::Any),
    ("True", Ty::Bool),
    ("__debug__", Ty::Bool),
    ("math.e", Ty::Float64),
    ("math.inf", Ty::Float64),
    ("math.nan", Ty::Float64),
    ("math.pi", Ty::Float64),
    ("math.tau", Ty::Float64),
];

/// Signature lookup for a call site. `key` is the builtin's name, a method
/// key `<receiver>.<name>` (with the receiver as `args[0]`), or `math.<fn>`
/// for a member of `math`. `None` when no entry fits: inference falls back
/// to `Top` for the result.
pub fn call(key: &str, args: &[Ty]) -> Option<Call> {
    let sigs = SIGS
        .binary_search_by(|(k, _)| (*k).cmp(key))
        .ok()
        .map(|at| SIGS[at].1)?;
    for sig in sigs {
        if fits(sig, args) {
            return Some(Call {
                result: sig.result,
                may_overflow: sig.may_overflow,
            });
        }
    }
    None
}

/// The exact type of a runtime singleton by name (spec section 8.1): the
/// builtins' own singletons and the `math` constants.
pub fn singleton(name: &str) -> Option<Ty> {
    SINGLETONS
        .binary_search_by(|(k, _)| (*k).cmp(name))
        .ok()
        .map(|at| SINGLETONS[at].1)
}

/// Whether a call with these argument types can take this signature: the
/// arity fits (optional parameters trailing, variadic past `params`) and
/// every argument's type flows into its parameter's.
fn fits(sig: &Sig, args: &[Ty]) -> bool {
    let required = sig.params.iter().take_while(|p| !p.optional).count();
    let (min, max) = match sig.vararg {
        Some(_) => (required, usize::MAX),
        None => (required, sig.params.len()),
    };
    if args.len() < min || args.len() > max {
        return false;
    }
    if !sig.params.iter().zip(args).all(|(p, &a)| accepts(p.ty, a)) {
        return false;
    }
    match sig.vararg {
        Some(elem) => args
            .iter()
            .skip(sig.params.len())
            .all(|&a| accepts(elem, a)),
        None => true,
    }
}

/// Whether an argument of type `arg` flows into a parameter of type `param`:
/// an `Any` parameter takes everything, `Bool` is a Python `int` (and so a
/// `float`), and an `Any` argument fits only an `Any` parameter, so the
/// specific entries decline it and the generic one answers.
fn accepts(param: Ty, arg: Ty) -> bool {
    match (param, arg) {
        (Ty::Any, _) | (_, Ty::Bottom) => true,
        (Ty::Int64 | Ty::Float64, Ty::Bool) => true,
        _ => param == arg,
    }
}

impl Sig {
    const fn with_overflow(mut self) -> Sig {
        self.may_overflow = true;
        self
    }
}

/// A variadic signature whose result the overflow rule may box.
const fn ovf_var(params: &'static [Param], vararg: Ty, result: Ty) -> Sig {
    Sig {
        params,
        vararg: Some(vararg),
        result,
        may_overflow: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(key: &str, args: &[Ty]) -> Option<Ty> {
        call(key, args).map(|c| c.result)
    }

    #[test]
    fn the_table_is_sorted_by_key() {
        for w in SIGS.windows(2) {
            assert!(w[0].0 < w[1].0, "'{}' is not before '{}'", w[0].0, w[1].0);
        }
        for w in SINGLETONS.windows(2) {
            assert!(w[0].0 < w[1].0, "'{}' is not before '{}'", w[0].0, w[1].0);
        }
    }

    #[test]
    fn optional_parameters_are_trailing_and_entries_unique() {
        for (_, sigs) in SIGS {
            for sig in *sigs {
                let first_optional = sig.params.iter().position(|p| p.optional);
                let last_required = sig.params.iter().rposition(|p| !p.optional);
                if let (Some(opt), Some(req)) = (first_optional, last_required) {
                    assert!(opt > req, "an optional parameter precedes a required one");
                }
            }
            for w in sigs.windows(2) {
                let same_shape = w[0].params.len() == w[1].params.len()
                    && w[0]
                        .params
                        .iter()
                        .zip(w[1].params)
                        .all(|(a, b)| a.ty == b.ty && a.optional == b.optional)
                    && w[0].vararg == w[1].vararg;
                assert!(!same_shape, "two entries with the same parameter shape");
            }
        }
    }

    #[test]
    fn len_takes_anything_and_returns_int64() {
        assert_eq!(result("len", &[Ty::Any]), Some(Ty::Int64));
        assert_eq!(result("len", &[Ty::Str]), Some(Ty::Int64));
        assert_eq!(call("len", &[]), None);
    }

    #[test]
    fn isinstance_returns_bool() {
        assert_eq!(result("isinstance", &[Ty::Any, Ty::Any]), Some(Ty::Bool));
        assert_eq!(result("isinstance", &[Ty::Str, Ty::List]), Some(Ty::Bool));
    }

    #[test]
    fn int_refines_by_argument_and_overflows_to_bigint() {
        assert_eq!(result("int", &[]), Some(Ty::Int64));
        assert_eq!(result("int", &[Ty::Int64]), Some(Ty::Int64));
        assert_eq!(result("int", &[Ty::Bool]), Some(Ty::Int64));
        assert_eq!(result("int", &[Ty::Str]), Some(Ty::BigInt));
        assert_eq!(result("int", &[Ty::Any]), Some(Ty::BigInt));
        assert_eq!(result("int", &[Ty::Str, Ty::Int64]), Some(Ty::BigInt));
        assert_eq!(call("int", &[Ty::Int64, Ty::Int64]), None);
    }

    #[test]
    fn abs_overflows_only_on_the_int_path() {
        let int = call("abs", &[Ty::Int64]).unwrap();
        assert_eq!(int.result, Ty::Int64);
        assert!(int.may_overflow);
        let float = call("abs", &[Ty::Float64]).unwrap();
        assert_eq!(float.result, Ty::Float64);
        assert!(!float.may_overflow);
        assert_eq!(result("abs", &[Ty::Any]), Some(Ty::Any));
    }

    #[test]
    fn math_floor_and_ceil_may_overflow() {
        for name in ["math.ceil", "math.floor", "math.trunc"] {
            let c = call(name, &[Ty::Float64]).unwrap();
            assert_eq!(c.result, Ty::Int64, "{name}");
            assert!(c.may_overflow, "{name}");
        }
        let sqrt = call("math.sqrt", &[Ty::Float64]).unwrap();
        assert_eq!(sqrt.result, Ty::Float64);
        assert!(!sqrt.may_overflow);
    }

    #[test]
    fn constructors_and_iters() {
        assert_eq!(result("str", &[Ty::Any]), Some(Ty::Str));
        assert_eq!(result("float", &[]), Some(Ty::Float64));
        assert_eq!(result("list", &[Ty::Any]), Some(Ty::List));
        assert_eq!(result("dict", &[]), Some(Ty::Dict));
        assert_eq!(result("sorted", &[Ty::Any]), Some(Ty::List));
        // Iterator and range objects have no IR lattice point yet.
        assert_eq!(result("range", &[Ty::Int64]), Some(Ty::Any));
        assert_eq!(result("enumerate", &[Ty::Any]), Some(Ty::Any));
        assert_eq!(result("next", &[Ty::Any]), Some(Ty::Any));
        assert_eq!(
            call("range", &[Ty::Int64, Ty::Int64, Ty::Int64, Ty::Int64]),
            None
        );
    }

    #[test]
    fn method_keys_check_the_receiver() {
        assert_eq!(result("list.append", &[Ty::List, Ty::Any]), Some(Ty::None));
        assert_eq!(call("list.append", &[Ty::Dict, Ty::Any]), None);
        assert_eq!(result("list.count", &[Ty::List, Ty::Str]), Some(Ty::Int64));
        assert_eq!(
            result("str.split", &[Ty::Str, Ty::Str, Ty::Int64]),
            Some(Ty::List)
        );
        assert_eq!(
            call("str.split", &[Ty::Str, Ty::Str, Ty::Int64, Ty::Int64]),
            None
        );
        assert_eq!(result("str.encode", &[Ty::Str]), Some(Ty::Bytes));
        assert_eq!(result("dict.copy", &[Ty::Dict]), Some(Ty::Dict));
        assert_eq!(result("set.union", &[Ty::Set, Ty::List]), Some(Ty::Set));
        assert_eq!(
            result("tuple.index", &[Ty::Tuple, Ty::Any]),
            Some(Ty::Int64)
        );
    }

    #[test]
    fn variadic_builtins() {
        assert_eq!(result("print", &[]), Some(Ty::None));
        assert_eq!(result("print", &[Ty::Str, Ty::Int64]), Some(Ty::None));
        assert_eq!(call("min", &[]), None);
        assert_eq!(result("min", &[Ty::Int64]), Some(Ty::Any));
        assert_eq!(result("zip", &[Ty::List, Ty::Str]), Some(Ty::Any));
    }

    #[test]
    fn singletons_carry_their_exact_type() {
        assert_eq!(singleton("None"), Some(Ty::None));
        assert_eq!(singleton("True"), Some(Ty::Bool));
        assert_eq!(singleton("False"), Some(Ty::Bool));
        assert_eq!(singleton("__debug__"), Some(Ty::Bool));
        assert_eq!(singleton("math.pi"), Some(Ty::Float64));
        assert_eq!(singleton("math.inf"), Some(Ty::Float64));
        assert_eq!(singleton("NotImplemented"), Some(Ty::Any));
        assert_eq!(singleton("Ellipsis"), Some(Ty::Any));
        assert_eq!(singleton("math.sqrt"), None);
    }

    #[test]
    fn ty_converts_to_the_ir_type() {
        assert_eq!(Ty::Any.to_type(), Type::Top);
        assert_eq!(Ty::Int64.to_type(), Type::Int64);
        assert_eq!(Ty::BigInt.to_type(), Type::BigInt);
        assert_eq!(Ty::None.to_type(), Type::None);
        // No unknown-element tuple point exists in the IR.
        assert_eq!(Ty::Tuple.to_type(), Type::Top);
        assert_eq!(Ty::Bottom.to_type(), Type::Bottom);
    }
}

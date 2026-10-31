//! The object-model seam the IR interpreter runs against (spec sections 5,
//! 8 and 10).
//!
//! Build step 1's runtime (`coil-rt` -> `python312.dll`) exports the
//! `coil_rt_*` operations compiled code calls for everything dynamic:
//! `getattr`, binary operators, calls, iteration, exceptions, the thread
//! state's current exception and `exc_info` stack. The interpreter is
//! generic over [`ObjectModel`] so the same executor serves that runtime
//! (dynamic `exec`/`eval`, spec section 6 tier 3) and the test model in
//! `interp/testmodel.rs` the behavioural corpus runs on.
//!
//! The trait is shaped by the dynamic instructions of `ir.rs` and by the
//! object model of sections 8 and 10, not by either implementation's
//! convenience: values are an associated type (a `PyObject*` in the runtime,
//! a reference-counted enum in the test model); operations that can run
//! Python code take the interpreter back (`vm`) because attribute lookups,
//! calls and comparisons re-enter compiled code.
//!
//! Method map, in trait order:
//!
//! | Trait method | `coil_rt_*`-shaped operation | Used by |
//! | --- | --- | --- |
//! | `none` .. `tuple` | the constant constructors (`PyFloat_FromDouble`, `PyTuple_Pack`, ...) | `Const`, `Box` |
//! | `is_none`, `is_same` | identity tests (`x is None`) | `Compare(Is)`, `Raise`, `FinallyExit` |
//! | `unbox_int`/`unbox_float`/`unbox_bool` | guarded unbox (`PyLong_AsLongAndOverflow`, `PyFloat_AsDouble`) | `Unbox` |
//! | `truth` | `PyObject_IsTrue` | `Truth`, `Branch` |
//! | `binop`, `unary`, `compare`, `contains` | `coil_rt_binop`, `PyNumber_*`, `PyObject_RichCompare` | `BinOp`, `UnaryOp`, `Compare`, `Contains` |
//! | `getattr`, `setattr`, `delattr` | `coil_rt_getattr`, `coil_rt_setattr`, `coil_rt_delattr` | `GetAttr`, `SetAttr`, `DelAttr`, `CallMethod` |
//! | `getitem`, `setitem`, `delitem` | `coil_rt_getitem`, `PyObject_SetItem`, `PyObject_DelItem` | `GetItem`, `SetItem`, `DelItem` |
//! | `get_iter`, `iter_next`, `iter_send`, `iter_throw` | `PyObject_GetIter`, `coil_rt_iternext`, send/throw | `GetIter`, `Next`, `YieldFrom`, `Await` |
//! | `await_iter` | the awaitable protocol (`__await__`) | `Await` |
//! | `call`, `call_ex` | `coil_rt_call` (vectorcall), `CALL_FUNCTION_EX` shape | `Call`, `CallEx`, `Raise` (class instantiation) |
//! | `len` | `PyObject_Size` | `Len` |
//! | `format` | `PyObject_Format` | `Format` |
//! | `build_list` .. `build_slice` | container construction | `Build*` |
//! | `unpack` | `unpack` with a starred target | `Unpack` |
//! | `import`, `star_names` | `coil_rt_import` (`__import__`), `IMPORT_STAR` | `Import`, `ImportStar` |
//! | `new_dict`, `dict_get`, `dict_set`, `dict_get_val` | module-globals and namespace access | `LoadGlobal`, `LoadName`, `MakeClass` |
//! | `builtin` | builtins-module lookup | `LoadGlobal`, `LoadName` |
//! | `make_function`, `make_class` | function and class object creation | `MakeFunction`, `MakeClass` |
//! | `new_cell`, `cell_get`, `cell_set`, `is_cell` | closure cells | `MakeCell`, `LoadCell`, `StoreCell`, `MakeClass` |
//! | `tuple_items`, `function_flags` | introspection the executor needs | argument binding |
//! | `error`, `stop_iteration` | `PyErr_SetString`-shaped construction | every raise site in the executor |
//! | `is_class`, `is_exception_instance`, `exc_matches`, `set_exc_context`, `set_exc_cause` | exception predicates and chaining | `CatchBegin`, `Raise` |
//! | `catch_star_split`, `prep_reraise_star` | `exception_group_match`, `_PyExc_PrepReraiseStar` | `CatchStarBegin`, `PrepReraiseStar` |
//! | `add_traceback`, `format_uncaught` | `PyTraceBack` handling | frame exit, the runner |
//! | `alloc_instance`, `field_get`, `field_set`, `type_matches` | shape-based instance operations | `AllocInst`, `StackAlloc`, `LoadField`, `StoreField`, `TypeGuard` |

use crate::ir::{BinOp, CmpOp, FnFlags, UnaryOp};
use crate::types::{FnId, Type};

use super::Interp;

/// Why a guarded unbox failed: the value is out of range for the target
/// scalar (`OverflowError`) or not of the right type at all (`TypeError`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnboxError {
    Overflow,
    Type,
}

/// The built-in exception kinds the executor itself raises; the model maps
/// each to its class. Kept as an enum (not a string) so an unknown kind is a
/// compile error, and the runtime can use its preallocated `PyExc_*` slots.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ErrorTag {
    TypeError,
    ValueError,
    NameError,
    UnboundLocalError,
    AttributeError,
    KeyError,
    IndexError,
    StopIteration,
    ZeroDivisionError,
    OverflowError,
    RuntimeError,
    RecursionError,
    GeneratorExit,
    AssertionError,
    NotImplementedError,
    ImportError,
    ModuleNotFoundError,
}

/// An IR-backed function: everything a later call needs to bind arguments
/// and resume the frame. `defaults`/`kwdefaults` are the `None` constant or
/// the tuple/dict value `MakeFunction` received.
#[derive(Clone)]
pub struct IrFunction<Val> {
    pub func: FnId,
    pub cells: Vec<Val>,
    pub defaults: Val,
    pub kwdefaults: Val,
}

/// The operations the dynamic instructions need, in `ir.rs` terms. Every
/// method that can run Python code takes the interpreter (`vm`), which
/// executes IR frames on the model's behalf. The model hands `vm` back
/// its own type, so the trait is `Sized` (it is never used as a trait
/// object; the interpreter owns the model by value).
pub trait ObjectModel: Sized {
    type Val: Clone;

    // ---- constants ------------------------------------------------------

    fn none(&self) -> Self::Val;
    fn bool(&self, v: bool) -> Self::Val;
    fn int(&self, v: i64) -> Self::Val;
    /// A decimal literal, `-?` digits, no separators; arbitrary precision.
    fn bigint(&self, decimal: &str) -> Self::Val;
    fn float(&self, v: f64) -> Self::Val;
    fn str(&self, v: &str) -> Self::Val;
    fn bytes(&self, v: &[u8]) -> Self::Val;
    fn ellipsis(&self) -> Self::Val;
    fn tuple(&self, items: Vec<Self::Val>) -> Self::Val;

    // ---- identity -------------------------------------------------------

    fn is_none(&self, v: &Self::Val) -> bool;
    /// `a is b`: pointer identity, with the model's singleton rules.
    fn is_same(&self, a: &Self::Val, b: &Self::Val) -> bool;

    // ---- guarded unboxing -------------------------------------------------

    fn unbox_int(&self, v: &Self::Val) -> Result<i64, UnboxError>;
    fn unbox_float(&self, v: &Self::Val) -> Result<f64, UnboxError>;
    fn unbox_bool(&self, v: &Self::Val) -> Result<bool, UnboxError>;

    // ---- dynamic operations ------------------------------------------------

    fn truth(&mut self, vm: &mut Interp<'_, Self>, v: &Self::Val) -> Result<bool, Self::Val>;
    fn binop(
        &mut self,
        vm: &mut Interp<'_, Self>,
        op: BinOp,
        lhs: &Self::Val,
        rhs: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    fn unary(
        &mut self,
        vm: &mut Interp<'_, Self>,
        op: UnaryOp,
        v: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    /// Rich comparison; the result is the `__eq__`-family result value
    /// (a bool for every builtin), not necessarily a bool.
    fn compare(
        &mut self,
        vm: &mut Interp<'_, Self>,
        op: CmpOp,
        lhs: &Self::Val,
        rhs: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    fn contains(
        &mut self,
        vm: &mut Interp<'_, Self>,
        haystack: &Self::Val,
        needle: &Self::Val,
    ) -> Result<bool, Self::Val>;
    fn getattr(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
        name: &str,
    ) -> Result<Self::Val, Self::Val>;
    fn setattr(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
        name: &str,
        value: &Self::Val,
    ) -> Result<(), Self::Val>;
    fn delattr(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
        name: &str,
    ) -> Result<(), Self::Val>;
    fn getitem(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
        key: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    fn setitem(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
        key: &Self::Val,
        value: &Self::Val,
    ) -> Result<(), Self::Val>;
    fn delitem(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
        key: &Self::Val,
    ) -> Result<(), Self::Val>;
    fn get_iter(
        &mut self,
        vm: &mut Interp<'_, Self>,
        obj: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    /// One iteration step; exhaustion is the model raising `StopIteration`.
    fn iter_next(
        &mut self,
        vm: &mut Interp<'_, Self>,
        iter: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    /// `iter.send(value)` with `None` for the first `next()`: the `YieldFrom`
    /// delegation step.
    fn iter_send(
        &mut self,
        vm: &mut Interp<'_, Self>,
        iter: &Self::Val,
        value: Option<Self::Val>,
    ) -> Result<Self::Val, Self::Val>;
    /// `iter.throw(exc)` at a suspension point.
    fn iter_throw(
        &mut self,
        vm: &mut Interp<'_, Self>,
        iter: &Self::Val,
        exc: Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    /// The iterator an `await` delegates to (`__await__`).
    fn await_iter(
        &mut self,
        vm: &mut Interp<'_, Self>,
        v: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    fn call(
        &mut self,
        vm: &mut Interp<'_, Self>,
        callee: &Self::Val,
        args: &[Self::Val],
        kwargs: &[(&str, Self::Val)],
    ) -> Result<Self::Val, Self::Val>;
    /// The `CallEx` shape: `args` a tuple, `kwargs` a dict or the `None`
    /// constant.
    fn call_ex(
        &mut self,
        vm: &mut Interp<'_, Self>,
        callee: &Self::Val,
        args: &Self::Val,
        kwargs: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    fn len(&mut self, vm: &mut Interp<'_, Self>, v: &Self::Val) -> Result<i64, Self::Val>;
    fn format(
        &mut self,
        vm: &mut Interp<'_, Self>,
        value: &Self::Val,
        spec: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;

    // ---- container construction -------------------------------------------

    fn build_list(&self, items: Vec<Self::Val>) -> Self::Val;
    fn build_tuple(&self, items: Vec<Self::Val>) -> Self::Val;
    /// Raises on an unhashable key.
    fn build_dict(
        &mut self,
        vm: &mut Interp<'_, Self>,
        items: Vec<(Self::Val, Self::Val)>,
    ) -> Result<Self::Val, Self::Val>;
    /// Raises on an unhashable item.
    fn build_set(
        &mut self,
        vm: &mut Interp<'_, Self>,
        items: Vec<Self::Val>,
    ) -> Result<Self::Val, Self::Val>;
    fn build_str(&self, parts: Vec<Self::Val>) -> Self::Val;
    fn build_slice(
        &self,
        start: Option<Self::Val>,
        stop: Self::Val,
        step: Option<Self::Val>,
    ) -> Self::Val;
    /// Unpack `src` into `n` targets, `star` naming the starred index;
    /// raises `ValueError` on a size mismatch.
    fn unpack(
        &mut self,
        vm: &mut Interp<'_, Self>,
        src: &Self::Val,
        n: u32,
        star: Option<u32>,
    ) -> Result<Vec<Self::Val>, Self::Val>;

    // ---- import -------------------------------------------------------------

    fn import(
        &mut self,
        vm: &mut Interp<'_, Self>,
        name: &str,
        fromlist: &Self::Val,
        level: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    /// The names `from module import *` binds (the model applies `__all__`
    /// and the underscore rule).
    fn star_names(
        &mut self,
        vm: &mut Interp<'_, Self>,
        module: &Self::Val,
    ) -> Result<Vec<(String, Self::Val)>, Self::Val>;

    // ---- namespaces -----------------------------------------------------------

    /// A fresh dict, for module globals, class namespaces and `**kwargs`.
    fn new_dict(&self) -> Self::Val;
    fn dict_get(&self, dict: &Self::Val, key: &str) -> Option<Self::Val>;
    fn dict_get_val(&self, dict: &Self::Val, key: &Self::Val) -> Option<Self::Val>;
    fn dict_set(&mut self, dict: &Self::Val, key: &str, value: Self::Val);
    /// The named builtin, or `None` when no such builtin exists.
    fn builtin(&self, name: &str) -> Option<Self::Val>;

    // ---- functions, classes, cells ----------------------------------------------

    /// A function object over IR `func`, closing over `cells`, with
    /// `defaults`/`kwdefaults` (each the `None` constant or a tuple/dict).
    fn make_function(
        &mut self,
        vm: &mut Interp<'_, Self>,
        func: FnId,
        cells: Vec<Self::Val>,
        defaults: Self::Val,
        kwdefaults: Self::Val,
    ) -> Self::Val;
    /// Runs the class-body function against a fresh namespace dict and
    /// creates the class (C3 MRO, `__init_subclass__`, `__set_name__`,
    /// the `__classcell__` binding), as `__build_class__` does.
    fn make_class(
        &mut self,
        vm: &mut Interp<'_, Self>,
        name: &str,
        body: &Self::Val,
        bases: &[Self::Val],
        kwds: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;

    fn new_cell(&self, initial: Option<Self::Val>) -> Self::Val;
    fn cell_get(&self, cell: &Self::Val) -> Option<Self::Val>;
    fn cell_set(&mut self, cell: &Self::Val, value: Self::Val);
    fn is_cell(&self, v: &Self::Val) -> bool;

    fn tuple_items(&self, v: &Self::Val) -> Option<Vec<Self::Val>>;

    // ---- exceptions --------------------------------------------------------------

    /// A fresh exception instance of the named builtin kind.
    fn error(&self, tag: ErrorTag, msg: &str) -> Self::Val;
    /// `StopIteration` carrying `value` (the `.value` attribute).
    fn stop_iteration(&self, value: Self::Val) -> Self::Val;
    /// Whether `v` is a class object (callable to instantiate).
    fn is_class(&self, v: &Self::Val) -> bool;
    /// Whether `v` is a `BaseException` instance.
    fn is_exception_instance(&self, v: &Self::Val) -> bool;
    /// `PyErr_GivenExceptionMatches`: `exc` (an instance or a class)
    /// against `types` (a class or tuple of classes).
    fn exc_matches(&self, exc: &Self::Val, types: &Self::Val) -> bool;
    /// Implicit chaining: `__context__`.
    fn set_exc_context(&mut self, exc: &Self::Val, context: Option<Self::Val>);
    /// `raise ... from ...`: `None` is `from None` (suppress), `Some(v)` sets
    /// `__cause__` and `__suppress_context__`.
    fn set_exc_cause(&mut self, exc: &Self::Val, cause: Option<Self::Val>);
    /// The `except*` split of `src` against `types`
    /// (`exception_group_match`): `(matched, subgroup-or-None,
    /// rest-or-None)`.
    fn catch_star_split(
        &mut self,
        vm: &mut Interp<'_, Self>,
        src: &Self::Val,
        types: &Self::Val,
    ) -> Result<(bool, Self::Val, Self::Val), Self::Val>;
    /// `_PyExc_PrepReraiseStar`: the exception to re-raise after the
    /// `except*` handlers, or the `None` constant when all was handled.
    fn prep_reraise_star(
        &mut self,
        vm: &mut Interp<'_, Self>,
        orig: &Self::Val,
        excs: &Self::Val,
    ) -> Result<Self::Val, Self::Val>;
    fn add_traceback(&mut self, exc: &Self::Val, file: &str, line: u32, qualname: &str);
    /// The CPython-style report of an uncaught exception: the traceback and
    /// the final `Type: message` line.
    fn format_uncaught(&self, exc: &Self::Val) -> String;

    // ---- shape-based instances (the optimiser's static instructions) ----------

    fn alloc_instance(
        &mut self,
        vm: &mut Interp<'_, Self>,
        cls: &Self::Val,
        slots: u32,
    ) -> Result<Self::Val, Self::Val>;
    fn field_get(&self, obj: &Self::Val, slot: u32) -> Result<Self::Val, Self::Val>;
    fn field_set(&mut self, obj: &Self::Val, slot: u32, value: &Self::Val)
    -> Result<(), Self::Val>;

    // ---- type tests --------------------------------------------------------------

    /// Whether `v` inhabits the static type `ty` (the `TypeGuard` check).
    /// `Type::Inst`/`Type::Class` resolve through `vm`'s class registry.
    fn type_matches(
        &self,
        vm: &Interp<'_, Self>,
        v: &Self::Val,
        ty: &Type,
    ) -> Result<bool, UnboxError>;
}

/// The generator/coroutine flags of an IR function, for `make_function`'s
/// caller: a call to a generator function produces a suspended frame, not a
/// result.
pub fn function_flags<M: ObjectModel>(vm: &Interp<'_, M>, func: FnId) -> FnFlags {
    vm.flags_of(func)
}

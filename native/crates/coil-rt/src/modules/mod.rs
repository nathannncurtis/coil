//! The native built-in modules of the Rust column (spec section 10.6):
//! `_struct`, `math`, `cmath`, `_collections`, `itertools`, `array`,
//! `binascii`, `zlib` and `audioop`, each a C-API-shaped surface over
//! the exact layouts and the shared helpers below.
//!
//! Module objects (the `PyModuleDef` machinery of section 10.4) need
//! str-keyed dicts, which arrive with the unicode task; until then the
//! modules expose their functions the way the rest of this runtime
//! does, as C API entry points, and the method tables beside them
//! (`PyMethodDef` arrays per module) are what module creation will bind.
//! Keyword arguments travel the same seam: no str objects exist to be
//! keys yet, so a non-empty keyword dict raises exactly CPython's
//! "keywords must be strings", the answer it gives non-str keys.

use coil_capi_sys::PyObject;

use crate::object::header;
use crate::object::singletons;
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::long;

/// A module's function table. The raw pointers inside `PyMethodDef`
/// make it non-`Sync` by construction, but every entry points at static
/// strings and functions, so the tables can live in statics.
pub struct MethodTable(pub &'static [coil_capi_sys::PyMethodDef]);
// SAFETY: the name strings and function pointers are process-static.
unsafe impl Sync for MethodTable {}

pub mod _collections;
pub mod _struct;
pub mod array;
pub mod audioop;
pub mod binascii;
pub mod cmath;
pub mod itertools;
pub mod math;
pub mod zlib;

// ------------------------------------------------------------ call helper

/// Call `callable` with `args` positionally and null keywords, through
/// the C function dispatch or the type's `tp_call`.
///
/// # Safety
///
/// `callable` must be a valid object; `args` valid objects.
pub unsafe fn call(callable: *mut PyObject, args: &[*mut PyObject]) -> *mut PyObject {
    // SAFETY: the header read is within the object.
    if unsafe { crate::types::pycfunction::is_cfunction(callable) } {
        // SAFETY: the vectorcall contract holds on the arguments.
        return unsafe {
            crate::types::pycfunction::vectorcall(
                callable,
                args.as_ptr(),
                args.len() as isize,
                core::ptr::null_mut(),
            )
        };
    }
    let tuple = crate::types::tuple::from_array(args);
    // SAFETY: the slot read is within the type object.
    let call_slot = unsafe { (*header::type_of(callable)).tp_call };
    let Some(call_slot) = call_slot else {
        // SAFETY: the name read is within the type object.
        let name = unsafe { crate::util::cstr((*header::type_of(callable)).tp_name) };
        tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not callable").as_bytes(),
        );
        return core::ptr::null_mut();
    };
    // SAFETY: the slot takes the callable and the argument tuple.
    let result = unsafe { call_slot(callable, tuple, core::ptr::null_mut()) };
    // The tuple was built from borrowed arguments, so the block drops
    // without touching the items.
    // SAFETY: the block was allocated above.
    unsafe { crate::alloc::free_block(tuple.cast()) };
    result
}

/// `PyObject_CallNoArgs` over the shared path.
///
/// # Safety
///
/// `callable` must be a valid object.
pub unsafe fn call_no_args(callable: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `call`.
    unsafe { call(callable, &[]) }
}

/// `PyObject_CallOneArg` over the shared path.
///
/// # Safety
///
/// `callable` and `arg` must be valid objects.
pub unsafe fn call_one_arg(callable: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `call`.
    unsafe { call(callable, &[arg]) }
}

// --------------------------------------------------------------- coercion

/// `PyLong_CheckExact`: int without subtypes (bool is not exact).
///
/// # Safety
///
/// `op` must be a valid object.
pub(crate) unsafe fn is_exact_long(op: *const PyObject) -> bool {
    // SAFETY: the header read is within the object.
    unsafe { header::type_of(op) == &raw mut crate::types::long::PyLong_Type }
}

/// `PyFloat_CheckExact`.
///
/// # Safety
///
/// `op` must be a valid object.
pub(crate) unsafe fn is_exact_float(op: *const PyObject) -> bool {
    // SAFETY: the header read is within the object.
    unsafe { header::type_of(op) == &raw mut crate::types::float_::PyFloat_Type }
}

/// `PyObject_IsTrue`'s protocol: `nb_bool`, then `mp_length`, then
/// `sq_length`, otherwise true. Returns -1 only when a slot raised.
///
/// # Safety
///
/// `obj` must be a valid object.
pub(crate) unsafe fn object_is_true(obj: *mut PyObject) -> core::ffi::c_int {
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(obj) };
    // SAFETY: the slots, when present, take the object.
    unsafe {
        if let Some(table) = (*ty).tp_as_number.as_ref()
            && let Some(nb_bool) = table.nb_bool
        {
            let res = nb_bool(obj);
            return if res > 0 { 1 } else { res as core::ffi::c_int };
        }
        if let Some(table) = (*ty).tp_as_mapping.as_ref()
            && let Some(mp_length) = table.mp_length
        {
            let res = mp_length(obj);
            return if res > 0 { 1 } else { res as core::ffi::c_int };
        }
        if let Some(table) = (*ty).tp_as_sequence.as_ref()
            && let Some(sq_length) = table.sq_length
        {
            let res = sq_length(obj);
            return if res > 0 { 1 } else { res as core::ffi::c_int };
        }
    }
    1
}

/// `PyTuple_Check`.
///
/// # Safety
///
/// `op` must be a valid object.
pub(crate) unsafe fn number_index(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: the layout check reads the header.
    if unsafe { long::is_long(obj) } {
        // SAFETY: retaining the int keeps the caller's reference intact.
        unsafe { exceptions::retain_object(obj) };
        return obj;
    }
    // SAFETY: the slot read is within the type object.
    let ty = unsafe { header::type_of(obj) };
    let nb = unsafe { (*ty).tp_as_number };
    let index_slot = if nb.is_null() {
        None
    } else {
        unsafe { (*nb).nb_index }
    };
    if let Some(index_slot) = index_slot {
        // SAFETY: the slot takes the object.
        let converted = unsafe { index_slot(obj) };
        if converted.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: the layout check reads the header.
        if !unsafe { long::is_long(converted) } {
            let name = unsafe { crate::util::cstr((*ty).tp_name) };
            tstate::set_string(
                exceptions::type_error(),
                format!("__index__ returned non-int (type {name})").as_bytes(),
            );
            return core::ptr::null_mut();
        }
        return converted;
    }
    let name = unsafe { crate::util::cstr((*ty).tp_name) };
    tstate::set_string(
        exceptions::type_error(),
        format!("'{name}' object cannot be interpreted as an integer").as_bytes(),
    );
    core::ptr::null_mut()
}

/// A number binary operation over the number slots
/// (`binary_op`): the left type's slot, then the right's when it
/// answers NotImplemented, else CPython's unsupported-operand message
/// with `symbol` ("+", "*", ...) between the type names.
///
/// # Safety
///
/// `a` and `b` must be valid objects of readied types.
pub(crate) unsafe fn number_binary(
    a: *mut PyObject,
    b: *mut PyObject,
    slot: unsafe fn(*mut coil_capi_sys::PyNumberMethods) -> Option<coil_capi_sys::binaryfunc>,
    symbol: &str,
) -> *mut PyObject {
    // SAFETY: the slot reads are within the type objects.
    unsafe {
        let slot_a = slot_of(a, slot);
        if let Some(f) = slot_a {
            let result = f(a, b);
            if result.is_null() || !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
            // NotImplemented from a slot is the borrowed immortal singleton;
            // the other side gets its chance.
        }
        let slot_b = slot_of(b, slot);
        if let Some(f) = slot_b {
            let result = f(a, b);
            if result.is_null() || !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
        }
        let a_name = crate::util::cstr((*header::type_of(a)).tp_name);
        let b_name = crate::util::cstr((*header::type_of(b)).tp_name);
        tstate::set_string(
            exceptions::type_error(),
            format!("unsupported operand type(s) for {symbol}: '{a_name}' and '{b_name}'")
                .as_bytes(),
        );
        core::ptr::null_mut()
    }
}

/// Read a number slot from an object's type.
unsafe fn slot_of(
    obj: *mut PyObject,
    slot: unsafe fn(*mut coil_capi_sys::PyNumberMethods) -> Option<coil_capi_sys::binaryfunc>,
) -> Option<coil_capi_sys::binaryfunc> {
    // SAFETY: the table read follows the header.
    let nb = unsafe { (*header::type_of(obj)).tp_as_number };
    if nb.is_null() {
        None
    } else {
        // SAFETY: the table belongs to a readied type.
        unsafe { slot(nb) }
    }
}

unsafe fn nb_add(nb: *mut coil_capi_sys::PyNumberMethods) -> Option<coil_capi_sys::binaryfunc> {
    // SAFETY: the caller guarantees the table is valid.
    unsafe { (*nb).nb_add }
}

unsafe fn nb_multiply(
    nb: *mut coil_capi_sys::PyNumberMethods,
) -> Option<coil_capi_sys::binaryfunc> {
    // SAFETY: as `nb_add`.
    unsafe { (*nb).nb_multiply }
}

/// `PyNumber_Add` over the number slots.
///
/// # Safety
///
/// `a` and `b` must be valid objects of readied types.
pub(crate) unsafe fn number_add(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the arguments are valid objects.
    unsafe { number_binary(a, b, nb_add, "+") }
}

/// `PyNumber_Multiply` over the number slots.
///
/// # Safety
///
/// `a` and `b` must be valid objects of readied types.
pub(crate) unsafe fn number_multiply(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the arguments are valid objects.
    unsafe { number_binary(a, b, nb_multiply, "*") }
}

/// The `__index__` protocol as CPython's argument conversion applies it
/// (`PyNumber_Index` reduced to this runtime's types): int and bool pass
/// through, a type with `nb_index` goes through the slot once, anything
/// else is "'X' object cannot be interpreted as an integer".
///
/// # Safety
///
/// `obj` must be a valid object.
pub(crate) unsafe fn reject_keywords(kwds: *mut PyObject) -> Result<(), ()> {
    // SAFETY: the dict-layout read follows the calling machinery's
    // contract on `kwds`.
    if !kwds.is_null() && unsafe { crate::util::dict_used(kwds) } > 0 {
        tstate::set_string(exceptions::type_error(), b"keywords must be strings");
        return Err(());
    }
    Ok(())
}

/// The argument-count error of CPython's positional parsing
/// (`_PyArg_CheckPositional`), with the function's name.
pub(crate) fn positional_error(name: &str, min: usize, max: usize, given: usize) {
    if min == max {
        let plural = if min == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name} expected {min} argument{plural}, got {given}").as_bytes(),
        );
    } else if given < min {
        let plural = if min == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name} expected at least {min} argument{plural}, got {given}").as_bytes(),
        );
    } else {
        let plural = if max == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name} expected at most {max} argument{plural}, got {given}").as_bytes(),
        );
    }
}

/// The count error of the keyword-capable parse path (`_PyArg_Parser`)
/// for too many positionals: required arguments can also arrive by
/// keyword there — impossible in this runtime — so the shortfall side
/// is handled by `missing_argument_error` instead.
pub(crate) fn positional_error_kw(name: &str, min: usize, max: usize, given: usize) {
    if max == 0 {
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes no positional arguments").as_bytes(),
        );
    } else {
        let qualifier = if min < max { "at most" } else { "exactly" };
        let plural = if max == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes {qualifier} {max} positional argument{plural} ({given} given)")
                .as_bytes(),
        );
    }
}

/// `_PyArg_UnpackKeywords`'s two-layer count error for too many
/// positionals: positional overflow past every parameter (keyword-only
/// ones included) reports the total, overflow past the
/// positional-or-keyword parameters reports those.
pub(crate) fn keywords_arity_error(
    name: &str,
    minpos: usize,
    maxpos: usize,
    maxargs: usize,
    given: usize,
) {
    if given > maxargs {
        let plural = if maxargs == 1 { "" } else { "s" };
        tstate::set_string(
            exceptions::type_error(),
            format!("{name}() takes at most {maxargs} argument{plural} ({given} given)").as_bytes(),
        );
    } else {
        positional_error_kw(name, minpos, maxpos, given);
    }
}

/// `_PyArg_Parser`'s message for a missing positional-or-keyword
/// parameter (it could still arrive by name in CPython).
pub(crate) fn missing_argument_error(name: &str, param: &str, pos: usize) {
    tstate::set_string(
        exceptions::type_error(),
        format!("{name}() missing required argument '{param}' (pos {pos})").as_bytes(),
    );
}

// -------------------------------------------------------------- iteration

/// `PyObject_GetIter` for the types with `tp_iter`.
///
/// # Safety
///
/// `obj` must be a valid object of a readied type.
pub(crate) unsafe fn get_iter(obj: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot read is within the type object.
    let iter = unsafe { (*header::type_of(obj)).tp_iter };
    let Some(iter) = iter else {
        let name = unsafe { crate::util::cstr((*header::type_of(obj)).tp_name) };
        tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not iterable").as_bytes(),
        );
        return core::ptr::null_mut();
    };
    // SAFETY: the slot takes the object.
    unsafe { iter(obj) }
}

/// `PyIter_Next`: the next item, None-shaped null at exhaustion, null
/// with the exception set on error. The caller distinguishes by
/// `tstate::occurred()`.
///
/// # Safety
///
/// `iter` must be an iterator object.
pub(crate) unsafe fn iter_next(iter: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot read is within the type object.
    let next = unsafe { (*header::type_of(iter)).tp_iternext };
    let Some(next) = next else {
        let name = unsafe { crate::util::cstr((*header::type_of(iter)).tp_name) };
        tstate::set_string(
            exceptions::type_error(),
            format!("'{name}' object is not an iterator").as_bytes(),
        );
        return core::ptr::null_mut();
    };
    // SAFETY: the slot takes the iterator.
    unsafe { next(iter) }
}

/// Collect an arbitrary iterable into a vector (`PySequence_List`'s
/// role); Err with the exception set.
///
/// # Safety
///
/// `obj` must be a valid object of a readied type.
pub(crate) unsafe fn collect_iterable(obj: *mut PyObject) -> Result<Vec<*mut PyObject>, ()> {
    // SAFETY: as `get_iter`.
    let iter = unsafe { get_iter(obj) };
    if iter.is_null() {
        return Err(());
    }
    let mut items = Vec::new();
    loop {
        // SAFETY: the loop hands `get_iter`'s result to `iter_next`.
        let item = unsafe { iter_next(iter) };
        if item.is_null() {
            if !tstate::occurred().is_null() {
                return Err(());
            }
            return Ok(items);
        }
        items.push(item);
    }
}

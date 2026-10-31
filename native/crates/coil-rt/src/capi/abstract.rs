//! `abstract.h`: the number, sequence, mapping and iterator protocols
//! plus the buffer helpers (spec section 10.1) — `Objects/abstract.c`
//! transcribed over this runtime's protocol-struct slots.

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{
    PYGEN_ERROR, PYGEN_NEXT, PYGEN_RETURN, Py_buffer, PyBUF_SIMPLE, PyObject, PySequenceMethods,
    PyTypeObject, binaryfunc, ternaryfunc,
};

use crate::capi::object::PyObject_GetIter;
use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{Py_EQ as RICH_EQ, PyType_IsSubtype};
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::{bytes, float_, long, unicode};
use crate::util;

// ------------------------------------------------------------ shared helpers

fn truncate(text: &str, limit: usize) -> &str {
    match text.char_indices().nth(limit) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// # Safety
///
/// `object` must be a valid object.
unsafe fn type_name(object: *mut PyObject, limit: usize) -> &'static str {
    // SAFETY: the type name is a valid C string on live types.
    truncate(
        unsafe { util::cstr((*header::type_of(object)).tp_name) },
        limit,
    )
}

/// CPython's `type_error(msg, o)`: `msg` with one `%.200s` for the type.
///
/// # Safety
///
/// `object` must be a valid object.
unsafe fn type_error(message: &str, object: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object's type is valid.
    let name = unsafe { type_name(object, 200) };
    let text = match message.find("%.200s").or_else(|| message.find("%s")) {
        Some(at) => {
            let width = if message[at..].starts_with("%.200s") {
                6
            } else {
                2
            };
            format!("{}{}{}", &message[..at], name, &message[at + width..])
        }
        None => message.to_string(),
    };
    tstate::set_string(exceptions::type_error(), text.as_bytes());
    core::ptr::null_mut()
}

/// CPython's `null_error`.
fn null_error() -> *mut PyObject {
    tstate::set_string(
        exceptions::system_error(),
        b"null argument to internal routine",
    );
    core::ptr::null_mut()
}

/// The slice-object error: the `slice` type this runtime has not built
/// yet, named by the entry points that need one.
fn slice_unavailable() {
    tstate::set_string(
        exceptions::not_implemented_error(),
        b"coil: slice objects are not implemented",
    );
}

// ------------------------------------------------------------- PyNumber

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    // SAFETY: the slot read is within the type object.
    let methods = unsafe { (*header::type_of(o)).tp_as_number.as_ref() };
    let Some(methods) = methods else {
        return 0;
    };
    c_int::from(
        methods.nb_index.is_some() || methods.nb_int.is_some() || methods.nb_float.is_some(),
    )
}

/// CPython's `binary_op1` with the slot chosen by a selector instead of
/// a byte offset.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
// Slot identity is CPython's own protocol test; the slots are this
// DLL's `no_mangle` functions, whose addresses are as fixed as C's.
#[allow(unpredictable_function_pointer_comparisons)]
unsafe fn binary_op1(
    v: *mut PyObject,
    w: *mut PyObject,
    select: fn(&coil_capi_sys::PyNumberMethods) -> Option<binaryfunc>,
) -> *mut PyObject {
    // SAFETY: the slot reads and calls are the operands' types' own.
    unsafe {
        let tyv = header::type_of(v);
        let tyw = header::type_of(w);
        let slotv = (*tyv).tp_as_number.as_ref().and_then(select);
        let mut slotw = if !core::ptr::eq(tyw, tyv) {
            (*tyw).tp_as_number.as_ref().and_then(select)
        } else {
            None
        };
        if slotw == slotv {
            slotw = None;
        }

        if let Some(f) = slotv {
            if let Some(g) = slotw
                && PyType_IsSubtype(tyw, tyv) != 0
            {
                let x = g(v, w);
                if !core::ptr::eq(x, singletons::not_implemented()) {
                    return x;
                }
                exceptions::release_object(x);
            }
            let x = f(v, w);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }
        if let Some(g) = slotw {
            let x = g(v, w);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }
        exceptions::retain_object(singletons::not_implemented());
        singletons::not_implemented()
    }
}

/// CPython's `binop_type_error`.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
unsafe fn binop_type_error(v: *mut PyObject, w: *mut PyObject, op_name: &str) -> *mut PyObject {
    // SAFETY: both operands are valid objects per the contract.
    let (name_v, name_w) = unsafe { (type_name(v, 100), type_name(w, 100)) };
    tstate::set_string(
        exceptions::type_error(),
        format!(
            "unsupported operand type(s) for {}: '{}' and '{}'",
            truncate(op_name, 100),
            name_v,
            name_w
        )
        .as_bytes(),
    );
    core::ptr::null_mut()
}

/// CPython's `binary_op`.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
unsafe fn binary_op(
    v: *mut PyObject,
    w: *mut PyObject,
    select: fn(&coil_capi_sys::PyNumberMethods) -> Option<binaryfunc>,
    op_name: &str,
) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    let result = unsafe { binary_op1(v, w, select) };
    if core::ptr::eq(result, singletons::not_implemented()) {
        unsafe { exceptions::release_object(result) };
        return unsafe { binop_type_error(v, w, op_name) };
    }
    result
}

/// CPython's `ternary_op`.
///
/// # Safety
///
/// `v`, `w` and `z` must be valid objects.
#[allow(unpredictable_function_pointer_comparisons)]
unsafe fn ternary_op(
    v: *mut PyObject,
    w: *mut PyObject,
    z: *mut PyObject,
    select: fn(&coil_capi_sys::PyNumberMethods) -> Option<ternaryfunc>,
    op_name: &str,
) -> *mut PyObject {
    // SAFETY: the slot reads and calls are the operands' types' own.
    unsafe {
        let tyv = header::type_of(v);
        let tyw = header::type_of(w);
        let slotv = (*tyv).tp_as_number.as_ref().and_then(select);
        let mut slotw = if !core::ptr::eq(tyw, tyv) {
            (*tyw).tp_as_number.as_ref().and_then(select)
        } else {
            None
        };
        if slotw == slotv {
            slotw = None;
        }

        if let Some(f) = slotv {
            if let Some(g) = slotw
                && PyType_IsSubtype(tyw, tyv) != 0
            {
                let x = g(v, w, z);
                if !core::ptr::eq(x, singletons::not_implemented()) {
                    return x;
                }
                exceptions::release_object(x);
            }
            let x = f(v, w, z);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }
        if let Some(g) = slotw {
            let x = g(v, w, z);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }

        let tyz = header::type_of(z);
        let mut slotz = (*tyz).tp_as_number.as_ref().and_then(select);
        if slotz == slotv || slotz == slotw {
            slotz = None;
        }
        if let Some(h) = slotz {
            let x = h(v, w, z);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }

        let not_implemented = singletons::none();
        if core::ptr::eq(z, not_implemented) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "unsupported operand type(s) for {}: '{}' and '{}'",
                    truncate(op_name, 100),
                    type_name(v, 100),
                    type_name(w, 100)
                )
                .as_bytes(),
            );
        } else {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "unsupported operand type(s) for {}: '{}', '{}', '{}'",
                    truncate(op_name, 100),
                    type_name(v, 100),
                    type_name(w, 100),
                    type_name(z, 100)
                )
                .as_bytes(),
            );
        }
        core::ptr::null_mut()
    }
}

macro_rules! binary_number_func {
    ($func:ident, $slot:ident, $op_name:expr) => {
        /// See the CPython 3.12 contract for this entry point.
        ///
        /// # Safety
        ///
        /// `v` and `w` must be valid objects.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $func(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
            // SAFETY: the operation follows the C contract.
            unsafe { binary_op(v, w, |m| m.$slot, $op_name) }
        }
    };
}

binary_number_func!(PyNumber_Or, nb_or, "|");
binary_number_func!(PyNumber_Xor, nb_xor, "^");
binary_number_func!(PyNumber_And, nb_and, "&");
binary_number_func!(PyNumber_Lshift, nb_lshift, "<<");
binary_number_func!(PyNumber_Rshift, nb_rshift, ">>");
binary_number_func!(PyNumber_Subtract, nb_subtract, "-");
binary_number_func!(PyNumber_Divmod, nb_divmod, "divmod()");
binary_number_func!(PyNumber_MatrixMultiply, nb_matrix_multiply, "@");
binary_number_func!(PyNumber_FloorDivide, nb_floor_divide, "//");
binary_number_func!(PyNumber_TrueDivide, nb_true_divide, "/");
binary_number_func!(PyNumber_Remainder, nb_remainder, "%");

/// See the CPython 3.12 contract for this entry point: `+` falls back
/// to `sq_concat`.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Add(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    unsafe {
        let result = binary_op1(v, w, |m| m.nb_add);
        if !core::ptr::eq(result, singletons::not_implemented()) {
            return result;
        }
        exceptions::release_object(result);
        if let Some(concat) = (*header::type_of(v))
            .tp_as_sequence
            .as_ref()
            .and_then(|m| m.sq_concat)
        {
            return concat(v, w);
        }
        binop_type_error(v, w, "+")
    }
}

/// CPython's `sequence_repeat`: the repeat slot with `n` reduced to a
/// C count first.
///
/// # Safety
///
/// `seq` and `n` must be valid objects.
unsafe fn sequence_repeat(
    repeatfunc: coil_capi_sys::ssizeargfunc,
    seq: *mut PyObject,
    n: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the index conversion follows the C contract.
    unsafe {
        if index_check(n) == 0 {
            return type_error("can't multiply sequence by non-int of type '%s'", n);
        }
        let count = PyNumber_AsSsize_t(n, exceptions::overflow_error());
        if count == -1 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        repeatfunc(seq, count)
    }
}

/// See the CPython 3.12 contract for this entry point: `*` falls back
/// to `sq_repeat` on either side.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Multiply(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    unsafe {
        let result = binary_op1(v, w, |m| m.nb_multiply);
        if !core::ptr::eq(result, singletons::not_implemented()) {
            return result;
        }
        exceptions::release_object(result);
        let mv = (*header::type_of(v)).tp_as_sequence.as_ref();
        let mw = (*header::type_of(w)).tp_as_sequence.as_ref();
        if let Some(repeat) = mv.and_then(|m| m.sq_repeat) {
            return sequence_repeat(repeat, v, w);
        }
        if let Some(repeat) = mw.and_then(|m| m.sq_repeat) {
            return sequence_repeat(repeat, w, v);
        }
        binop_type_error(v, w, "*")
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v`, `w` and `z` must be valid objects; `z` null means `None`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Power(
    v: *mut PyObject,
    w: *mut PyObject,
    z: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    unsafe { ternary_op(v, w, z, |m| m.nb_power, "** or pow()") }
}

/// CPython's `binary_iop1`.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[allow(unpredictable_function_pointer_comparisons)]
unsafe fn binary_iop1(
    v: *mut PyObject,
    w: *mut PyObject,
    iselect: fn(&coil_capi_sys::PyNumberMethods) -> Option<binaryfunc>,
    select: fn(&coil_capi_sys::PyNumberMethods) -> Option<binaryfunc>,
) -> *mut PyObject {
    // SAFETY: the slot reads and calls are the operand's type's own.
    unsafe {
        if let Some(f) = (*header::type_of(v))
            .tp_as_number
            .as_ref()
            .and_then(iselect)
        {
            let x = f(v, w);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }
        binary_op1(v, w, select)
    }
}

/// CPython's `binary_iop`.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
unsafe fn binary_iop(
    v: *mut PyObject,
    w: *mut PyObject,
    iselect: fn(&coil_capi_sys::PyNumberMethods) -> Option<binaryfunc>,
    select: fn(&coil_capi_sys::PyNumberMethods) -> Option<binaryfunc>,
    op_name: &str,
) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    let result = unsafe { binary_iop1(v, w, iselect, select) };
    if core::ptr::eq(result, singletons::not_implemented()) {
        unsafe { exceptions::release_object(result) };
        return unsafe { binop_type_error(v, w, op_name) };
    }
    result
}

macro_rules! inplace_number_func {
    ($func:ident, $islot:ident, $slot:ident, $op_name:expr) => {
        /// See the CPython 3.12 contract for this entry point.
        ///
        /// # Safety
        ///
        /// `v` and `w` must be valid objects.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $func(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
            // SAFETY: the operation follows the C contract.
            unsafe { binary_iop(v, w, |m| m.$islot, |m| m.$slot, $op_name) }
        }
    };
}

inplace_number_func!(PyNumber_InPlaceOr, nb_inplace_or, nb_or, "|=");
inplace_number_func!(PyNumber_InPlaceXor, nb_inplace_xor, nb_xor, "^=");
inplace_number_func!(PyNumber_InPlaceAnd, nb_inplace_and, nb_and, "&=");
inplace_number_func!(PyNumber_InPlaceLshift, nb_inplace_lshift, nb_lshift, "<<=");
inplace_number_func!(PyNumber_InPlaceRshift, nb_inplace_rshift, nb_rshift, ">>=");
inplace_number_func!(
    PyNumber_InPlaceSubtract,
    nb_inplace_subtract,
    nb_subtract,
    "-="
);
inplace_number_func!(
    PyNumber_InPlaceMatrixMultiply,
    nb_inplace_matrix_multiply,
    nb_matrix_multiply,
    "@="
);
inplace_number_func!(
    PyNumber_InPlaceFloorDivide,
    nb_inplace_floor_divide,
    nb_floor_divide,
    "//="
);
inplace_number_func!(
    PyNumber_InPlaceTrueDivide,
    nb_inplace_true_divide,
    nb_true_divide,
    "/="
);
inplace_number_func!(
    PyNumber_InPlaceRemainder,
    nb_inplace_remainder,
    nb_remainder,
    "%="
);

/// See the CPython 3.12 contract for this entry point: `+=` falls back
/// to the sequence concat slots.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_InPlaceAdd(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    unsafe {
        let result = binary_iop1(v, w, |m| m.nb_inplace_add, |m| m.nb_add);
        if !core::ptr::eq(result, singletons::not_implemented()) {
            return result;
        }
        exceptions::release_object(result);
        if let Some(methods) = (*header::type_of(v)).tp_as_sequence.as_ref() {
            let func = methods.sq_inplace_concat.or(methods.sq_concat);
            if let Some(func) = func {
                return func(v, w);
            }
        }
        binop_type_error(v, w, "+=")
    }
}

/// See the CPython 3.12 contract for this entry point: `*=` falls back
/// to the sequence repeat slots.
///
/// # Safety
///
/// `v` and `w` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_InPlaceMultiply(
    v: *mut PyObject,
    w: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    unsafe {
        let result = binary_iop1(v, w, |m| m.nb_inplace_multiply, |m| m.nb_multiply);
        if !core::ptr::eq(result, singletons::not_implemented()) {
            return result;
        }
        exceptions::release_object(result);
        let mv = (*header::type_of(v)).tp_as_sequence.as_ref();
        let mw = (*header::type_of(w)).tp_as_sequence.as_ref();
        if let Some(methods) = mv {
            let f = methods.sq_inplace_repeat.or(methods.sq_repeat);
            if let Some(f) = f {
                return sequence_repeat(f, v, w);
            }
        } else if let Some(repeat) = mw.and_then(|m| m.sq_repeat) {
            // The right operand is not mutated, so only the plain
            // repeat slot applies.
            return sequence_repeat(repeat, w, v);
        }
        binop_type_error(v, w, "*=")
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v`, `w` and `z` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_InPlacePower(
    v: *mut PyObject,
    w: *mut PyObject,
    z: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the operation follows the C contract.
    unsafe {
        if let Some(f) = (*header::type_of(v))
            .tp_as_number
            .as_ref()
            .and_then(|m| m.nb_inplace_power)
        {
            let x = f(v, w, z);
            if !core::ptr::eq(x, singletons::not_implemented()) {
                return x;
            }
            exceptions::release_object(x);
        }
        ternary_op(v, w, z, |m| m.nb_power, "**=")
    }
}

macro_rules! unary_number_func {
    ($func:ident, $slot:ident, $message:expr) => {
        /// See the CPython 3.12 contract for this entry point.
        ///
        /// # Safety
        ///
        /// `o` must be a valid object.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $func(o: *mut PyObject) -> *mut PyObject {
            // SAFETY: the slot read and call are the object's type's own.
            unsafe {
                if o.is_null() {
                    return null_error();
                }
                if let Some(f) = (*header::type_of(o))
                    .tp_as_number
                    .as_ref()
                    .and_then(|m| m.$slot)
                {
                    return f(o);
                }
                type_error($message, o)
            }
        }
    };
}

unary_number_func!(
    PyNumber_Negative,
    nb_negative,
    "bad operand type for unary -: '%s'"
);
unary_number_func!(
    PyNumber_Positive,
    nb_positive,
    "bad operand type for unary +: '%s'"
);
unary_number_func!(
    PyNumber_Invert,
    nb_invert,
    "bad operand type for unary ~: '%s'"
);
unary_number_func!(
    PyNumber_Absolute,
    nb_absolute,
    "bad operand type for abs(): '%s'"
);

/// CPython's `_PyIndex_Check`.
///
/// # Safety
///
/// `obj` must be a valid object.
unsafe fn index_check(obj: *mut PyObject) -> c_int {
    // SAFETY: the slot read is within the type object.
    unsafe {
        c_int::from(
            (*header::type_of(obj))
                .tp_as_number
                .as_ref()
                .is_some_and(|m| m.nb_index.is_some()),
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIndex_Check(obj: *mut PyObject) -> c_int {
    // SAFETY: the check follows the C contract.
    unsafe { index_check(obj) }
}

/// See the CPython 3.12 contract for this entry point: an `int` that
/// may be a subclass.
///
/// # Safety
///
/// `item` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PyNumber_Index(item: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot read and call are the object's type's own.
    unsafe {
        if item.is_null() {
            return null_error();
        }
        if long::is_long(item) {
            exceptions::retain_object(item);
            return item;
        }
        if index_check(item) == 0 {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "'{}' object cannot be interpreted as an integer",
                    type_name(item, 200)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }
        // SAFETY: `nb_index` exists per `index_check`.
        let result = (*header::type_of(item))
            .tp_as_number
            .as_ref()
            .and_then(|m| m.nb_index)
            .unwrap()(item);
        if result.is_null() {
            return result;
        }
        if core::ptr::eq(header::type_of(result), &raw mut long::PyLong_Type) {
            return result;
        }
        if !long::is_long(result) {
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "__index__ returned non-int (type {})",
                    type_name(result, 200)
                )
                .as_bytes(),
            );
            exceptions::release_object(result);
            return core::ptr::null_mut();
        }
        // CPython warns here that a strict subclass is deprecated; the
        // runtime has no warnings machinery yet, and the default filter
        // hides the warning anyway, so the value goes through as-is.
        result
    }
}

/// See the CPython 3.12 contract for this entry point: an exact `int`.
///
/// # Safety
///
/// `item` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Index(item: *mut PyObject) -> *mut PyObject {
    // SAFETY: the check and copy follow the C contract.
    unsafe {
        let result = _PyNumber_Index(item);
        if !result.is_null() && !core::ptr::eq(header::type_of(result), &raw mut long::PyLong_Type)
        {
            let copy = long::exact_copy(result.cast());
            exceptions::release_object(result);
            return copy;
        }
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `item` must be a valid object; `err` null or an exception class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_AsSsize_t(item: *mut PyObject, err: *mut PyObject) -> isize {
    // SAFETY: the conversion follows the C contract.
    unsafe {
        let value = _PyNumber_Index(item);
        if value.is_null() {
            return -1;
        }
        let result = long::PyLong_AsSsize_t(value);
        if result != -1 {
            exceptions::release_object(value);
            return result;
        }
        if tstate::occurred().is_null() {
            exceptions::release_object(value);
            return result;
        }
        if exceptions::PyErr_GivenExceptionMatches(tstate::occurred(), exceptions::overflow_error())
            == 0
        {
            exceptions::release_object(value);
            return result;
        }
        tstate::clear();
        if err.is_null() {
            // The default clipping.
            let clipped = if long::is_negative(value.cast()) {
                isize::MIN
            } else {
                isize::MAX
            };
            exceptions::release_object(value);
            clipped
        } else {
            tstate::set_string(
                err,
                format!(
                    "cannot fit '{}' into an index-sized integer",
                    type_name(item, 200)
                )
                .as_bytes(),
            );
            exceptions::release_object(value);
            -1
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Long(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversions follow the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        if core::ptr::eq(header::type_of(o), &raw mut long::PyLong_Type) {
            exceptions::retain_object(o);
            return o;
        }
        if let Some(nb_int) = (*header::type_of(o))
            .tp_as_number
            .as_ref()
            .and_then(|m| m.nb_int)
        {
            let result = nb_int(o);
            if result.is_null() {
                return result;
            }
            if core::ptr::eq(header::type_of(result), &raw mut long::PyLong_Type) {
                return result;
            }
            if !long::is_long(result) {
                tstate::set_string(
                    exceptions::type_error(),
                    format!("__int__ returned non-int (type {})", type_name(result, 200))
                        .as_bytes(),
                );
                exceptions::release_object(result);
                return core::ptr::null_mut();
            }
            // The subclass-value copy replaces CPython's deprecation
            // warning for strict subclasses.
            let copy = long::exact_copy(result.cast());
            exceptions::release_object(result);
            return copy;
        }
        if (*header::type_of(o))
            .tp_as_number
            .as_ref()
            .is_some_and(|m| m.nb_index.is_some())
        {
            return PyNumber_Index(o);
        }
        let trunc = crate::capi::object::_PyObject_LookupSpecial(
            o,
            unicode::PyUnicode_FromString(c"__trunc__".as_ptr()),
        );
        if trunc.is_null() && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        if !trunc.is_null() {
            // CPython raises a deprecation warning before the call; with
            // no warnings machinery the call proceeds (the default
            // filter would hide the warning anyway).
            let result = crate::capi::call::PyObject_CallNoArgs(trunc);
            exceptions::release_object(trunc);
            if result.is_null()
                || core::ptr::eq(header::type_of(result), &raw mut long::PyLong_Type)
            {
                return result;
            }
            if long::is_long(result) {
                let copy = long::exact_copy(result.cast());
                exceptions::release_object(result);
                return copy;
            }
            if PyIndex_Check(result) == 0 {
                tstate::set_string(
                    exceptions::type_error(),
                    format!(
                        "__trunc__ returned non-Integral (type {})",
                        type_name(result, 200)
                    )
                    .as_bytes(),
                );
                exceptions::release_object(result);
                return core::ptr::null_mut();
            }
            let index = PyNumber_Index(result);
            exceptions::release_object(result);
            return index;
        }
        if unicode::check(o) {
            return long::PyLong_FromUnicodeObject(o, 10);
        }
        if bytes::check(o) || bytes::bytearray_check(o) {
            // SAFETY: the buffer accessor is the object's own.
            let data = bytes::bytes_of(o);
            return long::from_string_bytes(data, 10, None);
        }
        let mut view: Py_buffer = core::mem::zeroed();
        if bytes::PyObject_GetBuffer(o, &raw mut view, PyBUF_SIMPLE) == 0 {
            let buffer = core::slice::from_raw_parts(view.buf as *const u8, view.len as usize);
            let result = long::from_string_bytes(buffer, 10, None);
            bytes::PyBuffer_Release(&raw mut view);
            return result;
        }
        tstate::clear();
        type_error(
            "int() argument must be a string, a bytes-like object or a real number, not '%s'",
            o,
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_Float(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversions follow the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        if core::ptr::eq(header::type_of(o), &raw mut float_::PyFloat_Type) {
            exceptions::retain_object(o);
            return o;
        }
        if let Some(nb_float) = (*header::type_of(o))
            .tp_as_number
            .as_ref()
            .and_then(|m| m.nb_float)
        {
            let res = nb_float(o);
            if res.is_null() {
                return res;
            }
            if core::ptr::eq(header::type_of(res), &raw mut float_::PyFloat_Type) {
                return res;
            }
            if !float_::is_float(res) {
                tstate::set_string(
                    exceptions::type_error(),
                    format!(
                        "{}.__float__ returned non-float (type {})",
                        type_name(o, 50),
                        type_name(res, 50)
                    )
                    .as_bytes(),
                );
                exceptions::release_object(res);
                return core::ptr::null_mut();
            }
            // CPython warns for a strict subclass before unwrapping; the
            // default filter hides that warning.
            let val = float_::value(res);
            exceptions::release_object(res);
            return float_::PyFloat_FromDouble(val);
        }
        if (*header::type_of(o))
            .tp_as_number
            .as_ref()
            .is_some_and(|m| m.nb_index.is_some())
        {
            let res = _PyNumber_Index(o);
            if res.is_null() {
                return core::ptr::null_mut();
            }
            let val = long::PyLong_AsDouble(res);
            exceptions::release_object(res);
            if val == -1.0 && !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            return float_::PyFloat_FromDouble(val);
        }
        // A float subclass with `nb_float` unset.
        if float_::is_float(o) {
            return float_::PyFloat_FromDouble(float_::value(o));
        }
        float_::PyFloat_FromString(o)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `n` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyNumber_ToBase(n: *mut PyObject, base: c_int) -> *mut PyObject {
    if !(base == 2 || base == 8 || base == 10 || base == 16) {
        tstate::set_string(
            exceptions::system_error(),
            b"PyNumber_ToBase: base must be 2, 8, 10 or 16",
        );
        return core::ptr::null_mut();
    }
    // SAFETY: the index conversion and format call follow the contract.
    unsafe {
        let index = _PyNumber_Index(n);
        if index.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: the object is an exact `int` now.
        let text = long::format(index.cast(), base as u32, true);
        exceptions::release_object(index);
        match text {
            Some(text) => unicode::str_from_utf8(&text),
            None => core::ptr::null_mut(),
        }
    }
}

// --------------------------------------------------------------- PySequence

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Check(s: *mut PyObject) -> c_int {
    if s.is_null() {
        return 0;
    }
    // SAFETY: the flag and slot reads are within the type object.
    unsafe {
        if crate::types::dict::is_dict(s) {
            return 0;
        }
        c_int::from(
            (*header::type_of(s))
                .tp_as_sequence
                .as_ref()
                .is_some_and(|m| m.sq_item.is_some()),
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Size(s: *mut PyObject) -> isize {
    // SAFETY: the slot reads and calls are the object's type's own.
    unsafe {
        if s.is_null() {
            null_error();
            return -1;
        }
        if let Some(length) = (*header::type_of(s))
            .tp_as_sequence
            .as_ref()
            .and_then(|m| m.sq_length)
        {
            return length(s);
        }
        if (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_length.is_some())
        {
            type_error("%s is not a sequence", s);
            return -1;
        }
        type_error("object of type '%s' has no len()", s);
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Length(s: *mut PyObject) -> isize {
    // SAFETY: the delegation follows the C contract.
    unsafe { PySequence_Size(s) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` and `o` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Concat(s: *mut PyObject, o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if s.is_null() || o.is_null() {
            return null_error();
        }
        if let Some(concat) = (*header::type_of(s))
            .tp_as_sequence
            .as_ref()
            .and_then(|m| m.sq_concat)
        {
            return concat(s, o);
        }
        // User classes define `__add__`, which lands in `nb_add`.
        if PySequence_Check(s) != 0 && PySequence_Check(o) != 0 {
            let result = binary_op1(s, o, |m| m.nb_add);
            if !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
            exceptions::release_object(result);
        }
        type_error("'%s' object can't be concatenated", s)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Repeat(o: *mut PyObject, count: isize) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        if let Some(repeat) = (*header::type_of(o))
            .tp_as_sequence
            .as_ref()
            .and_then(|m| m.sq_repeat)
        {
            return repeat(o, count);
        }
        // User classes define `__mul__`, which lands in `nb_multiply`.
        if PySequence_Check(o) != 0 {
            let n = long::PyLong_FromSsize_t(count);
            if n.is_null() {
                return core::ptr::null_mut();
            }
            let result = binary_op1(o, n, |m| m.nb_multiply);
            exceptions::release_object(n);
            if !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
            exceptions::release_object(result);
        }
        type_error("'%s' object can't be repeated", o)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` and `o` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_InPlaceConcat(
    s: *mut PyObject,
    o: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if s.is_null() || o.is_null() {
            return null_error();
        }
        let methods = (*header::type_of(s)).tp_as_sequence.as_ref();
        if let Some(concat) = methods.and_then(|m| m.sq_inplace_concat) {
            return concat(s, o);
        }
        if let Some(concat) = methods.and_then(|m| m.sq_concat) {
            return concat(s, o);
        }
        if PySequence_Check(s) != 0 && PySequence_Check(o) != 0 {
            let result = binary_iop1(s, o, |m| m.nb_inplace_add, |m| m.nb_add);
            if !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
            exceptions::release_object(result);
        }
        type_error("'%s' object can't be concatenated", s)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_InPlaceRepeat(o: *mut PyObject, count: isize) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        let methods = (*header::type_of(o)).tp_as_sequence.as_ref();
        if let Some(repeat) = methods.and_then(|m| m.sq_inplace_repeat) {
            return repeat(o, count);
        }
        if let Some(repeat) = methods.and_then(|m| m.sq_repeat) {
            return repeat(o, count);
        }
        if PySequence_Check(o) != 0 {
            let n = long::PyLong_FromSsize_t(count);
            if n.is_null() {
                return core::ptr::null_mut();
            }
            let result = binary_iop1(o, n, |m| m.nb_inplace_multiply, |m| m.nb_multiply);
            exceptions::release_object(n);
            if !core::ptr::eq(result, singletons::not_implemented()) {
                return result;
            }
            exceptions::release_object(result);
        }
        type_error("'%s' object can't be repeated", o)
    }
}

/// Resolve a negative index against the sequence length (CPython's
/// `PySequence_GetItem`/`SetItem`/`DelItem` share the step).
///
/// # Safety
///
/// `s` must be a valid object.
unsafe fn resolve_index(methods: &PySequenceMethods, s: *mut PyObject, mut i: isize) -> isize {
    // SAFETY: the length call is the object's type's own.
    unsafe {
        if i < 0
            && let Some(length) = methods.sq_length
        {
            let l = length(s);
            i += l;
        }
        i
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_GetItem(s: *mut PyObject, i: isize) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if s.is_null() {
            return null_error();
        }
        if let Some(methods) = (*header::type_of(s)).tp_as_sequence.as_ref()
            && let Some(item) = methods.sq_item
        {
            let i = resolve_index(methods, s, i);
            return item(s, i);
        }
        if (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_subscript.is_some())
        {
            return type_error("%s is not a sequence", s);
        }
        type_error("'%s' object does not support indexing", s)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_GetSlice(
    s: *mut PyObject,
    i1: isize,
    i2: isize,
) -> *mut PyObject {
    let _ = (i1, i2);
    if s.is_null() {
        null_error();
        return core::ptr::null_mut();
    }
    // CPython builds a slice object and hands it to `mp_subscript`.
    // SAFETY: the type read is within the object.
    let has_subscript = unsafe {
        (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_subscript.is_some())
    };
    if has_subscript {
        slice_unavailable();
        return core::ptr::null_mut();
    }
    // SAFETY: the type read is within the object.
    unsafe { type_error("'%s' object is unsliceable", s) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object; `o` an object or null to delete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_SetItem(s: *mut PyObject, i: isize, o: *mut PyObject) -> c_int {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if s.is_null() {
            null_error();
            return -1;
        }
        if let Some(methods) = (*header::type_of(s)).tp_as_sequence.as_ref()
            && let Some(ass_item) = methods.sq_ass_item
        {
            let i = resolve_index(methods, s, i);
            return ass_item(s, i, o);
        }
        if (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_ass_subscript.is_some())
        {
            type_error("%s is not a sequence", s);
            return -1;
        }
        type_error("'%s' object does not support item assignment", s);
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_DelItem(s: *mut PyObject, i: isize) -> c_int {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if s.is_null() {
            null_error();
            return -1;
        }
        if let Some(methods) = (*header::type_of(s)).tp_as_sequence.as_ref()
            && let Some(ass_item) = methods.sq_ass_item
        {
            let i = resolve_index(methods, s, i);
            return ass_item(s, i, core::ptr::null_mut());
        }
        if (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_ass_subscript.is_some())
        {
            type_error("%s is not a sequence", s);
            return -1;
        }
        type_error("'%s' object doesn't support item deletion", s);
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object; `o` an object or null to delete.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_SetSlice(
    s: *mut PyObject,
    i1: isize,
    i2: isize,
    o: *mut PyObject,
) -> c_int {
    let _ = (i1, i2, o);
    if s.is_null() {
        null_error();
        return -1;
    }
    // SAFETY: the type read is within the object.
    let has_ass_subscript = unsafe {
        (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_ass_subscript.is_some())
    };
    if has_ass_subscript {
        slice_unavailable();
        return -1;
    }
    // SAFETY: the type read is within the object.
    unsafe { type_error("'%s' object doesn't support slice assignment", s) };
    -1
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_DelSlice(s: *mut PyObject, i1: isize, i2: isize) -> c_int {
    let _ = (i1, i2);
    if s.is_null() {
        null_error();
        return -1;
    }
    // SAFETY: the type read is within the object.
    let has_ass_subscript = unsafe {
        (*header::type_of(s))
            .tp_as_mapping
            .as_ref()
            .is_some_and(|m| m.mp_ass_subscript.is_some())
    };
    if has_ass_subscript {
        slice_unavailable();
        return -1;
    }
    // SAFETY: the type read is within the object.
    unsafe { type_error("'%s' object doesn't support slice deletion", s) };
    -1
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Tuple(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if v.is_null() {
            return null_error();
        }
        if core::ptr::eq(
            header::type_of(v),
            &raw mut crate::types::tuple::PyTuple_Type,
        ) {
            exceptions::retain_object(v);
            return v;
        }
        if core::ptr::eq(header::type_of(v), &raw mut crate::types::list::PyList_Type) {
            return crate::types::list::PyList_AsTuple(v);
        }
        let it = PyObject_GetIter(v);
        if it.is_null() {
            return core::ptr::null_mut();
        }
        let hint = crate::capi::object::PyObject_LengthHint(v, 10);
        if hint == -1 && !tstate::occurred().is_null() {
            exceptions::release_object(it);
            return core::ptr::null_mut();
        }
        // The guess only sizes a Vec here; the tuple is cut to the real
        // length at the end, which is what CPython's resize dance
        // achieves.
        let mut items: Vec<*mut PyObject> = Vec::with_capacity(hint.max(0) as usize);
        loop {
            let item = PyIter_Next(it);
            if item.is_null() {
                if !tstate::occurred().is_null() {
                    for arg in &items {
                        exceptions::release_object(*arg);
                    }
                    exceptions::release_object(it);
                    return core::ptr::null_mut();
                }
                break;
            }
            items.push(item);
        }
        exceptions::release_object(it);
        crate::types::tuple::from_array(&items)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_List(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if v.is_null() {
            return null_error();
        }
        let result = crate::types::list::PyList_New(0);
        if result.is_null() {
            return core::ptr::null_mut();
        }
        // `_PyList_Extend`'s generic path: iterate and append.
        let it = PyObject_GetIter(v);
        if it.is_null() {
            exceptions::release_object(result);
            return core::ptr::null_mut();
        }
        loop {
            let item = PyIter_Next(it);
            if item.is_null() {
                if !tstate::occurred().is_null() {
                    exceptions::release_object(it);
                    exceptions::release_object(result);
                    return core::ptr::null_mut();
                }
                break;
            }
            if crate::types::list::PyList_Append(result, item) < 0 {
                exceptions::release_object(item);
                exceptions::release_object(it);
                exceptions::release_object(result);
                return core::ptr::null_mut();
            }
            exceptions::release_object(item);
        }
        exceptions::release_object(it);
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `v` must be a valid object; `m` a message for the non-iterable case.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Fast(v: *mut PyObject, m: *const c_char) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if v.is_null() {
            return null_error();
        }
        if core::ptr::eq(header::type_of(v), &raw mut crate::types::list::PyList_Type)
            || core::ptr::eq(
                header::type_of(v),
                &raw mut crate::types::tuple::PyTuple_Type,
            )
        {
            exceptions::retain_object(v);
            return v;
        }
        let it = PyObject_GetIter(v);
        if it.is_null() {
            if exceptions::PyErr_ExceptionMatches(exceptions::type_error()) != 0 {
                // SAFETY: `m` is a C string by contract.
                tstate::set_string(exceptions::type_error(), util::cstr(m).as_bytes());
            }
            return core::ptr::null_mut();
        }
        let result = PySequence_List(it);
        exceptions::release_object(it);
        result
    }
}

const ITERSEARCH_COUNT: c_int = 1;
const ITERSEARCH_INDEX: c_int = 2;
const ITERSEARCH_CONTAINS: c_int = 3;

/// CPython's `_PySequence_IterSearch`.
///
/// # Safety
///
/// `seq` and `obj` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _PySequence_IterSearch(
    seq: *mut PyObject,
    obj: *mut PyObject,
    operation: c_int,
) -> isize {
    // SAFETY: the iteration follows the C contract.
    unsafe {
        if seq.is_null() || obj.is_null() {
            null_error();
            return -1;
        }
        let it = PyObject_GetIter(seq);
        if it.is_null() {
            if exceptions::PyErr_ExceptionMatches(exceptions::type_error()) != 0 {
                type_error("argument of type '%s' is not iterable", seq);
            }
            return -1;
        }
        let mut n: isize = 0;
        let mut wrapped = false;
        let mut failed = false;
        let mut done = false;
        loop {
            let item = PyIter_Next(it);
            if item.is_null() {
                if !tstate::occurred().is_null() {
                    failed = true;
                }
                break;
            }
            let cmp = crate::capi::object::PyObject_RichCompareBool(item, obj, RICH_EQ);
            exceptions::release_object(item);
            if cmp < 0 {
                failed = true;
                break;
            }
            if cmp > 0 {
                match operation {
                    x if x == ITERSEARCH_COUNT => {
                        if n == isize::MAX {
                            tstate::set_string(
                                exceptions::overflow_error(),
                                b"count exceeds C integer size",
                            );
                            failed = true;
                            break;
                        }
                        n += 1;
                    }
                    x if x == ITERSEARCH_INDEX => {
                        if wrapped {
                            tstate::set_string(
                                exceptions::overflow_error(),
                                b"index exceeds C integer size",
                            );
                            failed = true;
                        }
                        done = true;
                        break;
                    }
                    x if x == ITERSEARCH_CONTAINS => {
                        n = 1;
                        done = true;
                        break;
                    }
                    _ => {
                        // CPython marks the branch unreachable.
                        tstate::set_string(
                            exceptions::system_error(),
                            b"invalid operation in _PySequence_IterSearch",
                        );
                        failed = true;
                        break;
                    }
                }
            }
            if operation == ITERSEARCH_INDEX {
                if n == isize::MAX {
                    wrapped = true;
                }
                n += 1;
            }
        }
        if !failed && !done && operation == ITERSEARCH_INDEX {
            tstate::set_string(
                exceptions::value_error(),
                b"sequence.index(x): x not in sequence",
            );
            failed = true;
        }
        exceptions::release_object(it);
        if failed {
            return -1;
        }
        n
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` and `o` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Count(s: *mut PyObject, o: *mut PyObject) -> isize {
    // SAFETY: the search follows the C contract.
    unsafe { _PySequence_IterSearch(s, o, ITERSEARCH_COUNT) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `seq` and `ob` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Contains(seq: *mut PyObject, ob: *mut PyObject) -> c_int {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if let Some(contains) = (*header::type_of(seq))
            .tp_as_sequence
            .as_ref()
            .and_then(|m| m.sq_contains)
        {
            return contains(seq, ob);
        }
        let result = _PySequence_IterSearch(seq, ob, ITERSEARCH_CONTAINS);
        result as c_int
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `w` and `v` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_In(w: *mut PyObject, v: *mut PyObject) -> c_int {
    // SAFETY: the delegation follows the C contract.
    unsafe { PySequence_Contains(w, v) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `s` and `o` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PySequence_Index(s: *mut PyObject, o: *mut PyObject) -> isize {
    // SAFETY: the search follows the C contract.
    unsafe { _PySequence_IterSearch(s, o, ITERSEARCH_INDEX) }
}

// --------------------------------------------------------------- PyMapping

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Check(o: *mut PyObject) -> c_int {
    if o.is_null() {
        return 0;
    }
    // SAFETY: the slot read is within the type object.
    unsafe {
        c_int::from(
            (*header::type_of(o))
                .tp_as_mapping
                .as_ref()
                .is_some_and(|m| m.mp_subscript.is_some()),
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Size(o: *mut PyObject) -> isize {
    // SAFETY: the slot reads and calls are the object's type's own.
    unsafe {
        if o.is_null() {
            null_error();
            return -1;
        }
        if let Some(length) = (*header::type_of(o))
            .tp_as_mapping
            .as_ref()
            .and_then(|m| m.mp_length)
        {
            return length(o);
        }
        if (*header::type_of(o))
            .tp_as_sequence
            .as_ref()
            .is_some_and(|m| m.sq_length.is_some())
        {
            type_error("%s is not a mapping", o);
            return -1;
        }
        type_error("object of type '%s' has no len()", o);
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Length(o: *mut PyObject) -> isize {
    // SAFETY: the delegation follows the C contract.
    unsafe { PyMapping_Size(o) }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object; `key` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_GetItemString(
    o: *mut PyObject,
    key: *const c_char,
) -> *mut PyObject {
    // SAFETY: the conversion and lookup follow the C contract.
    unsafe {
        if key.is_null() {
            return null_error();
        }
        let okey = unicode::PyUnicode_FromString(key);
        if okey.is_null() {
            return core::ptr::null_mut();
        }
        let r = PyObject_GetItem(o, okey);
        exceptions::release_object(okey);
        r
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object; `key` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_SetItemString(
    o: *mut PyObject,
    key: *const c_char,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the conversion and store follow the C contract.
    unsafe {
        if key.is_null() {
            null_error();
            return -1;
        }
        let okey = unicode::PyUnicode_FromString(key);
        if okey.is_null() {
            return -1;
        }
        let r = PyObject_SetItem(o, okey, value);
        exceptions::release_object(okey);
        r
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object; `key` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_HasKeyString(o: *mut PyObject, key: *const c_char) -> c_int {
    // SAFETY: the lookup follows the C contract.
    unsafe {
        let v = PyMapping_GetItemString(o, key);
        if !v.is_null() {
            exceptions::release_object(v);
            return 1;
        }
        tstate::clear();
        0
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` and `key` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_HasKey(o: *mut PyObject, key: *mut PyObject) -> c_int {
    // SAFETY: the lookup follows the C contract.
    unsafe {
        let v = PyObject_GetItem(o, key);
        if !v.is_null() {
            exceptions::release_object(v);
            return 1;
        }
        tstate::clear();
        0
    }
}

/// CPython's `method_output_as_list`: `o.meth()` as a fresh list.
///
/// # Safety
///
/// `o` must be a valid object; `meth` a string object.
unsafe fn method_output_as_list(o: *mut PyObject, meth: *mut PyObject) -> *mut PyObject {
    // SAFETY: the call and conversion follow the C contract.
    unsafe {
        // `PyObject_CallMethodNoArgs` is an inline helper over the
        // vectorcall method entry point.
        let mut stack = [o];
        let meth_output = crate::capi::call::PyObject_VectorcallMethod(
            meth,
            stack.as_mut_ptr(),
            1,
            core::ptr::null_mut(),
        );
        if meth_output.is_null()
            || core::ptr::eq(
                header::type_of(meth_output),
                &raw mut crate::types::list::PyList_Type,
            )
        {
            return meth_output;
        }
        let it = PyObject_GetIter(meth_output);
        if it.is_null() {
            if exceptions::PyErr_ExceptionMatches(exceptions::type_error()) != 0 {
                let method = unicode::as_utf8_vec(meth);
                let name = String::from_utf8_lossy(&method);
                tstate::set_string(
                    exceptions::type_error(),
                    format!(
                        "{}.{}() returned a non-iterable (type {})",
                        type_name(o, 200),
                        name,
                        type_name(meth_output, 200)
                    )
                    .as_bytes(),
                );
            }
            exceptions::release_object(meth_output);
            return core::ptr::null_mut();
        }
        exceptions::release_object(meth_output);
        let result = PySequence_List(it);
        exceptions::release_object(it);
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Keys(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        if core::ptr::eq(header::type_of(o), &raw mut crate::types::dict::PyDict_Type) {
            return crate::types::dict::PyDict_Keys(o);
        }
        method_output_as_list(o, unicode::str_from_utf8("keys"))
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Items(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        if core::ptr::eq(header::type_of(o), &raw mut crate::types::dict::PyDict_Type) {
            return crate::types::dict::PyDict_Items(o);
        }
        method_output_as_list(o, unicode::str_from_utf8("items"))
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyMapping_Values(o: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() {
            return null_error();
        }
        if core::ptr::eq(header::type_of(o), &raw mut crate::types::dict::PyDict_Type) {
            return crate::types::dict::PyDict_Values(o);
        }
        method_output_as_list(o, unicode::str_from_utf8("values"))
    }
}

// ----------------------------------------------------------- item protocol

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` and `key` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_GetItem(o: *mut PyObject, key: *mut PyObject) -> *mut PyObject {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() || key.is_null() {
            return null_error();
        }
        if let Some(subscript) = (*header::type_of(o))
            .tp_as_mapping
            .as_ref()
            .and_then(|m| m.mp_subscript)
        {
            return subscript(o, key);
        }
        if (*header::type_of(o))
            .tp_as_sequence
            .as_ref()
            .is_some_and(|m| m.sq_item.is_some())
        {
            if index_check(key) != 0 {
                let key_value = PyNumber_AsSsize_t(key, exceptions::index_error());
                if key_value == -1 && !tstate::occurred().is_null() {
                    return core::ptr::null_mut();
                }
                return PySequence_GetItem(o, key_value);
            }
            return type_error("sequence index must be integer, not '%s'", key);
        }
        if crate::object::typeobject::chain_is_subtype(
            header::type_of(o),
            &raw mut crate::object::typeobject::PyType_Type,
        ) {
            // `type[int]` is `types.GenericAlias`, which this runtime
            // has not built; other classes take `__class_getitem__`.
            if core::ptr::eq(
                o.cast::<PyTypeObject>(),
                &raw mut crate::object::typeobject::PyType_Type,
            ) {
                tstate::set_string(
                    exceptions::not_implemented_error(),
                    b"coil: types.GenericAlias is not implemented",
                );
                return core::ptr::null_mut();
            }
            let name_object = unicode::PyUnicode_FromString(c"__class_getitem__".as_ptr());
            if name_object.is_null() {
                return core::ptr::null_mut();
            }
            let mut meth = core::ptr::null_mut();
            let found = crate::capi::object::_PyObject_LookupAttr(o, name_object, &raw mut meth);
            exceptions::release_object(name_object);
            if found < 0 {
                return core::ptr::null_mut();
            }
            if !meth.is_null() && !core::ptr::eq(meth, singletons::none()) {
                let result = crate::capi::call::PyObject_CallOneArg(meth, key);
                exceptions::release_object(meth);
                return result;
            }
            if !meth.is_null() {
                exceptions::release_object(meth);
            }
            tstate::set_string(
                exceptions::type_error(),
                format!(
                    "type '{}' is not subscriptable",
                    truncate(util::cstr((*o.cast::<PyTypeObject>()).tp_name), 200)
                )
                .as_bytes(),
            );
            return core::ptr::null_mut();
        }
        type_error("'%s' object is not subscriptable", o)
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o`, `key` and `value` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_SetItem(
    o: *mut PyObject,
    key: *mut PyObject,
    value: *mut PyObject,
) -> c_int {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() || key.is_null() || value.is_null() {
            null_error();
            return -1;
        }
        if let Some(ass_subscript) = (*header::type_of(o))
            .tp_as_mapping
            .as_ref()
            .and_then(|m| m.mp_ass_subscript)
        {
            return ass_subscript(o, key, value);
        }
        if let Some(methods) = (*header::type_of(o)).tp_as_sequence.as_ref() {
            if index_check(key) != 0 {
                let key_value = PyNumber_AsSsize_t(key, exceptions::index_error());
                if key_value == -1 && !tstate::occurred().is_null() {
                    return -1;
                }
                return PySequence_SetItem(o, key_value, value);
            }
            if methods.sq_ass_item.is_some() {
                type_error("sequence index must be integer, not '%s'", key);
                return -1;
            }
        }
        type_error("'%s' object does not support item assignment", o);
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` and `key` must be valid objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_DelItem(o: *mut PyObject, key: *mut PyObject) -> c_int {
    // SAFETY: the dispatch follows the C contract.
    unsafe {
        if o.is_null() || key.is_null() {
            null_error();
            return -1;
        }
        if let Some(ass_subscript) = (*header::type_of(o))
            .tp_as_mapping
            .as_ref()
            .and_then(|m| m.mp_ass_subscript)
        {
            return ass_subscript(o, key, core::ptr::null_mut());
        }
        if let Some(methods) = (*header::type_of(o)).tp_as_sequence.as_ref() {
            if index_check(key) != 0 {
                let key_value = PyNumber_AsSsize_t(key, exceptions::index_error());
                if key_value == -1 && !tstate::occurred().is_null() {
                    return -1;
                }
                return PySequence_DelItem(o, key_value);
            }
            if methods.sq_ass_item.is_some() {
                type_error("sequence index must be integer, not '%s'", key);
                return -1;
            }
        }
        type_error("'%s' object does not support item deletion", o);
        -1
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `o` must be a valid object; `key` a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_DelItemString(o: *mut PyObject, key: *const c_char) -> c_int {
    // SAFETY: the conversion and delete follow the C contract.
    unsafe {
        if o.is_null() || key.is_null() {
            null_error();
            return -1;
        }
        let okey = unicode::PyUnicode_FromString(key);
        if okey.is_null() {
            return -1;
        }
        let ret = PyObject_DelItem(o, okey);
        exceptions::release_object(okey);
        ret
    }
}

// ------------------------------------------------------------ callables

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `x` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyCallable_Check(x: *mut PyObject) -> c_int {
    if x.is_null() {
        return 0;
    }
    // SAFETY: the slot read is within the type object.
    unsafe { c_int::from((*header::type_of(x)).tp_call.is_some()) }
}

// ---------------------------------------------------------------- buffers

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyObject_CheckBuffer(obj: *mut PyObject) -> c_int {
    // SAFETY: the slot read is within the type object.
    unsafe {
        c_int::from(
            (*header::type_of(obj))
                .tp_as_buffer
                .as_ref()
                .is_some_and(|procs| procs.bf_getbuffer.is_some()),
        )
    }
}

/// A buffer view's shape (or strides) as the `ndim`-entry slice it carries,
/// or `None` when the buffer has none.
///
/// # Safety
///
/// `view` must be valid, and `ptr` must hold `ndim` entries when non-null.
unsafe fn dims_of(view: &Py_buffer, ptr: *mut isize) -> Option<&[isize]> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees the entries.
    Some(unsafe { core::slice::from_raw_parts(ptr, view.ndim.max(0) as usize) })
}

/// CPython's `_IsFortranContiguous`.
///
/// # Safety
///
/// `view` must be a valid buffer view.
unsafe fn is_fortran_contiguous(view: &Py_buffer) -> bool {
    if view.len == 0 {
        return true;
    }
    // SAFETY: the view is valid.
    let strides = unsafe { dims_of(view, view.strides) };
    let Some(strides) = strides else {
        // C-contiguous layout: F-contiguous too for the effectively
        // 1-d shapes.
        if view.ndim <= 1 {
            return true;
        }
        // CPython asserts the shape is present when ndim > 1.
        // SAFETY: as above.
        let shape = unsafe { dims_of(view, view.shape).expect("ndim > 1 implies shape") };
        return shape.iter().filter(|dim| **dim > 1).count() <= 1;
    };
    // Non-null strides imply the shape.
    // SAFETY: as above.
    let shape = unsafe { dims_of(view, view.shape).expect("strides imply shape") };
    let mut sd = view.itemsize;
    for i in 0..shape.len() {
        let dim = shape[i];
        if dim > 1 && strides[i] != sd {
            return false;
        }
        sd *= dim;
    }
    true
}

/// CPython's `_IsCContiguous`.
///
/// # Safety
///
/// `view` must be a valid buffer view.
unsafe fn is_c_contiguous(view: &Py_buffer) -> bool {
    if view.len == 0 {
        return true;
    }
    // SAFETY: the view is valid.
    let strides = unsafe { dims_of(view, view.strides) };
    let Some(strides) = strides else {
        return true;
    };
    // Non-null strides imply the shape.
    // SAFETY: as above.
    let shape = unsafe { dims_of(view, view.shape).expect("strides imply shape") };
    let mut sd = view.itemsize;
    for i in (0..shape.len()).rev() {
        let dim = shape[i];
        if dim > 1 && strides[i] != sd {
            return false;
        }
        sd *= dim;
    }
    true
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `view` must be a valid buffer view with `ndim` shape and strides
/// entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_IsContiguous(view: *const Py_buffer, order: c_char) -> c_int {
    // SAFETY: the view is a valid struct per the contract.
    let view = unsafe { &*view };
    if !view.suboffsets.is_null() {
        return 0;
    }
    let ok = match order as u8 {
        // SAFETY: the view is valid.
        b'C' => unsafe { is_c_contiguous(view) },
        // SAFETY: as above.
        b'F' => unsafe { is_fortran_contiguous(view) },
        // SAFETY: as above.
        b'A' => unsafe { is_c_contiguous(view) || is_fortran_contiguous(view) },
        _ => false,
    };
    c_int::from(ok)
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `shape` and `strides` must hold `nd` writable entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FillContiguousStrides(
    nd: c_int,
    shape: *mut isize,
    strides: *mut isize,
    itemsize: c_int,
    fort: c_char,
) {
    // SAFETY: the arrays hold `nd` entries by contract.
    unsafe {
        let mut sd = itemsize as isize;
        if fort as u8 == b'F' {
            for k in 0..nd as usize {
                *strides.add(k) = sd;
                sd *= *shape.add(k);
            }
        } else {
            for k in (0..nd as usize).rev() {
                *strides.add(k) = sd;
                sd *= *shape.add(k);
            }
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `view` must be a valid buffer view; `indices` holds `view->ndim`
/// indices within range.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_GetPointer(
    view: *const Py_buffer,
    indices: *const isize,
) -> *mut c_void {
    // SAFETY: the walk follows the C contract.
    unsafe {
        let view = &*view;
        let mut pointer = view.buf as *mut u8;
        for i in 0..view.ndim as usize {
            pointer = pointer.wrapping_add((*view.strides.add(i) * *indices.add(i)) as usize);
            if !view.suboffsets.is_null() && *view.suboffsets.add(i) >= 0 {
                pointer = core::ptr::read(pointer as *mut *mut u8)
                    .wrapping_add(*view.suboffsets.add(i) as usize);
            }
        }
        pointer as *mut c_void
    }
}

/// CPython's `_Py_add_one_to_index_F`.
fn add_one_to_index_f(nd: c_int, index: &mut [isize], shape: &[isize]) {
    for k in 0..nd as usize {
        if index[k] < shape[k] - 1 {
            index[k] += 1;
            break;
        }
        index[k] = 0;
    }
}

/// CPython's `_Py_add_one_to_index_C`.
fn add_one_to_index_c(nd: c_int, index: &mut [isize], shape: &[isize]) {
    for k in (0..nd as usize).rev() {
        if index[k] < shape[k] - 1 {
            index[k] += 1;
            break;
        }
        index[k] = 0;
    }
}

/// See the CPython 3.12 contract for this entry point. The byte counts
/// come from the native format table; CPython delegates to
/// `struct.calcsize`, which needs the module system.
///
/// # Safety
///
/// `format` must be a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_SizeFromFormat(format: *const c_char) -> isize {
    // The format table is safe to consult; the error below names the
    // module system this entry point would otherwise need.
    if format.is_null() {
        return -1;
    }
    match crate::capi::memoryobject::native_format_size(format) {
        Some(size) => size,
        None => {
            tstate::set_string(
                exceptions::not_implemented_error(),
                b"coil: PyBuffer_SizeFromFormat supports the native single-character formats only",
            );
            -1
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `view` must be a valid buffer view; `buf` holds `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyBuffer_FromContiguous(
    view: *const Py_buffer,
    buf: *const c_void,
    mut len: isize,
    fort: c_char,
) -> c_int {
    // SAFETY: the copies follow the C contract.
    unsafe {
        let view = &*view;
        if len > view.len {
            len = view.len;
        }
        if PyBuffer_IsContiguous(view, fort) != 0 {
            core::ptr::copy_nonoverlapping(buf as *const u8, view.buf as *mut u8, len as usize);
            return 0;
        }
        let nd = view.ndim;
        let shape = core::slice::from_raw_parts(view.shape, nd as usize);
        let mut indices = vec![0isize; nd as usize];
        let mut src = buf as *const u8;
        let mut elements = len / view.itemsize;
        while elements != 0 {
            elements -= 1;
            let ptr = PyBuffer_GetPointer(view, indices.as_ptr());
            core::ptr::copy_nonoverlapping(src, ptr as *mut u8, view.itemsize as usize);
            src = src.add(view.itemsize as usize);
            if fort as u8 == b'F' {
                add_one_to_index_f(nd, &mut indices, shape);
            } else {
                add_one_to_index_c(nd, &mut indices, shape);
            }
        }
        0
    }
}

// --------------------------------------------------------------- iterators

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
#[allow(unpredictable_function_pointer_comparisons)]
pub unsafe extern "C" fn PyIter_Check(obj: *mut PyObject) -> c_int {
    // SAFETY: the slot read is within the type object.
    unsafe {
        let ty = header::type_of(obj);
        c_int::from(
            (*ty).tp_iternext.is_some()
                && (*ty).tp_iternext != Some(crate::capi::object::_PyObject_NextNotImplemented),
        )
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `obj` must be a valid object.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyAIter_Check(obj: *mut PyObject) -> c_int {
    // SAFETY: the slot read is within the type object.
    unsafe {
        let ty = header::type_of(obj);
        let anext = (*ty)
            .tp_as_async
            .as_ref()
            .and_then(|methods| methods.am_anext);
        c_int::from(anext.is_some())
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `iter` must be a valid object with an `tp_iternext` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIter_Next(iter: *mut PyObject) -> *mut PyObject {
    // SAFETY: the slot read and call are the object's type's own.
    unsafe {
        let next = (*header::type_of(iter)).tp_iternext;
        // CPython calls the slot unconditionally; a type that reaches
        // this branch carries `object.tp_iternext`'s placeholder
        // behavior, which the function below spells.
        let result = match next {
            Some(next) => next(iter),
            None => crate::capi::object::_PyObject_NextNotImplemented(iter),
        };
        if result.is_null()
            && !tstate::occurred().is_null()
            && exceptions::PyErr_ExceptionMatches(exceptions::stop_iteration()) != 0
        {
            tstate::clear();
        }
        result
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `iter` and `arg` must be valid objects; `result` writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyIter_Send(
    iter: *mut PyObject,
    arg: *mut PyObject,
    result: *mut *mut PyObject,
) -> c_int {
    // SAFETY: the send follows the C contract.
    unsafe {
        let ty = header::type_of(iter);
        if let Some(send) = (*ty)
            .tp_as_async
            .as_ref()
            .and_then(|methods| methods.am_send)
        {
            return send(iter, arg, result);
        }
        if core::ptr::eq(arg, singletons::none()) && PyIter_Check(iter) != 0 {
            // SAFETY: `tp_iternext` exists per `PyIter_Check`.
            *result = (*ty).tp_iternext.unwrap()(iter);
        } else {
            // `PyObject_CallMethodOneArg`'s inline body.
            let name = unicode::str_from_utf8("send");
            if name.is_null() {
                return PYGEN_ERROR;
            }
            let mut stack = [iter, arg];
            *result = crate::capi::call::PyObject_VectorcallMethod(
                name,
                stack.as_mut_ptr(),
                2,
                core::ptr::null_mut(),
            );
            exceptions::release_object(name);
        }
        if !(*result).is_null() {
            return PYGEN_NEXT;
        }
        // `_PyGen_FetchStopIterationValue`: a pending StopIteration
        // carries the return value (None when the instance has none —
        // this runtime's StopIteration instances hold no value slot).
        if exceptions::PyErr_ExceptionMatches(exceptions::stop_iteration()) != 0 {
            let exc = exceptions::PyErr_GetRaisedException();
            exceptions::release_object(exc);
            exceptions::retain_object(singletons::none());
            *result = singletons::none();
            return PYGEN_RETURN;
        }
        PYGEN_ERROR
    }
}

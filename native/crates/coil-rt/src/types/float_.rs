//! `float`: CPython 3.12's `PyFloatObject` — arithmetic, comparison against
//! `int`, the modular double hash, CPython's repr and `__format__`, and the
//! `PyFloat_*` C API entry points (spec sections 8.2 and 8.5).
//!
//! The algorithms are CPython's (`floatobject.c`, `pyhash.c`, `pystrtod.c`,
//! `formatter_unicode.c`). David Gay's `dtoa` is replaced by Rust's
//! correctly rounded formatting primitives, which share its contract —
//! round ties to even, and produce the shortest digit string that
//! round-trips — re-expressed as the digits/decimal-point pair
//! `format_float_short` consumes. The `__format__` machinery parses and
//! renders plain strings because `str` objects do not exist yet; the
//! unicode task rewraps it. `tp_repr` and `tp_new` stay unset for the same
//! reason, and `nb_divmod` until tuples do. The format-spec parser and the
//! number rendering it drives are shared with `complex`.

use core::ffi::{c_char, c_int, c_void};

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, PyNumberMethods, PyObject, PyTypeObject,
};

use crate::object::exceptions;
use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{
    Py_EQ, Py_GE, Py_GT, Py_LE, Py_LT, Py_NE, chain_is_subtype, zero_type,
};
use crate::types::long;

/// `_Py_TPFLAGS_MATCH_SELF`: the float type's private extra flag.
const PY_TPFLAGS_MATCH_SELF: u32 = 1 << 22;

/// `Py_DTSF_SIGN`: always emit a sign.
pub const PY_DTSF_SIGN: i32 = 0x01;
/// `Py_DTSF_ADD_DOT_0`: keep integral results looking like floats.
pub const PY_DTSF_ADD_DOT_0: i32 = 0x02;
/// `Py_DTSF_ALT`: '#' formatting.
pub const PY_DTSF_ALT: i32 = 0x04;
/// `Py_DTSF_NO_NEG_0`: coerce a negative zero to zero.
pub const PY_DTSF_NO_NEG_0: i32 = 0x08;

/// `Py_DTST_FINITE`.
pub(crate) const PY_DTST_FINITE: c_int = 0;
/// `Py_DTST_INFINITE`.
pub(crate) const PY_DTST_INFINITE: c_int = 1;
/// `Py_DTST_NAN`.
pub(crate) const PY_DTST_NAN: c_int = 2;

/// The hash modulus `2**61 - 1` (`_PyHASH_MODULUS`).
pub(crate) const HASH_MODULUS: usize = (1usize << 61) - 1;
/// The hash of infinity (`_PyHASH_INF`).
const HASH_INF: isize = 314159;

/// CPython 3.12's `PyFloatObject`: the header then `ob_fval` (spec section
/// 8.2's exact class).
#[repr(C)]
pub struct PyFloatObject {
    pub ob_base: PyObject,
    pub ob_fval: f64,
}

/// `PyFloat_AS_DOUBLE`.
///
/// # Safety
///
/// `op` must point to a `PyFloatObject`.
pub(crate) unsafe fn value(op: *const PyObject) -> f64 {
    // SAFETY: `ob_fval` is at the object's fixed offset.
    unsafe { (*(op as *const PyFloatObject)).ob_fval }
}

/// `PyFloat_Check`: a float or a subclass. CPython compares against the
/// type; the `tp_base` chain is the subtype test for this runtime's static
/// types.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_float(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: every type this runtime creates is readied with a complete
    // `tp_base` chain.
    !ty.is_null() && unsafe { chain_is_subtype(ty, &raw mut PyFloat_Type) }
}

// ------------------------------------------------------------- construction

/// `PyFloat_FromDouble`'s body: a float from `PyObject_Malloc` storage with
/// the one reference a C caller owns.
pub(crate) fn from_double(fval: f64) -> *mut PyObject {
    let block = crate::alloc::alloc_block(coil_rt_abi::cp312::float::SIZEOF);
    if block.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    let op = block.cast::<PyFloatObject>();
    // SAFETY: the fresh block is zeroed and exclusively ours.
    unsafe {
        header::c_init(op.cast(), &raw mut PyFloat_Type);
        (*op).ob_fval = fval;
    }
    op.cast()
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyFloat_FromDouble(fval: f64) -> *mut PyObject {
    from_double(fval)
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_AsDouble(op: *mut PyObject) -> f64 {
    if op.is_null() {
        crate::tstate::set_string(
            exceptions::type_error(),
            b"bad argument type for built-in operation",
        );
        return -1.0;
    }
    // SAFETY: `value` reads the fixed offset of a float object after the
    // type check.
    if unsafe { is_float(op) } {
        return unsafe { value(op) };
    }
    // SAFETY: the type and number-table reads follow the header.
    unsafe {
        let ty = header::type_of(op);
        let nb = (*ty).tp_as_number;
        if nb.is_null() || (*nb).nb_float.is_none() {
            if !nb.is_null() && (*nb).nb_index.is_some() {
                let res = (*nb).nb_index.unwrap_unchecked()(op);
                if res.is_null() {
                    return -1.0;
                }
                let val = long::PyLong_AsDouble(res);
                crate::alloc::free_block(res.cast());
                return val;
            }
            let name = crate::util::cstr((*ty).tp_name);
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("must be real number, not {name}").as_bytes(),
            );
            return -1.0;
        }
        let res = (*nb).nb_float.unwrap_unchecked()(op);
        if res.is_null() {
            return -1.0;
        }
        if !is_float(res) {
            let name = crate::util::cstr((*ty).tp_name);
            let res_name = crate::util::cstr((*header::type_of(res)).tp_name);
            crate::tstate::set_string(
                exceptions::type_error(),
                format!("{name}.__float__ returned non-float (type {res_name})").as_bytes(),
            );
            return -1.0;
        }
        let val = value(res);
        crate::alloc::free_block(res.cast());
        val
    }
}

// -------------------------------------------------------------------- hash

/// `_Py_HashDouble` (pyhash.c): the `2**61 - 1` modular hash of a double,
/// folding the mantissa 28 bits at a time and rotating by the exponent, so
/// equal values of different numeric types hash equally. Non-finite values
/// hash to the infinity constants or, for NaN, the pointer hash.
///
/// # Safety
///
/// `inst` is used only as a pointer when `v` is a NaN.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Py_HashDouble(inst: *mut PyObject, v: f64) -> isize {
    if !v.is_finite() {
        if v.is_infinite() {
            return if v > 0.0 { HASH_INF } else { -HASH_INF };
        }
        // SAFETY: the pointer hash reads only the address.
        return unsafe { crate::object::typeobject::_Py_HashPointer(inst as *const c_void) };
    }
    let (mut m, mut e) = long::frexp(v);
    let mut sign: isize = 1;
    if m < 0.0 {
        sign = -1;
        m = -m;
    }
    let mut x: usize = 0;
    while m != 0.0 {
        x = ((x << 28) & HASH_MODULUS) | (x >> (61 - 28));
        m *= 268435456.0; // 2**28
        e -= 28;
        let y = m as usize; // the integer part
        m -= y as f64;
        x += y;
        if x >= HASH_MODULUS {
            x -= HASH_MODULUS;
        }
    }
    let e = if e >= 0 { e % 61 } else { 60 - ((-1 - e) % 61) };
    x = ((x << e) & HASH_MODULUS) | (x >> (61 - e));
    let mut x = x.wrapping_mul(sign as usize);
    if x == usize::MAX {
        x = usize::MAX - 1;
    }
    x as isize
}

unsafe extern "C" fn float_hash(v: *mut PyObject) -> isize {
    // SAFETY: `v` is a float per the slot contract.
    unsafe { _Py_HashDouble(v, value(v)) }
}

// -------------------------------------------------------------- comparison

/// `_PyLong_NumBits`: the number of bits in the magnitude, saturating at
/// `usize::MAX` when even the digit count overflows a size.
///
/// # Safety
///
/// `w` must point to a normalized `PyLongObject`.
unsafe fn long_num_bits(w: *mut PyObject) -> usize {
    // SAFETY: the digit reads are within the object.
    let digits = unsafe { long::digits(w.cast()) };
    let ndigits = digits.len();
    if ndigits == 0 {
        return 0;
    }
    if ndigits > (isize::MAX as usize) / long::SHIFT {
        return usize::MAX;
    }
    (ndigits - 1) * long::SHIFT + (32 - digits[ndigits - 1].leading_zeros()) as usize
}

/// One magnitude shifted left by a bit, the digit-wise form of
/// `_PyLong_Lshift(w, 1)`.
fn mag_lshift1(a: &[u32]) -> Vec<u32> {
    let mut z = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u32;
    for &d in a {
        z.push((d << 1) | carry);
        carry = d >> (long::SHIFT as u32 - 1);
    }
    if carry != 0 {
        z.push(carry);
    }
    z
}

/// `_Py_SwappedOp`, indexed like CPython's table.
const SWAPPED_OP: [c_int; 6] = [Py_GT, Py_GE, Py_EQ, Py_NE, Py_LT, Py_LE];

/// `float_richcompare`: float/float straightforwardly so NaN behaves like
/// the platform compare, float/int through CPython's sign, width and
/// constructed-integer ladder.
///
/// # Safety
///
/// `v` must be a float; `w` must be null or a valid `PyObject`.
pub(crate) unsafe extern "C" fn float_richcompare(
    v: *mut PyObject,
    w: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    // SAFETY: the slot contract guarantees `v` is a float and the reads
    // below follow the stated type checks.
    unsafe {
        let i = value(v);
        let (i, j, op) = if is_float(w) {
            (i, value(w), op)
        } else if !i.is_finite() {
            // An infinity or NaN exceeds any finite int's magnitude.
            if long::is_long(w) {
                (i, 0.0, op)
            } else {
                return singletons::not_implemented();
            }
        } else if long::is_long(w) {
            let wsign = long::sign(w.cast());
            let vsign: i32 = if i == 0.0 {
                0
            } else if i < 0.0 {
                -1
            } else {
                1
            };
            if vsign != wsign {
                // The signs alone decide.
                ((vsign as f64), (wsign as f64), op)
            } else {
                let nbits = long_num_bits(w);
                if nbits == usize::MAX {
                    // So large its magnitude exceeds any finite float's.
                    ((vsign as f64), (wsign as f64 * 2.0), op)
                } else if nbits <= 48 {
                    (i, long::PyLong_AsDouble(w), op)
                } else {
                    let (i, op) = if vsign < 0 {
                        // Compare magnitudes; -1 swaps the comparator.
                        (-i, SWAPPED_OP[op.clamp(0, 5) as usize])
                    } else {
                        (i, op)
                    };
                    let exponent = long::frexp(i).1;
                    if exponent < 0 || (exponent as usize) < nbits {
                        (1.0, 2.0, op)
                    } else if (exponent as usize) > nbits {
                        (2.0, 1.0, op)
                    } else {
                        // Same bit count: compare the scaled integer parts.
                        let intpart = i.trunc();
                        let fracpart = i - intpart;
                        let vv = long::PyLong_FromDouble(intpart);
                        if vv.is_null() {
                            return core::ptr::null_mut();
                        }
                        let mut magv = long::digits(vv.cast()).to_vec();
                        let mut magw = long::digits(w.cast()).to_vec();
                        if fracpart != 0.0 {
                            // The lost fraction is a low 1 bit on vv.
                            magw = mag_lshift1(&magw);
                            magv = mag_lshift1(&magv);
                            magv[0] |= 1;
                        }
                        let truth = match op {
                            Py_LT => long::mag_cmp(&magv, &magw) == core::cmp::Ordering::Less,
                            Py_LE => long::mag_cmp(&magv, &magw) != core::cmp::Ordering::Greater,
                            Py_EQ => long::mag_cmp(&magv, &magw) == core::cmp::Ordering::Equal,
                            Py_NE => long::mag_cmp(&magv, &magw) != core::cmp::Ordering::Equal,
                            Py_GT => long::mag_cmp(&magv, &magw) == core::cmp::Ordering::Greater,
                            Py_GE => long::mag_cmp(&magv, &magw) != core::cmp::Ordering::Less,
                            _ => return singletons::not_implemented(),
                        };
                        return crate::types::bool_::PyBool_FromLong(truth as c_int);
                    }
                }
            }
        } else {
            return singletons::not_implemented();
        };
        let truth = match op {
            Py_EQ => i == j,
            Py_NE => i != j,
            Py_LE => i <= j,
            Py_GE => i >= j,
            Py_LT => i < j,
            Py_GT => i > j,
            _ => return singletons::not_implemented(),
        };
        crate::types::bool_::PyBool_FromLong(truth as c_int)
    }
}

// -------------------------------------------------------------- arithmetic

/// `CONVERT_TO_DOUBLE`: the operand as a double, `Err(NotImplemented)` for
/// the caller to hand on, or `Err(null)` with the exception set.
fn to_double(obj: *mut PyObject) -> Result<f64, *mut PyObject> {
    // SAFETY: the type checks precede every read.
    unsafe {
        if is_float(obj) {
            return Ok(value(obj));
        }
        if long::is_long(obj) {
            let d = long::PyLong_AsDouble(obj);
            if d == -1.0 && !crate::tstate::occurred().is_null() {
                return Err(core::ptr::null_mut());
            }
            return Ok(d);
        }
    }
    Err(singletons::not_implemented())
}

macro_rules! float_binop {
    ($name:ident, |$a:ident, $b:ident| $body:expr) => {
        unsafe extern "C" fn $name(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
            let $a = match to_double(v) {
                Ok(x) => x,
                Err(e) => return e,
            };
            let $b = match to_double(w) {
                Ok(x) => x,
                Err(e) => return e,
            };
            $body
        }
    };
}

float_binop!(float_add, |a, b| from_double(a + b));
float_binop!(float_sub, |a, b| from_double(a - b));
float_binop!(float_mul, |a, b| from_double(a * b));

float_binop!(float_div, |a, b| {
    if b == 0.0 {
        crate::tstate::set_string(exceptions::zero_division_error(), b"float division by zero");
        return core::ptr::null_mut();
    }
    from_double(a / b)
});

float_binop!(float_rem, |vx, wx| {
    if wx == 0.0 {
        crate::tstate::set_string(exceptions::zero_division_error(), b"float modulo");
        return core::ptr::null_mut();
    }
    let mut r#mod = vx % wx;
    if r#mod != 0.0 {
        // The remainder carries the denominator's sign.
        if (wx < 0.0) != (r#mod < 0.0) {
            r#mod += wx;
        }
    } else {
        r#mod = 0.0f64.copysign(wx);
    }
    from_double(r#mod)
});

/// `_float_div_mod`: the exact remainder and the quotient snapped to the
/// nearest integer.
fn float_div_mod(vx: f64, wx: f64) -> (f64, f64) {
    let mut r#mod = vx % wx;
    let mut div = (vx - r#mod) / wx;
    if r#mod != 0.0 {
        if (wx < 0.0) != (r#mod < 0.0) {
            r#mod += wx;
            div -= 1.0;
        }
    } else {
        r#mod = 0.0f64.copysign(wx);
    }
    let mut floordiv;
    if div != 0.0 {
        floordiv = div.floor();
        if div - floordiv > 0.5 {
            floordiv += 1.0;
        }
    } else {
        // Zero quotient: keep the sign of the true quotient.
        floordiv = 0.0f64.copysign(vx / wx);
    }
    (floordiv, r#mod)
}

float_binop!(float_floor_div, |vx, wx| {
    if wx == 0.0 {
        crate::tstate::set_string(
            exceptions::zero_division_error(),
            b"float floor division by zero",
        );
        return core::ptr::null_mut();
    }
    let (floordiv, _) = float_div_mod(vx, wx);
    from_double(floordiv)
});

/// Whether `x` is an odd integer, for the power special cases.
fn is_odd_integer(x: f64) -> bool {
    x.abs() % 2.0 == 1.0
}

/// `float_pow`, shared with `int`'s power slot for the negative-exponent
/// case (CPython forwards there through `PyFloat_Type`'s `nb_power`).
///
/// # Safety
///
/// `v` must be a float or int, `w` a float or int, `z` null, `None` or a
/// valid `PyObject`.
pub(crate) unsafe extern "C" fn float_pow(
    v: *mut PyObject,
    w: *mut PyObject,
    z: *mut PyObject,
) -> *mut PyObject {
    // SAFETY: the reads follow the operand type checks.
    unsafe {
        if !core::ptr::eq(z, singletons::none()) {
            crate::tstate::set_string(
                exceptions::type_error(),
                b"pow() 3rd argument not allowed unless all arguments are integers",
            );
            return core::ptr::null_mut();
        }
        let iv = match to_double(v) {
            Ok(x) => x,
            Err(e) => return e,
        };
        let iw = match to_double(w) {
            Ok(x) => x,
            Err(e) => return e,
        };
        if iw == 0.0 {
            return from_double(1.0); // v**0 is 1, even 0**0
        }
        if iv.is_nan() {
            return from_double(iv);
        }
        if iw.is_nan() {
            return from_double(if iv == 1.0 { 1.0 } else { iw });
        }
        if iw.is_infinite() {
            let iv = iv.abs();
            if iv == 1.0 {
                return from_double(1.0);
            }
            if (iw > 0.0) == (iv > 1.0) {
                return from_double(iw.abs());
            }
            return from_double(0.0);
        }
        if iv.is_infinite() {
            let iw_is_odd = is_odd_integer(iw);
            if iw > 0.0 {
                return from_double(if iw_is_odd { iv } else { iv.abs() });
            }
            return from_double(if iw_is_odd { 0.0f64.copysign(iv) } else { 0.0 });
        }
        if iv == 0.0 {
            if iw < 0.0 {
                crate::tstate::set_string(
                    exceptions::zero_division_error(),
                    b"0.0 cannot be raised to a negative power",
                );
                return core::ptr::null_mut();
            }
            let iw_is_odd = is_odd_integer(iw);
            return from_double(if iw_is_odd { iv } else { 0.0 });
        }
        let mut iv = iv;
        let mut negate_result = false;
        if iv < 0.0 {
            if iw != iw.floor() {
                // Negative numbers to fractional powers become complex.
                return crate::types::complex::complex_pow(v, w, z);
            }
            iv = -iv;
            negate_result = is_odd_integer(iw);
        }
        if iv == 1.0 {
            return from_double(if negate_result { -1.0 } else { 1.0 });
        }
        let mut ix = iv.powf(iw);
        if negate_result {
            ix = -ix;
        }
        if ix.is_infinite() {
            // C's pow sets ERANGE on overflow and `PyErr_SetFromErrno`
            // renders it; both supported platforms say "Result too large".
            crate::tstate::set_string(exceptions::overflow_error(), b"(34, 'Result too large')");
            return core::ptr::null_mut();
        }
        from_double(ix)
    }
}

unsafe extern "C" fn float_neg(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: `v` is a float per the slot contract.
    unsafe { from_double(-value(v)) }
}

unsafe extern "C" fn float_abs(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: `v` is a float per the slot contract.
    unsafe { from_double(value(v).abs()) }
}

unsafe extern "C" fn float_bool(v: *mut PyObject) -> c_int {
    // SAFETY: `v` is a float per the slot contract.
    unsafe { (value(v) != 0.0) as c_int }
}

unsafe extern "C" fn float_float(v: *mut PyObject) -> *mut PyObject {
    // `float.__float__`: an exact float is itself.
    // SAFETY: the header read is within the object.
    unsafe {
        if core::ptr::eq(header::type_of(v), &raw mut PyFloat_Type) {
            return v;
        }
        from_double(value(v))
    }
}

unsafe extern "C" fn float_trunc(v: *mut PyObject) -> *mut PyObject {
    // `float.__trunc__`, the `nb_int` slot.
    // SAFETY: `v` is a float per the slot contract.
    unsafe { long::PyLong_FromDouble(value(v)) }
}

unsafe extern "C" fn float_dealloc(object: *mut PyObject) {
    // SAFETY: the object came from the allocator behind `PyObject_Malloc`,
    // so `PyObject_Free`'s contract holds.
    unsafe { crate::alloc::pymem::PyObject_Free(object.cast()) }
}

// --------------------------------------------------------- repr formatting

/// The digit string and decimal-point position `dtoa` yields: the value is
/// `0.<digits> * 10**decpt`. Digits carry no leading or trailing zero; a
/// zero value is the single digit "0" with `decpt == 1`.
struct Dtoa {
    sign: bool,
    digits: String,
    decpt: i32,
}

/// `_Py_dg_dtoa` over Rust's correctly rounded formatting. Mode 0 is the
/// shortest round-tripping string, mode 2 `ndigits` significant digits with
/// trailing zeros suppressed, mode 3 through `ndigits` past the decimal
/// point; all three round ties to even as Gay's code does.
fn dtoa(d: f64, mode: u8, ndigits: i32) -> Dtoa {
    let sign = d.is_sign_negative();
    let a = d.abs();
    if a == 0.0 {
        return Dtoa {
            sign,
            digits: "0".to_string(),
            decpt: 1,
        };
    }
    match mode {
        0 => {
            let mut p = 1;
            loop {
                let s = format!("{:.*e}", p - 1, a);
                if p == 17 || s.parse::<f64>() == Ok(a) {
                    return from_scientific(sign, &s);
                }
                p += 1;
            }
        }
        2 => from_scientific(sign, &format!("{:.*e}", (ndigits.max(1) - 1) as usize, a)),
        _ => from_fixed(sign, &format!("{:.*}", ndigits.max(0) as usize, a)),
    }
}

/// The scientific-form output of `{:e}` as `dtoa`'s pair: the mantissa
/// digits with trailing zeros stripped, the decimal point after the first.
fn from_scientific(sign: bool, s: &str) -> Dtoa {
    let (mantissa, exponent) = s.split_once('e').expect("scientific form");
    let exp: i32 = exponent.parse().expect("exponent");
    let mut digits = mantissa.replace('.', "");
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    Dtoa {
        sign,
        digits,
        decpt: exp + 1,
    }
}

/// The fixed-form output of `{:.prec}` as `dtoa`'s pair: the digits around
/// the point with leading and trailing zeros stripped; a zero value rounds
/// to the single digit "0" with `decpt == 1`.
fn from_fixed(sign: bool, s: &str) -> Dtoa {
    let point = s.find('.').unwrap_or(s.len());
    let all: String = s.chars().filter(|c| *c != '.').collect();
    let Some(first) = all.find(|c: char| c != '0') else {
        return Dtoa {
            sign,
            digits: "0".to_string(),
            decpt: 1,
        };
    };
    let mut digits = all[first..].to_string();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    Dtoa {
        sign,
        digits,
        decpt: point as i32 - first as i32,
    }
}

/// `format_float_short` minus Gay's allocator: CPython's exact bytes for
/// one rendered double.
#[allow(clippy::too_many_arguments)]
fn format_float_short(
    d: f64,
    format_code: u8,
    mode: u8,
    precision: i32,
    always_add_sign: bool,
    add_dot_0_if_integer: bool,
    use_alt_formatting: bool,
    no_negative_zero: bool,
    uppercase: bool,
    type_out: *mut c_int,
) -> Option<String> {
    if !d.is_finite() {
        let body = if d.is_nan() { "nan" } else { "inf" };
        if !type_out.is_null() {
            // SAFETY: the caller guarantees a writable c_int when not null.
            unsafe {
                *type_out = if d.is_nan() {
                    PY_DTST_NAN
                } else {
                    PY_DTST_INFINITE
                }
            };
        }
        // A NaN's sign is ignored, an infinity keeps it.
        let sign = if d.is_nan() {
            false
        } else {
            d.is_sign_negative()
        };
        let mut out = String::new();
        if sign {
            out.push('-');
        } else if always_add_sign {
            out.push('+');
        }
        if uppercase {
            out.push_str(&body.to_uppercase());
        } else {
            out.push_str(body);
        }
        return Some(out);
    }
    if !type_out.is_null() {
        // SAFETY: the caller guarantees a writable c_int when not null.
        unsafe { *type_out = PY_DTST_FINITE };
    }
    let Dtoa {
        mut sign,
        digits,
        mut decpt,
    } = dtoa(d, mode, precision);
    if no_negative_zero && sign && digits == "0" {
        sign = false;
    }
    let digits_len = digits.len() as i64;
    let mut use_exp = false;
    let mut vdigits_end: i64 = digits_len;
    match format_code {
        b'e' => {
            use_exp = true;
            vdigits_end = precision as i64;
        }
        b'f' => {
            vdigits_end = decpt as i64 + precision as i64;
        }
        b'g' => {
            let bound = if add_dot_0_if_integer {
                precision - 1
            } else {
                precision
            };
            if decpt <= -4 || decpt > bound {
                use_exp = true;
            }
            if use_alt_formatting {
                vdigits_end = precision as i64;
            }
        }
        b'r' => {
            // Exponential form at 1e16, where padding a 16-digit shortest
            // repr with zeros would lie.
            if decpt <= -4 || decpt > 16 {
                use_exp = true;
            }
        }
        _ => {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return None;
        }
    }
    let mut exp = 0i32;
    if use_exp {
        exp = decpt - 1;
        decpt = 1;
    }
    let decpt = decpt as i64;
    let vdigits_start: i64 = if decpt <= 0 { decpt - 1 } else { 0 };
    if !use_exp && add_dot_0_if_integer {
        vdigits_end = vdigits_end.max(decpt + 1);
    } else {
        vdigits_end = vdigits_end.max(decpt);
    }
    let mut out = String::new();
    if sign {
        out.push('-');
    } else if always_add_sign {
        out.push('+');
    }
    if decpt <= 0 {
        out.push_str(&"0".repeat((decpt - vdigits_start) as usize));
        out.push('.');
        out.push_str(&"0".repeat((-decpt) as usize));
    } else {
        out.push_str(&"0".repeat((-vdigits_start) as usize));
    }
    if 0 < decpt && decpt <= digits_len {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    } else {
        out.push_str(&digits);
    }
    if digits_len < decpt {
        out.push_str(&"0".repeat((decpt - digits_len) as usize));
        out.push('.');
        out.push_str(&"0".repeat((vdigits_end - decpt) as usize));
    } else {
        out.push_str(&"0".repeat((vdigits_end - digits_len) as usize));
    }
    if out.ends_with('.') && !use_alt_formatting {
        out.pop();
    }
    if use_exp {
        out.push(if uppercase { 'E' } else { 'e' });
        out.push_str(&format!("{:+03}", exp));
    }
    Some(out)
}

/// `PyOS_double_to_string`'s body over the format-code table of
/// `pystrtod.c`. `None` with `SystemError` set for a bad format code.
pub fn double_to_string(
    val: f64,
    format_code: u8,
    precision: i32,
    flags: i32,
    type_: *mut c_int,
) -> Option<String> {
    let uppercase = matches!(format_code, b'E' | b'F' | b'G');
    let (code, mode, precision) = match format_code {
        b'e' | b'E' => (b'e', 2u8, precision + 1),
        b'f' | b'F' => (b'f', 3u8, precision),
        b'g' | b'G' => (b'g', 2u8, if precision == 0 { 1 } else { precision }),
        b'r' => {
            if precision != 0 {
                crate::tstate::set_string(
                    exceptions::system_error(),
                    b"bad argument to internal function",
                );
                return None;
            }
            (b'r', 0u8, 0)
        }
        _ => {
            crate::tstate::set_string(
                exceptions::system_error(),
                b"bad argument to internal function",
            );
            return None;
        }
    };
    format_float_short(
        val,
        code,
        mode,
        precision,
        flags & PY_DTSF_SIGN != 0,
        flags & PY_DTSF_ADD_DOT_0 != 0,
        flags & PY_DTSF_ALT != 0,
        flags & PY_DTSF_NO_NEG_0 != 0,
        uppercase,
        type_,
    )
}

/// See the CPython 3.12 contract for this entry point. The result is
/// `PyMem_Malloc`ed; free it with `PyMem_Free`.
///
/// # Safety
///
/// `type_` must be null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyOS_double_to_string(
    val: f64,
    format_code: c_char,
    precision: c_int,
    flags: c_int,
    type_: *mut c_int,
) -> *mut c_char {
    let Some(s) = double_to_string(val, format_code as u8, precision, flags, type_) else {
        return core::ptr::null_mut();
    };
    let block = crate::alloc::alloc_block(s.len() + 1);
    if block.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    // SAFETY: the fresh block covers the string and its terminator.
    unsafe {
        core::ptr::copy_nonoverlapping(s.as_ptr(), block.cast::<u8>(), s.len());
        *block.cast::<u8>().add(s.len()) = 0;
    }
    block.cast()
}

// ----------------------------------------------------------- format specs

/// The locale kinds a format spec can request (`enum LocaleType`); the
/// values are the separator characters, as in CPython.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocaleType {
    None,
    Comma,
    Underscore,
    UnderFour,
    Current,
}

/// A render's locale: decimal point, thousands separator and grouping.
pub(crate) struct LocaleInfo {
    decimal_point: char,
    thousands_sep: &'static str,
    grouping: Option<u32>,
}

/// `get_locale_info`. `Current` is the C locale — point '.', no grouping —
/// which is what a runtime without `setlocale` calls always sees.
pub(crate) fn locale_info(t: LocaleType) -> LocaleInfo {
    match t {
        LocaleType::None | LocaleType::Current => LocaleInfo {
            decimal_point: '.',
            thousands_sep: "",
            grouping: None,
        },
        LocaleType::Comma => LocaleInfo {
            decimal_point: '.',
            thousands_sep: ",",
            grouping: Some(3),
        },
        LocaleType::Underscore => LocaleInfo {
            decimal_point: '.',
            thousands_sep: "_",
            grouping: Some(3),
        },
        LocaleType::UnderFour => LocaleInfo {
            decimal_point: '.',
            thousands_sep: "_",
            grouping: Some(4),
        },
    }
}

/// A parsed format specification (`InternalFormatSpec`).
#[derive(Clone, Copy)]
pub(crate) struct FormatSpec {
    pub fill_char: char,
    pub align: char,
    pub alternate: bool,
    pub no_neg_0: bool,
    pub sign: char,
    pub width: isize,
    pub thousands_separators: LocaleType,
    pub precision: isize,
    pub type_: char,
}

pub(crate) fn value_error(message: &str) {
    crate::tstate::set_string(exceptions::value_error(), message.as_bytes());
}

/// `unknown_presentation_type`.
pub(crate) fn unknown_presentation_type(presentation_type: char, type_name: &str) {
    let detail = if (presentation_type as u32) > 32 && (presentation_type as u32) < 128 {
        format!("Unknown format code '{presentation_type}' for object of type '{type_name}'")
    } else {
        format!(
            "Unknown format code '\\x{:x}' for object of type '{type_name}'",
            presentation_type as u32
        )
    };
    value_error(&detail);
}

/// `invalid_thousands_separator_type`.
fn invalid_thousands_separator_type(specifier: LocaleType, presentation_type: char) {
    let specifier = match specifier {
        LocaleType::Comma => ',',
        LocaleType::Underscore | LocaleType::UnderFour => '_',
        _ => '?',
    };
    let detail = if (presentation_type as u32) > 32 && (presentation_type as u32) < 128 {
        format!("Cannot specify '{specifier}' with '{presentation_type}'.")
    } else {
        format!(
            "Cannot specify '{specifier}' with '\\x{:x}'.",
            presentation_type as u32
        )
    };
    value_error(&detail);
}

/// `get_integer`: consume decimal digits, `None` with `ValueError` set on
/// overflow.
fn get_integer(chars: &[char], pos: &mut usize, end: usize) -> Option<(usize, isize)> {
    let mut accumulator: isize = 0;
    let mut numdigits = 0;
    while *pos < end {
        let Some(digitval) = chars[*pos].to_digit(10) else {
            break;
        };
        if accumulator > (isize::MAX - digitval as isize) / 10 {
            value_error("Too many decimal digits in format string");
            return None;
        }
        accumulator = accumulator * 10 + digitval as isize;
        numdigits += 1;
        *pos += 1;
    }
    Some((numdigits, accumulator))
}

/// `parse_internal_render_format_spec`: the whole grammar — fill and align,
/// sign, `z`, `#`, the `0` shorthand, width, `,`/`_`, precision and type —
/// with CPython's cross-checks. `None` returns carry a set exception.
pub(crate) fn parse_format_spec(
    spec: &str,
    type_name: &str,
    default_type: char,
    default_align: char,
) -> Option<FormatSpec> {
    let chars: Vec<char> = spec.chars().collect();
    let end = chars.len();
    let mut pos = 0usize;
    let mut format = FormatSpec {
        fill_char: ' ',
        align: default_align,
        alternate: false,
        no_neg_0: false,
        sign: '\0',
        width: -1,
        thousands_separators: LocaleType::None,
        precision: -1,
        type_: default_type,
    };
    let is_alignment_token = |c: char| matches!(c, '<' | '>' | '=' | '^');
    let mut fill_char_specified = false;
    let mut align_specified = false;

    if end - pos >= 2 && is_alignment_token(chars[pos + 1]) {
        format.align = chars[pos + 1];
        format.fill_char = chars[pos];
        fill_char_specified = true;
        align_specified = true;
        pos += 2;
    } else if end - pos >= 1 && is_alignment_token(chars[pos]) {
        format.align = chars[pos];
        align_specified = true;
        pos += 1;
    }
    if end - pos >= 1 && matches!(chars[pos], '+' | '-' | ' ') {
        format.sign = chars[pos];
        pos += 1;
    }
    if end - pos >= 1 && chars[pos] == 'z' {
        format.no_neg_0 = true;
        pos += 1;
    }
    if end - pos >= 1 && chars[pos] == '#' {
        format.alternate = true;
        pos += 1;
    }
    if !fill_char_specified && end - pos >= 1 && chars[pos] == '0' {
        format.fill_char = '0';
        if !align_specified && default_align == '>' {
            format.align = '=';
        }
        pos += 1;
    }
    let (consumed, width) = get_integer(&chars, &mut pos, end)?;
    format.width = if consumed == 0 { -1 } else { width };

    if end > pos && chars[pos] == ',' {
        format.thousands_separators = LocaleType::Comma;
        pos += 1;
    }
    if end > pos && chars[pos] == '_' {
        if format.thousands_separators != LocaleType::None {
            value_error("Cannot specify both ',' and '_'.");
            return None;
        }
        format.thousands_separators = LocaleType::Underscore;
        pos += 1;
    }
    if end > pos && chars[pos] == ',' && format.thousands_separators == LocaleType::Underscore {
        value_error("Cannot specify both ',' and '_'.");
        return None;
    }
    if end > pos && chars[pos] == '.' {
        pos += 1;
        let (consumed, precision) = get_integer(&chars, &mut pos, end)?;
        if consumed == 0 {
            value_error("Format specifier missing precision");
            return None;
        }
        format.precision = precision;
    }
    if end - pos > 1 {
        value_error(&format!(
            "Invalid format specifier '{spec}' for object of type '{type_name}'"
        ));
        return None;
    }
    if end - pos == 1 {
        format.type_ = chars[pos];
    }
    if format.thousands_separators != LocaleType::None {
        match format.type_ {
            'd' | 'e' | 'f' | 'g' | 'E' | 'G' | '%' | 'F' | '\0' => {}
            // Underscores group by four in bin/oct/hex (PEP 515).
            'b' | 'o' | 'x' | 'X' if format.thousands_separators == LocaleType::Underscore => {
                format.thousands_separators = LocaleType::UnderFour;
            }
            'b' | 'o' | 'x' | 'X' => {
                invalid_thousands_separator_type(format.thousands_separators, format.type_);
                return None;
            }
            _ => {
                invalid_thousands_separator_type(format.thousands_separators, format.type_);
                return None;
            }
        }
    }
    Some(format)
}

/// `parse_number`: the remainder length — decimal or exponent — after the
/// leading digit run, and whether a decimal point sits between them.
fn parse_number(s: &str) -> (isize, bool) {
    let bytes = s.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        pos += 1;
    }
    let has_decimal = pos < bytes.len() && bytes[pos] == b'.';
    let remainder = if has_decimal { pos + 1 } else { pos };
    (bytes.len() as isize - remainder as isize, has_decimal)
}

/// `_PyUnicode_InsertThousandsGrouping` for a fixed group size: separators
/// between groups, zeros before the outermost group down to `min_width`
/// (zero fill with `=` alignment reaches the digit groups this way).
pub(crate) fn insert_thousands_grouping(
    digits: &str,
    min_width: isize,
    locale: &LocaleInfo,
) -> String {
    let Some(group) = locale.grouping else {
        return digits.to_string();
    };
    let group = group as isize;
    let sep_len = locale.thousands_sep.len() as isize;
    let mut remaining = digits.len() as isize;
    let mut min_width = min_width.max(0);
    let mut use_separator = false;
    let mut consumed = 0usize;
    let mut pieces: Vec<String> = Vec::new();
    loop {
        let len = group.min(remaining.max(min_width).max(1));
        let n_zeros = (len - remaining).max(0);
        let n_chars = remaining.min(len).max(0);
        // The buffer fills right to left, so a group's separator trails it.
        let mut piece = String::new();
        piece.push_str(&"0".repeat(n_zeros as usize));
        let end = digits.len() - consumed;
        piece.push_str(&digits[end - n_chars as usize..end]);
        consumed += n_chars as usize;
        if use_separator {
            piece.push_str(locale.thousands_sep);
        }
        pieces.push(piece);
        use_separator = true;
        remaining -= n_chars;
        min_width -= len;
        if remaining <= 0 && min_width <= 0 {
            break;
        }
        min_width -= sep_len;
    }
    pieces.into_iter().rev().collect()
}

/// One number's share of a render (`calc_number_widths` for `n_prefix`
/// zero): the sign and spacing decided, digits grouped.
pub(crate) struct NumberParts {
    n_sign: isize,
    sign: char,
    n_spadding: isize,
    grouped_digits: String,
    n_decimal: isize,
    n_remainder: isize,
    n_lpadding: isize,
    n_rpadding: isize,
}

/// `calc_number_widths` over a rendered number body (after any '-'):
/// returns the pieces with the sign, grouped digits, decimal point and
/// remainder measured out.
pub(crate) fn calc_number_widths(
    body: &str,
    sign_char: char,
    format: &FormatSpec,
    locale: &LocaleInfo,
) -> NumberParts {
    let (n_remainder, has_decimal) = parse_number(body);
    let n_digits = body.chars().count() as isize - n_remainder - if has_decimal { 1 } else { 0 };
    let (n_sign, sign) = match format.sign {
        '+' => (1, if sign_char == '-' { '-' } else { '+' }),
        ' ' => (1, if sign_char == '-' { '-' } else { ' ' }),
        _ => {
            if sign_char == '-' {
                (1, '-')
            } else {
                (0, '\0')
            }
        }
    };
    let n_decimal = if has_decimal {
        locale.decimal_point.len_utf8() as isize
    } else {
        0
    };
    let n_non_digit_non_padding = n_sign + n_decimal + n_remainder;
    let n_min_width = if format.fill_char == '0' && format.align == '=' {
        format.width - n_non_digit_non_padding
    } else {
        0
    };
    let grouped_digits = if n_digits == 0 {
        String::new()
    } else {
        let integer_part =
            &body[..body.len() - n_remainder as usize - if has_decimal { 1 } else { 0 }];
        insert_thousands_grouping(integer_part, n_min_width, locale)
    };
    let mut parts = NumberParts {
        n_sign,
        sign,
        n_spadding: 0,
        grouped_digits,
        n_decimal,
        n_remainder,
        n_lpadding: 0,
        n_rpadding: 0,
    };
    let n_padding =
        format.width - (n_non_digit_non_padding + parts.grouped_digits.chars().count() as isize);
    if n_padding > 0 {
        match format.align {
            '<' => parts.n_rpadding = n_padding,
            '^' => {
                parts.n_lpadding = n_padding / 2;
                parts.n_rpadding = n_padding - parts.n_lpadding;
            }
            '=' => parts.n_spadding = n_padding,
            _ => parts.n_lpadding = n_padding,
        }
    }
    parts
}

/// `fill_number` for `n_prefix` zero: the assembled number with padding.
pub(crate) fn fill_number(body: &str, parts: &NumberParts, format: &FormatSpec) -> String {
    let mut out = String::new();
    let fill = || format.fill_char.to_string();
    out.push_str(&fill().repeat(parts.n_lpadding.max(0) as usize));
    if parts.n_sign == 1 {
        out.push(parts.sign);
    }
    out.push_str(&fill().repeat(parts.n_spadding.max(0) as usize));
    out.push_str(&parts.grouped_digits);
    if parts.n_decimal == 1 {
        out.push('.');
    }
    let start = body.len() - parts.n_remainder.max(0) as usize;
    out.push_str(&body[start..]);
    out.push_str(&fill().repeat(parts.n_rpadding.max(0) as usize));
    out
}

/// `calc_padding` + `fill_padding` around already-rendered content.
pub(crate) fn pad(content: &str, format: &FormatSpec) -> String {
    if format.width < 0 {
        return content.to_string();
    }
    let n = content.chars().count() as isize;
    let total = n.max(format.width);
    let lpadding = match format.align {
        '>' => total - n,
        '^' => (total - n) / 2,
        // '<' and '=' (the latter already rejected where CPython rejects it).
        _ => 0,
    };
    let rpadding = total - n - lpadding;
    format!(
        "{}{}{}",
        format.fill_char.to_string().repeat(lpadding as usize),
        content,
        format.fill_char.to_string().repeat(rpadding as usize)
    )
}

/// `format_float_internal`: render `value` per `format`. `None` returns
/// carry a set exception.
pub(crate) fn format_float_internal(value: f64, format: &FormatSpec) -> Option<String> {
    if format.precision > i32::MAX as isize {
        value_error("precision too big");
        return None;
    }
    let mut flags = 0i32;
    if format.alternate {
        flags |= PY_DTSF_ALT;
    }
    if format.no_neg_0 {
        flags |= PY_DTSF_NO_NEG_0;
    }
    let mut type_ = format.type_;
    let mut default_precision = 6i32;
    if type_ == '\0' {
        // Omitted type: like repr, else 'g' with a digit after the point.
        flags |= PY_DTSF_ADD_DOT_0;
        type_ = 'r';
        default_precision = 0;
    }
    if type_ == 'n' {
        // 'n' is 'g' in the (C) locale.
        type_ = 'g';
    }
    let mut val = value;
    let mut add_pct = false;
    if type_ == '%' {
        type_ = 'f';
        val *= 100.0;
        add_pct = true;
    }
    let mut precision = format.precision as i32;
    if precision < 0 {
        precision = default_precision;
    } else if type_ == 'r' {
        type_ = 'g';
    }
    let mut body = double_to_string(val, type_ as u8, precision, flags, core::ptr::null_mut())?;
    if add_pct {
        body.push('%');
    }
    if format.sign != '+'
        && format.sign != ' '
        && format.width == -1
        && format.type_ != 'n'
        && format.thousands_separators == LocaleType::None
    {
        return Some(body);
    }
    let (rest, sign_char) = body
        .strip_prefix('-')
        .map_or((body.as_str(), '\0'), |r| (r, '-'));
    let locale = locale_info(if format.type_ == 'n' {
        LocaleType::Current
    } else {
        format.thousands_separators
    });
    let parts = calc_number_widths(rest, sign_char, format, &locale);
    Some(fill_number(rest, &parts, format))
}

/// `float.__format__` (`_PyFloat_FormatAdvancedWriter`): parse `spec`,
/// check the presentation type, render. `None` returns carry a set
/// exception.
pub fn format_value(value: f64, spec: &str) -> Option<String> {
    // A zero-length spec is str(), which is repr for a float.
    let format = parse_format_spec(spec, "float", '\0', '>')?;
    if !matches!(
        format.type_,
        '\0' | 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n' | '%'
    ) {
        unknown_presentation_type(format.type_, "float");
        return None;
    }
    format_float_internal(value, &format)
}

// ------------------------------------------------------------ pack/unpack

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` must point to two writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Pack2(x: f64, p: *mut c_char, le: c_int) -> c_int {
    fn overflow() -> c_int {
        crate::tstate::set_string(
            exceptions::overflow_error(),
            b"float too large to pack with e format",
        );
        -1
    }
    let mut x = x;
    let (sign, e, mut bits): (u16, i32, u16) = if x == 0.0 {
        (u16::from(x.is_sign_negative()), 0, 0)
    } else if x.is_infinite() {
        (u16::from(x < 0.0), 0x1f, 0)
    } else if x.is_nan() {
        // The quiet NaN with the top fraction bit only, sign kept.
        (u16::from(x.is_sign_negative()), 0x1f, 512)
    } else {
        let mut sign = 0u16;
        if x < 0.0 {
            sign = 1;
            x = -x;
        }
        let (mut f, e) = long::frexp(x);
        if !(0.5..1.0).contains(&f) {
            crate::tstate::set_string(exceptions::system_error(), b"frexp() result out of range");
            return -1;
        }
        // Normalize to [1.0, 2.0).
        f *= 2.0;
        let mut e = e - 1;
        if e >= 16 {
            return overflow();
        }
        if e < -25 {
            // |x| < 2**-25 underflows to zero.
            f = 0.0;
            e = 0;
        } else if e < -14 {
            // |x| < 2**-14: gradual underflow.
            f = long::ldexp(f, 14 + e);
            e = 0;
        } else {
            e += 15;
            f -= 1.0; // drop the leading 1
        }
        f *= 1024.0; // 2**10
        let mut bits = f as u16; // truncation
        // Round to even, carrying into the exponent.
        if f - bits as f64 > 0.5 || (f - bits as f64 == 0.5 && bits % 2 == 1) {
            bits += 1;
            if bits == 1024 {
                bits = 0;
                e += 1;
                if e == 31 {
                    return overflow();
                }
            }
        }
        (sign, e, bits)
    };
    bits |= ((e as u16) << 10) | (sign << 15);
    let bytes = if le != 0 {
        bits.to_le_bytes()
    } else {
        bits.to_be_bytes()
    };
    // SAFETY: the caller guarantees two writable bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), 2) };
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` must point to four writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Pack4(x: f64, p: *mut c_char, le: c_int) -> c_int {
    let y = x as f32;
    if y.is_infinite() && !x.is_infinite() {
        crate::tstate::set_string(
            exceptions::overflow_error(),
            b"float too large to pack with f format",
        );
        return -1;
    }
    let bytes = if le != 0 {
        y.to_bits().to_le_bytes()
    } else {
        y.to_bits().to_be_bytes()
    };
    // SAFETY: the caller guarantees four writable bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), 4) };
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` must point to eight writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Pack8(x: f64, p: *mut c_char, le: c_int) -> c_int {
    let bytes = if le != 0 {
        x.to_bits().to_le_bytes()
    } else {
        x.to_bits().to_be_bytes()
    };
    // SAFETY: the caller guarantees eight writable bytes.
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), 8) };
    0
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` must point to two readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Unpack2(p: *const c_char, le: c_int) -> f64 {
    // SAFETY: the caller guarantees two readable bytes.
    let b = unsafe { core::slice::from_raw_parts(p.cast::<u8>(), 2) };
    let bytes = if le != 0 { [b[1], b[0]] } else { [b[0], b[1]] };
    let bits = u16::from_be_bytes(bytes);
    let sign = ((bits >> 15) & 1) as i32;
    let e = ((bits & 0x7C00) >> 10) as i32;
    let f = (bits & 0x03FF) as u32;
    if e == 0x1f {
        return if f == 0 {
            if sign != 0 {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        } else if sign != 0 {
            -f64::NAN
        } else {
            f64::NAN
        };
    }
    let mut x = f as f64 / 1024.0;
    if e == 0 {
        x = long::ldexp(x, -14);
    } else {
        x += 1.0;
        x = long::ldexp(x, e - 15);
    }
    if sign != 0 {
        x = -x;
    }
    x
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` must point to four readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Unpack4(p: *const c_char, le: c_int) -> f64 {
    // SAFETY: the caller guarantees four readable bytes.
    let b = unsafe { core::slice::from_raw_parts(p.cast::<u8>(), 4) };
    let bits = if le != 0 {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    } else {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    };
    f32::from_bits(bits) as f64
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `p` must point to eight readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_Unpack8(p: *const c_char, le: c_int) -> f64 {
    // SAFETY: the caller guarantees eight readable bytes.
    let b = unsafe { core::slice::from_raw_parts(p.cast::<u8>(), 8) };
    let bits = if le != 0 {
        u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    } else {
        u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
    };
    f64::from_bits(bits)
}

// ------------------------------------------------------------- C API tail

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyFloat_GetMax() -> f64 {
    f64::MAX
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyFloat_GetMin() -> f64 {
    f64::MIN_POSITIVE
}

/// See the CPython 3.12 contract for this entry point. A spec-section-10.1
/// stub: it needs `str` objects, which arrive with the unicode task.
///
/// # Safety
///
/// `v` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyFloat_FromString(v: *mut PyObject) -> *mut PyObject {
    let _ = v;
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyFloat_FromString is not implemented",
    );
    core::ptr::null_mut()
}

/// See the CPython 3.12 contract for this entry point. A spec-section-10.1
/// stub: it needs struct sequences, which arrive with their own task.
#[unsafe(no_mangle)]
pub extern "C" fn PyFloat_GetInfo() -> *mut PyObject {
    crate::tstate::set_string(
        exceptions::system_error(),
        b"coil: PyFloat_GetInfo is not implemented",
    );
    core::ptr::null_mut()
}

// ------------------------------------------------------------ type object

const fn float_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_name = c"float".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::float::SIZEOF as isize;
    t.tp_dealloc = Some(float_dealloc);
    t.tp_as_number = &raw mut FLOAT_AS_NUMBER;
    t.tp_hash = Some(float_hash);
    t.tp_richcompare = Some(float_richcompare);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE | PY_TPFLAGS_MATCH_SELF;
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// `float`'s number slots, CPython's `float_as_number` except `nb_divmod`,
/// which needs tuples and arrives with them.
static mut FLOAT_AS_NUMBER: PyNumberMethods = PyNumberMethods {
    nb_add: Some(float_add),
    nb_subtract: Some(float_sub),
    nb_multiply: Some(float_mul),
    nb_remainder: Some(float_rem),
    nb_divmod: None,
    nb_power: Some(float_pow),
    nb_negative: Some(float_neg),
    nb_positive: Some(float_float),
    nb_absolute: Some(float_abs),
    nb_bool: Some(float_bool),
    nb_invert: None,
    nb_lshift: None,
    nb_rshift: None,
    nb_and: None,
    nb_xor: None,
    nb_or: None,
    nb_int: Some(float_trunc),
    nb_reserved: core::ptr::null_mut(),
    nb_float: Some(float_float),
    nb_inplace_add: None,
    nb_inplace_subtract: None,
    nb_inplace_multiply: None,
    nb_inplace_remainder: None,
    nb_inplace_power: None,
    nb_inplace_lshift: None,
    nb_inplace_rshift: None,
    nb_inplace_and: None,
    nb_inplace_xor: None,
    nb_inplace_or: None,
    nb_floor_divide: Some(float_floor_div),
    nb_true_divide: Some(float_div),
    nb_inplace_floor_divide: None,
    nb_inplace_true_divide: None,
    nb_index: None,
    nb_matrix_multiply: None,
    nb_inplace_matrix_multiply: None,
};

/// The `float` type object (CPython's `PyFloat_Type`). `tp_repr`, the
/// `real`/`imag` getsets and `tp_new` stay unset until `str` objects,
/// descriptors and argument parsing exist.
#[unsafe(no_mangle)]
pub static mut PyFloat_Type: PyTypeObject = float_type();

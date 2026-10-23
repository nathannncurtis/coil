//! `complex`: CPython 3.12's `PyComplexObject` — arithmetic over
//! `Py_complex` (`_Py_c_*`), equality against the other number types, the
//! combined modular hash, repr and `__format__`, and the `PyComplex_*` C
//! API entry points (spec sections 8.2 and 8.5).
//!
//! The algorithms are CPython's (`complexobject.c`). The formatting shares
//! `float`'s spec machinery, as `formatter_unicode.c` does. C errno is
//! modelled as an explicit flag on the internal arithmetic helpers: CPython
//! raises `ZeroDivisionError` on `EDOM` and `OverflowError` on `ERANGE`
//! exactly where those C library calls set it. `tp_new` stays unset
//! until argument parsing exists, and `nb_divmod` (and the slots CPython
//! also leaves empty) until tuples do.

use core::ffi::c_int;

use coil_capi_sys::{
    Py_TPFLAGS_BASETYPE, Py_TPFLAGS_DEFAULT, PyNumberMethods, PyObject, PyTypeObject,
};

use crate::object::header;
use crate::object::singletons;
use crate::object::typeobject::{Py_EQ, Py_NE, chain_is_subtype, zero_type};
use crate::types::bool_::PyBool_FromLong;
use crate::types::exc as exceptions;
use crate::types::float_;
use crate::types::float_::{
    PY_DTSF_ALT, PY_DTSF_NO_NEG_0, PY_DTSF_SIGN, double_to_string, parse_format_spec,
};
use crate::types::long;

/// The multiplier for the imaginary hash term (`_PyHASH_IMAG`).
const HASH_IMAG: usize = 1000003;

/// The `EDOM` the C library sets: division by zero, zero to a negative or
/// complex power.
const EDOM: u8 = 1;
/// The `ERANGE` the C library sets: overflow in `pow` or `exp`.
const ERANGE: u8 = 2;

/// CPython's `Py_complex`: real and imaginary parts.
#[repr(C)]
#[derive(Clone, Copy, PartialEq)]
pub struct Py_complex {
    pub real: f64,
    pub imag: f64,
}

/// CPython 3.12's `PyComplexObject`: the header then `cval` (spec section
/// 8.2's exact class).
#[repr(C)]
pub struct PyComplexObject {
    pub ob_base: PyObject,
    pub cval: Py_complex,
}

/// `PyComplex_Check` over the `tp_base` chain, like `PyFloat_Check`.
///
/// # Safety
///
/// `op` must be null or point to a valid `PyObject` header.
pub(crate) unsafe fn is_complex(op: *const PyObject) -> bool {
    if op.is_null() {
        return false;
    }
    // SAFETY: the header read is within the object.
    let ty = unsafe { header::type_of(op) };
    // SAFETY: every type this runtime creates is readied with a complete
    // `tp_base` chain.
    !ty.is_null() && unsafe { chain_is_subtype(ty, &raw mut PyComplex_Type) }
}

/// The value of a complex object.
///
/// # Safety
///
/// `op` must point to a `PyComplexObject`.
unsafe fn cval(op: *const PyObject) -> Py_complex {
    // SAFETY: `cval` is at the object's fixed offset.
    unsafe { (*(op as *const PyComplexObject)).cval }
}

/// `PyComplex_FromCComplex`'s body: a complex from `PyObject_Malloc`
/// storage with the one reference a C caller owns.
fn from_c_complex_value(cval: Py_complex) -> *mut PyObject {
    let block = crate::alloc::alloc_block(coil_rt_abi::cp312::complex::SIZEOF);
    if block.is_null() {
        crate::tstate::set_string(exceptions::memory_error(), b"");
        return core::ptr::null_mut();
    }
    let op = block.cast::<PyComplexObject>();
    // SAFETY: the fresh block is zeroed and exclusively ours.
    unsafe {
        header::c_init(op.cast(), &raw mut PyComplex_Type);
        (*op).cval = cval;
    }
    op.cast()
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_FromCComplex(cval: Py_complex) -> *mut PyObject {
    from_c_complex_value(cval)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn PyComplex_FromDoubles(real: f64, imag: f64) -> *mut PyObject {
    from_c_complex_value(Py_complex { real, imag })
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_RealAsDouble(op: *mut PyObject) -> f64 {
    // SAFETY: the read follows the type check.
    unsafe {
        if is_complex(op) {
            cval(op).real
        } else {
            float_::PyFloat_AsDouble(op)
        }
    }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be null or a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_ImagAsDouble(op: *mut PyObject) -> f64 {
    // SAFETY: the read follows the type check.
    unsafe { if is_complex(op) { cval(op).imag } else { 0.0 } }
}

/// See the CPython 3.12 contract for this entry point.
///
/// # Safety
///
/// `op` must be a valid `PyObject`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn PyComplex_AsCComplex(op: *mut PyObject) -> Py_complex {
    // SAFETY: the reads follow the type checks; a non-number reports
    // through `PyFloat_AsDouble` exactly as CPython's fallthrough does.
    unsafe {
        if is_complex(op) {
            return cval(op);
        }
        Py_complex {
            real: float_::PyFloat_AsDouble(op),
            imag: 0.0,
        }
    }
}

// ------------------------------------------------------- _Py_c arithmetic

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_sum(a: Py_complex, b: Py_complex) -> Py_complex {
    Py_complex {
        real: a.real + b.real,
        imag: a.imag + b.imag,
    }
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_diff(a: Py_complex, b: Py_complex) -> Py_complex {
    Py_complex {
        real: a.real - b.real,
        imag: a.imag - b.imag,
    }
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_neg(a: Py_complex) -> Py_complex {
    Py_complex {
        real: -a.real,
        imag: -a.imag,
    }
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_prod(a: Py_complex, b: Py_complex) -> Py_complex {
    // The C reads `a.real*b.real - a.imag*b.imag` and
    // `a.real*b.imag + a.imag*b.real`; the pinned 3.12 build contracts
    // those into fused multiplies, and fusing the same products here
    // keeps the results bit-identical to it.
    Py_complex {
        real: a.real.mul_add(b.real, -(a.imag * b.imag)),
        imag: a.real.mul_add(b.imag, a.imag * b.real),
    }
}

/// `_Py_c_quot`, with the `EDOM` the C library sets made explicit: divide
/// through whichever part of `b` is larger (CACM Algorithm 116).
pub(crate) fn c_quot(a: Py_complex, b: Py_complex) -> (Py_complex, u8) {
    let abs_breal = if b.real < 0.0 { -b.real } else { b.real };
    let abs_bimag = if b.imag < 0.0 { -b.imag } else { b.imag };
    let r = if abs_breal >= abs_bimag {
        if abs_breal == 0.0 {
            let zero = Py_complex {
                real: 0.0,
                imag: 0.0,
            };
            return (zero, EDOM);
        }
        let ratio = b.imag / b.real;
        // Fused as in `_Py_c_prod`, matching the pinned build.
        let denom = b.imag.mul_add(ratio, b.real);
        Py_complex {
            real: a.imag.mul_add(ratio, a.real) / denom,
            imag: (-a.real).mul_add(ratio, a.imag) / denom,
        }
    } else if abs_bimag >= abs_breal {
        let ratio = b.real / b.imag;
        let denom = b.real.mul_add(ratio, b.imag);
        Py_complex {
            real: a.real.mul_add(ratio, a.imag) / denom,
            imag: a.imag.mul_add(ratio, -a.real) / denom,
        }
    } else {
        // At least one of b's parts is a NaN.
        Py_complex {
            real: f64::NAN,
            imag: f64::NAN,
        }
    };
    (r, 0)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_quot(a: Py_complex, b: Py_complex) -> Py_complex {
    c_quot(a, b).0
}

/// `_Py_c_pow`, with the `EDOM`/`ERANGE` the C library sets made explicit.
fn c_pow(a: Py_complex, b: Py_complex) -> (Py_complex, u8) {
    if b.real == 0.0 && b.imag == 0.0 {
        return (
            Py_complex {
                real: 1.0,
                imag: 0.0,
            },
            0,
        );
    }
    if a.real == 0.0 && a.imag == 0.0 {
        let errno = if b.imag != 0.0 || b.real < 0.0 {
            EDOM
        } else {
            0
        };
        return (
            Py_complex {
                real: 0.0,
                imag: 0.0,
            },
            errno,
        );
    }
    let vabs = a.real.hypot(a.imag);
    let mut len = vabs.powf(b.real);
    let mut errno = 0u8;
    // pow sets ERANGE exactly when a finite computation overflows.
    if len.is_infinite() && vabs.is_finite() && b.real > 0.0 {
        errno = ERANGE;
    }
    let at = a.imag.atan2(a.real);
    let mut phase = at * b.real;
    if b.imag != 0.0 {
        let arg = -at * b.imag;
        let scale = arg.exp();
        if scale.is_infinite() && arg.is_finite() {
            errno = ERANGE;
        }
        len *= scale;
        phase += b.imag * vabs.ln();
    }
    (
        Py_complex {
            real: len * phase.cos(),
            imag: len * phase.sin(),
        },
        errno,
    )
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_pow(a: Py_complex, b: Py_complex) -> Py_complex {
    c_pow(a, b).0
}

/// `c_powu`: `x` to a non-negative integer power by squaring.
fn c_powu(x: Py_complex, n: i64) -> Py_complex {
    let mut r = Py_complex {
        real: 1.0,
        imag: 0.0,
    };
    let mut p = x;
    let mut mask: i64 = 1;
    while mask > 0 && n >= mask {
        if n & mask != 0 {
            r = _Py_c_prod(r, p);
        }
        mask <<= 1;
        p = _Py_c_prod(p, p);
    }
    r
}

/// `c_powi`: `x` to an integer power; the inverse case divides, which is
/// where `EDOM` can arise.
fn c_powi(x: Py_complex, n: i64) -> (Py_complex, u8) {
    if n > 0 {
        (c_powu(x, n), 0)
    } else {
        c_quot(
            Py_complex {
                real: 1.0,
                imag: 0.0,
            },
            c_powu(x, -n),
        )
    }
}

/// `_Py_c_abs`, with the `ERANGE` hypot overflow sets made explicit.
pub(crate) fn c_abs(z: Py_complex) -> (f64, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        // C99: an infinite part gives infinity even over a NaN part.
        if z.real.is_infinite() {
            return (z.real.abs(), 0);
        }
        if z.imag.is_infinite() {
            return (z.imag.abs(), 0);
        }
        return (f64::NAN, 0);
    }
    let result = z.real.hypot(z.imag);
    let errno = if result.is_infinite() { ERANGE } else { 0 };
    (result, errno)
}

/// See the CPython 3.12 contract for this entry point.
#[unsafe(no_mangle)]
pub extern "C" fn _Py_c_abs(z: Py_complex) -> f64 {
    c_abs(z).0
}

// ------------------------------------------------------------------ slots

/// `TO_COMPLEX`: the operand as a `Py_complex`, `Err(NotImplemented)` for
/// the caller to hand on, or `Err(null)` with the exception set.
fn to_complex(obj: *mut PyObject) -> Result<Py_complex, *mut PyObject> {
    // SAFETY: the reads follow the type checks.
    unsafe {
        if is_complex(obj) {
            return Ok(cval(obj));
        }
        if long::is_long(obj) {
            let real = long::PyLong_AsDouble(obj);
            if real == -1.0 && !crate::tstate::occurred().is_null() {
                return Err(core::ptr::null_mut());
            }
            return Ok(Py_complex { real, imag: 0.0 });
        }
        if float_::is_float(obj) {
            return Ok(Py_complex {
                real: float_::value(obj),
                imag: 0.0,
            });
        }
    }
    Err(singletons::not_implemented())
}

macro_rules! complex_binop {
    ($name:ident, |$a:ident, $b:ident| $body:expr) => {
        unsafe extern "C" fn $name(v: *mut PyObject, w: *mut PyObject) -> *mut PyObject {
            let $a = match to_complex(v) {
                Ok(x) => x,
                Err(e) => return e,
            };
            let $b = match to_complex(w) {
                Ok(x) => x,
                Err(e) => return e,
            };
            $body
        }
    };
}

complex_binop!(complex_add, |a, b| from_c_complex_value(_Py_c_sum(a, b)));
complex_binop!(complex_sub, |a, b| from_c_complex_value(_Py_c_diff(a, b)));
complex_binop!(complex_mul, |a, b| from_c_complex_value(_Py_c_prod(a, b)));

complex_binop!(complex_div, |a, b| {
    let (quot, errno) = c_quot(a, b);
    if errno == EDOM {
        crate::tstate::set_string(
            exceptions::zero_division_error(),
            b"complex division by zero",
        );
        return core::ptr::null_mut();
    }
    from_c_complex_value(quot)
});

/// `complex_pow`, shared with `float`'s power slot for the negative
/// fractional case (CPython forwards there through `PyComplex_Type`'s
/// `nb_power`).
///
/// # Safety
///
/// `v` and `w` must be numbers of this runtime; `z` must be null, `None`
/// or a valid `PyObject`.
pub(crate) unsafe extern "C" fn complex_pow(
    v: *mut PyObject,
    w: *mut PyObject,
    z: *mut PyObject,
) -> *mut PyObject {
    let a = match to_complex(v) {
        Ok(x) => x,
        Err(e) => return e,
    };
    let b = match to_complex(w) {
        Ok(x) => x,
        Err(e) => return e,
    };
    if !core::ptr::eq(z, singletons::none()) {
        crate::tstate::set_string(exceptions::value_error(), b"complex modulo");
        return core::ptr::null_mut();
    }
    // A small integer exponent takes the faster, more accurate path.
    let (p, mut errno) = if b.imag == 0.0 && b.real == b.real.floor() && b.real.abs() <= 100.0 {
        c_powi(a, b.real as i64)
    } else {
        c_pow(a, b)
    };
    // `_Py_ADJUST_ERANGE2` (pycore_pymath.h): an infinite part with no
    // error yet is an overflow; a finite result clears an `ERANGE`
    // underflow. `EDOM` survives, and a NaN never forces the range error.
    if p.real.is_infinite() || p.imag.is_infinite() {
        if errno == 0 {
            errno = ERANGE;
        }
    } else if errno == ERANGE {
        errno = 0;
    }
    match errno {
        EDOM => {
            crate::tstate::set_string(
                exceptions::zero_division_error(),
                b"0.0 to a negative or complex power",
            );
            core::ptr::null_mut()
        }
        ERANGE => {
            crate::tstate::set_string(exceptions::overflow_error(), b"complex exponentiation");
            core::ptr::null_mut()
        }
        _ => from_c_complex_value(p),
    }
}

unsafe extern "C" fn complex_neg(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: `v` is a complex per the slot contract.
    from_c_complex_value(_Py_c_neg(unsafe { cval(v) }))
}

unsafe extern "C" fn complex_pos(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: the header read is within the object.
    unsafe {
        if core::ptr::eq(header::type_of(v), &raw mut PyComplex_Type) {
            return v;
        }
        from_c_complex_value(cval(v))
    }
}

unsafe extern "C" fn complex_abs(v: *mut PyObject) -> *mut PyObject {
    // SAFETY: `v` is a complex per the slot contract.
    unsafe {
        let (result, errno) = c_abs(cval(v));
        if errno == ERANGE {
            crate::tstate::set_string(exceptions::overflow_error(), b"absolute value too large");
            return core::ptr::null_mut();
        }
        float_::from_double(result)
    }
}

unsafe extern "C" fn complex_bool(v: *mut PyObject) -> c_int {
    // SAFETY: `v` is a complex per the slot contract.
    unsafe {
        let c = cval(v);
        (c.real != 0.0 || c.imag != 0.0) as c_int
    }
}

/// `complex_richcompare`: only equality is defined.
///
/// # Safety
///
/// `v` must be a complex; `w` must be null or a valid `PyObject`.
unsafe extern "C" fn complex_richcompare(
    v: *mut PyObject,
    w: *mut PyObject,
    op: c_int,
) -> *mut PyObject {
    if op != Py_EQ && op != Py_NE {
        return singletons::not_implemented();
    }
    // SAFETY: the reads follow the type checks.
    unsafe {
        let i = cval(v);
        let equal = if long::is_long(w) {
            if i.imag == 0.0 {
                // Compare the real part as a float against the int.
                let j = float_::from_double(i.real);
                if j.is_null() {
                    return core::ptr::null_mut();
                }
                return float_::float_richcompare(j, w, op);
            }
            false
        } else if float_::is_float(w) {
            i.real == float_::value(w) && i.imag == 0.0
        } else if is_complex(w) {
            let j = cval(w);
            i.real == j.real && i.imag == j.imag
        } else {
            return singletons::not_implemented();
        };
        PyBool_FromLong((equal == (op == Py_EQ)) as c_int)
    }
}

unsafe extern "C" fn complex_hash(v: *mut PyObject) -> isize {
    // SAFETY: `v` is a complex per the slot contract; the hash reads only
    // its value.
    unsafe {
        let c = cval(v);
        let hashreal = float_::_Py_HashDouble(v, c.real) as usize;
        if hashreal == usize::MAX {
            return -1;
        }
        let hashimag = float_::_Py_HashDouble(v, c.imag) as usize;
        if hashimag == usize::MAX {
            return -1;
        }
        // A zero imaginary part leaves the real part's hash, so equal
        // values of different types hash equally.
        let mut combined = hashreal.wrapping_add(HASH_IMAG.wrapping_mul(hashimag));
        if combined == usize::MAX {
            combined = usize::MAX - 1;
        }
        combined as isize
    }
}

// ------------------------------------------------------------------ repr

/// `complex_repr` as a string: the imaginary part with `j`, parenthesised
/// with the real part unless the real part is +0.
///
/// # Safety
///
/// `v` must point to a `PyComplexObject`.
pub unsafe fn repr_string(v: *const PyObject) -> String {
    // SAFETY: `cval` reads the object's fixed offset.
    let c = unsafe { cval(v) };
    if c.real == 0.0 && c.real.is_sign_positive() {
        let im =
            double_to_string(c.imag, b'r', 0, 0, core::ptr::null_mut()).expect("repr formatting");
        format!("{im}j")
    } else {
        let re =
            double_to_string(c.real, b'r', 0, 0, core::ptr::null_mut()).expect("repr formatting");
        let im = double_to_string(c.imag, b'r', 0, PY_DTSF_SIGN, core::ptr::null_mut())
            .expect("repr formatting");
        format!("({re}{im}j)")
    }
}

// ---------------------------------------------------------------- format

/// `format_complex_internal`: both parts through `format_float_internal`'s
/// number machinery, the imaginary part always signed, parens and `j`
/// around them, width padding outside everything.
fn format_complex_internal(re: f64, im: f64, format: &float_::FormatSpec) -> Option<String> {
    use float_::{LocaleType, calc_number_widths, fill_number, locale_info, pad};
    if format.precision > i32::MAX as isize {
        float_::value_error("precision too big");
        return None;
    }
    if format.fill_char == '0' {
        float_::value_error("Zero padding is not allowed in complex format specifier");
        return None;
    }
    if format.align == '=' {
        float_::value_error("'=' alignment flag is not allowed in complex format specifier");
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
    let mut add_parens = false;
    let mut skip_re = false;
    if type_ == '\0' {
        // Omitted type: like str().
        type_ = 'r';
        default_precision = 0;
        if re == 0.0 && re.is_sign_positive() {
            skip_re = true;
        } else {
            add_parens = true;
        }
    }
    if type_ == 'n' {
        type_ = 'g';
    }
    let mut precision = format.precision as i32;
    if precision < 0 {
        precision = default_precision;
    } else if type_ == 'r' {
        type_ = 'g';
    }
    let re_body = double_to_string(re, type_ as u8, precision, flags, core::ptr::null_mut())?;
    let im_body = double_to_string(im, type_ as u8, precision, flags, core::ptr::null_mut())?;
    let (re_rest, re_sign_char) = re_body
        .strip_prefix('-')
        .map_or((re_body.as_str(), '\0'), |r| (r, '-'));
    let (im_rest, im_sign_char) = im_body
        .strip_prefix('-')
        .map_or((im_body.as_str(), '\0'), |r| (r, '-'));
    let locale = locale_info(if format.type_ == 'n' {
        LocaleType::Current
    } else {
        format.thousands_separators
    });
    // The parts render unpadded; the caller's width pads the whole.
    let mut tmp_format = *format;
    tmp_format.fill_char = '\0';
    tmp_format.align = '<';
    tmp_format.width = -1;
    let re_parts = calc_number_widths(re_rest, re_sign_char, &tmp_format, &locale);
    if !skip_re {
        tmp_format.sign = '+';
    }
    let im_parts = calc_number_widths(im_rest, im_sign_char, &tmp_format, &locale);
    let mut content = String::new();
    if add_parens {
        content.push('(');
    }
    if !skip_re {
        content.push_str(&fill_number(re_rest, &re_parts, &tmp_format));
    }
    content.push_str(&fill_number(im_rest, &im_parts, &tmp_format));
    content.push('j');
    if add_parens {
        content.push(')');
    }
    Some(pad(&content, format))
}

/// `complex.__format__` (`_PyComplex_FormatAdvancedWriter`): parse `spec`,
/// check the presentation type, render. `None` returns carry a set
/// exception.
pub fn format_value(re: f64, im: f64, spec: &str) -> Option<String> {
    let format = parse_format_spec(spec, "complex", '\0', '>')?;
    if !matches!(format.type_, '\0' | 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | 'n') {
        float_::unknown_presentation_type(format.type_, "complex");
        return None;
    }
    format_complex_internal(re, im, &format)
}

// ------------------------------------------------------------ type object

unsafe extern "C" fn complex_repr(self_: *mut PyObject) -> *mut PyObject {
    // SAFETY: the object is a complex per the slot contract.
    unsafe { crate::types::unicode::str_from_utf8(&repr_string(self_)) }
}

const fn complex_type() -> PyTypeObject {
    let mut t = zero_type();
    t.tp_repr = Some(complex_repr);
    t.tp_name = c"complex".as_ptr();
    t.tp_basicsize = coil_rt_abi::cp312::complex::SIZEOF as isize;
    t.tp_as_number = &raw mut COMPLEX_AS_NUMBER;
    t.tp_hash = Some(complex_hash);
    t.tp_richcompare = Some(complex_richcompare);
    t.tp_flags = Py_TPFLAGS_DEFAULT | Py_TPFLAGS_BASETYPE;
    t.tp_free = Some(crate::alloc::pymem::PyObject_Free);
    t
}

/// `complex`'s number slots, CPython's `complex_as_number` except
/// `nb_divmod`, which needs tuples and arrives with them.
static mut COMPLEX_AS_NUMBER: PyNumberMethods = PyNumberMethods {
    nb_add: Some(complex_add),
    nb_subtract: Some(complex_sub),
    nb_multiply: Some(complex_mul),
    nb_remainder: None,
    nb_divmod: None,
    nb_power: Some(complex_pow),
    nb_negative: Some(complex_neg),
    nb_positive: Some(complex_pos),
    nb_absolute: Some(complex_abs),
    nb_bool: Some(complex_bool),
    nb_invert: None,
    nb_lshift: None,
    nb_rshift: None,
    nb_and: None,
    nb_xor: None,
    nb_or: None,
    nb_int: None,
    nb_reserved: core::ptr::null_mut(),
    nb_float: None,
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
    nb_floor_divide: None,
    nb_true_divide: Some(complex_div),
    nb_inplace_floor_divide: None,
    nb_inplace_true_divide: None,
    nb_index: None,
    nb_matrix_multiply: None,
    nb_inplace_matrix_multiply: None,
};

/// The `complex` type object (CPython's `PyComplex_Type`). `tp_str`
/// stays unset so `str()` falls back to the repr, as in CPython; the
/// `real`/`imag` members and `tp_new` wait for descriptors and argument
/// parsing.
#[unsafe(no_mangle)]
pub static mut PyComplex_Type: PyTypeObject = complex_type();

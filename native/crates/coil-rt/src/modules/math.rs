//! `math`: CPython 3.12's `mathmodule.c` — every function, the `e`, `pi`,
//! `tau`, `inf` and `nan` constants, and CPython's exact error behaviour
//! (`math domain error`, `math range error`, the per-function messages
//! and the `__index__`/`__float__` argument protocols). The one-argument
//! and two-argument transcendental wrappers follow CPython's FUNC1/FUNC2
//! error discipline around the platform libm; `erf`/`erfc` use the
//! FDLIBM rational approximations (the algorithm behind CPython's libm
//! on every platform it ships for); and `gamma`, `lgamma`, `fsum`,
//! `hypot`, `dist`, `sumprod`, `isqrt`, `factorial`, `comb`, `perm`,
//! `gcd` and `lcm` are CPython's own exact algorithms.

use coil_capi_sys::{
    _PyCFunctionFast, _PyCFunctionFastWithKeywords, METH_FASTCALL, METH_KEYWORDS, METH_O,
    PyCFunction, PyMethodDef, PyObject,
};

use crate::modules::{
    self, is_exact_float, is_exact_long, keywords_arity_error, missing_argument_error, number_add,
    number_index, number_multiply, positional_error,
};
use crate::object::singletons;
use crate::tstate;
use crate::types::exc as exceptions;
use crate::types::float_;
use crate::types::long;
use crate::types::tuple;

/// `math.pi`
pub const PI: f64 = core::f64::consts::PI;
/// `math.e`
pub const E: f64 = core::f64::consts::E;
/// `math.tau`
pub const TAU: f64 = core::f64::consts::TAU;
/// `math.inf`
/// CPython's `radToDeg`/`degToRad` (the multiply is a single rounding).
pub const RAD_TO_DEG: f64 = 180.0 / PI;
pub const DEG_TO_RAD: f64 = PI / 180.0;
pub const INF: f64 = f64::INFINITY;
/// `math.nan`
pub const NAN: f64 = f64::NAN;

fn domain_error() {
    tstate::set_string(exceptions::value_error(), b"math domain error");
}

fn range_error() {
    tstate::set_string(exceptions::overflow_error(), b"math range error");
}

/// FUNC1: a one-argument double wrapper; a NaN from a non-NaN argument
/// is a domain error, and an infinity from a finite argument either
/// overflows (`can_overflow`) or marks a pole.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn math_1(
    arg: *mut PyObject,
    func: impl Fn(f64) -> f64,
    can_overflow: bool,
) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    let r = func(x);
    if r.is_nan() {
        if !x.is_nan() {
            domain_error();
            return core::ptr::null_mut();
        }
    } else if r.is_infinite() && x.is_finite() {
        if can_overflow {
            range_error();
        } else {
            domain_error();
        }
        return core::ptr::null_mut();
    }
    float_::PyFloat_FromDouble(r)
}

/// The double of `math_1`'s argument.
///
/// # Safety
///
/// As `math_1`; `args` holds `nargs` valid objects.
unsafe fn math_args2(args: &[*mut PyObject], name: &str) -> Option<(f64, f64)> {
    if args.len() != 2 {
        positional_error(name, 2, 2, args.len());
        return None;
    }
    // SAFETY: the conversions follow the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(args[0]) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return None;
    }
    let y = unsafe { float_::PyFloat_AsDouble(args[1]) };
    if y == -1.0 && !tstate::occurred().is_null() {
        return None;
    }
    Some((x, y))
}

/// FUNC2: a two-argument double wrapper; a NaN from NaN-free arguments
/// is a domain error and an infinity from finite arguments a range
/// error, the pairs of special arguments pass through.
///
/// # Safety
///
/// As `math_1`; `args` holds `nargs` valid objects.
unsafe fn math_2(
    args: &[*mut PyObject],
    func: impl Fn(f64, f64) -> f64,
    name: &str,
) -> *mut PyObject {
    let Some((x, y)) = (unsafe { math_args2(args, name) }) else {
        return core::ptr::null_mut();
    };
    let r = func(x, y);
    if r.is_nan() {
        if !x.is_nan() && !y.is_nan() {
            domain_error();
            return core::ptr::null_mut();
        }
    } else if r.is_infinite() && x.is_finite() && y.is_finite() {
        range_error();
        return core::ptr::null_mut();
    }
    float_::PyFloat_FromDouble(r)
}

macro_rules! func1 {
    ($name:ident, $overflow:expr, $rust:ident) => {
        unsafe extern "C" fn $name(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
            // SAFETY: the argument is a valid object.
            unsafe { math_1(arg, f64::$rust, $overflow) }
        }
    };
}

func1!(math_acos, false, acos);
func1!(math_acosh, false, acosh);
func1!(math_asin, false, asin);
func1!(math_asinh, false, asinh);
func1!(math_atan, false, atan);
func1!(math_atanh, false, atanh);
func1!(math_cbrt, false, cbrt);
func1!(math_cos, false, cos);
func1!(math_cosh, true, cosh);
func1!(math_exp, true, exp);
func1!(math_exp2, true, exp2);
func1!(math_expm1, true, exp_m1);
func1!(math_log1p, false, ln_1p);
func1!(math_sin, false, sin);
func1!(math_sinh, true, sinh);
func1!(math_sqrt, false, sqrt);
func1!(math_tan, false, tan);
func1!(math_tanh, false, tanh);

// ------------------------------------------------ libm ports of CPython's

/// `m_sinpi`: sin(pi*x) accurate for x integral or near an integer,
/// which the gamma reflection formula needs.
fn m_sinpi(x: f64) -> f64 {
    let y = x.abs() % 2.0;
    let r = match (2.0 * y).round() as i32 {
        0 => (PI * y).sin(),
        1 => (PI * (y - 0.5)).cos(),
        // -sin(pi*(y-1.0)) would give -0.0 where 0.0 belongs.
        2 => (PI * (1.0 - y)).sin(),
        3 => -(PI * (y - 1.5)).cos(),
        _ => (PI * (y - 2.0)).sin(),
    };
    r * 1.0f64.copysign(x)
}

const LANCZOS_N: usize = 13;
const LANCZOS_G: f64 = 6.024_680_040_776_73;
const LANCZOS_G_MINUS_HALF: f64 = 5.524_680_040_776_73;
const LOGPI: f64 = 1.144729885849400174143427351353058711647;
const LANCZOS_NUM: [f64; LANCZOS_N] = [
    23_531_376_880.410_76,
    42_919_803_642.649_1,
    35_711_959_237.355_67,
    17_921_034_426.037_21,
    6_039_542_586.352_028,
    1_439_720_407.311_721_6,
    248_874_557.862_054_17,
    31_426_415.585_400_194,
    2_876_370.628_935_372_5,
    186_056.265_395_223_48,
    8_071.672_002_365_816,
    210.824_277_751_579_36,
    2.506_628_274_631_000_2,
];
const LANCZOS_DEN: [f64; LANCZOS_N] = [
    0.0,
    39916800.0,
    120543840.0,
    150917976.0,
    105258076.0,
    45995730.0,
    13339535.0,
    2637558.0,
    357423.0,
    32670.0,
    1925.0,
    66.0,
    1.0,
];

fn lanczos_sum(x: f64) -> f64 {
    let mut num = 0.0;
    let mut den = 0.0;
    if x < 5.0 {
        for i in (0..LANCZOS_N).rev() {
            num = num * x + LANCZOS_NUM[i];
            den = den * x + LANCZOS_DEN[i];
        }
    } else {
        for i in 0..LANCZOS_N {
            num = num / x + LANCZOS_NUM[i];
            den = den / x + LANCZOS_DEN[i];
        }
    }
    num / den
}

/// `m_tgamma`: CPython's gamma, special cases and Lanczos evaluation
/// (the result carries `EDOM`/`ERANGE` as the `_dom`/`_range` flag).
fn m_tgamma(x: f64) -> (f64, bool, bool) {
    // (value, domain, range)
    if !x.is_finite() {
        if x.is_nan() || x > 0.0 {
            return (x, false, false);
        }
        return (f64::NAN, true, false);
    }
    if x == 0.0 {
        return (f64::INFINITY.copysign(x), true, false);
    }
    if x == x.floor() {
        if x < 0.0 {
            return (f64::NAN, true, false);
        }
        if x <= NGAMMA_INTEGRAL as f64 {
            return (GAMMA_INTEGRAL[x as usize - 1], false, false);
        }
    }
    let absx = x.abs();
    if absx < 1e-20 {
        let r = 1.0 / x;
        return (r, false, r.is_infinite());
    }
    if absx > 200.0 {
        if x < 0.0 {
            return (0.0 / m_sinpi(x), false, false);
        }
        return (f64::INFINITY, false, true);
    }
    let y = absx + LANCZOS_G_MINUS_HALF;
    let z = if absx > LANCZOS_G_MINUS_HALF {
        let q = y - absx;
        q - LANCZOS_G_MINUS_HALF
    } else {
        let q = y - LANCZOS_G_MINUS_HALF;
        q - absx
    };
    let z = z * LANCZOS_G / y;
    let mut r;
    if x < 0.0 {
        r = -PI / m_sinpi(absx) / absx * y.exp() / lanczos_sum(absx);
        r -= z * r;
        if absx < 140.0 {
            r /= y.powf(absx - 0.5);
        } else {
            let sqrtpow = y.powf(absx / 2.0 - 0.25);
            r /= sqrtpow;
            r /= sqrtpow;
        }
    } else {
        r = lanczos_sum(absx) / y.exp();
        r += z * r;
        if absx < 140.0 {
            r *= y.powf(absx - 0.5);
        } else {
            let sqrtpow = y.powf(absx / 2.0 - 0.25);
            r *= sqrtpow;
            r *= sqrtpow;
        }
    }
    (r, false, r.is_infinite())
}

/// `m_lgamma`: CPython's log-gamma.
fn m_lgamma(x: f64) -> (f64, bool, bool) {
    if !x.is_finite() {
        if x.is_nan() {
            return (x, false, false);
        }
        return (f64::INFINITY, false, false);
    }
    if x == x.floor() && x <= 2.0 {
        if x <= 0.0 {
            return (f64::INFINITY, true, false);
        }
        return (0.0, false, false);
    }
    let absx = x.abs();
    if absx < 1e-20 {
        return (-absx.ln(), false, false);
    }
    let mut r = lanczos_sum(absx).ln() - LANCZOS_G;
    r += (absx - 0.5) * ((absx + LANCZOS_G - 0.5).ln() - 1.0);
    if x < 0.0 {
        r = LOGPI - m_sinpi(absx).abs().ln() - absx.ln() - r;
    }
    (r, false, r.is_infinite())
}

const NGAMMA_INTEGRAL: usize = 23;
const GAMMA_INTEGRAL: [f64; NGAMMA_INTEGRAL] = [
    1.0,
    1.0,
    2.0,
    6.0,
    24.0,
    120.0,
    720.0,
    5040.0,
    40320.0,
    362880.0,
    3628800.0,
    39916800.0,
    479001600.0,
    6227020800.0,
    87178291200.0,
    1307674368000.0,
    20922789888000.0,
    355687428096000.0,
    6402373705728000.0,
    121645100408832000.0,
    2432902008176640000.0,
    51090942171709440000.0,
    1124000727777607680000.0,
];

/// FUNC1A: like FUNC1 but with the domain/range outcome carried by the
/// wrapper itself, which gamma and lgamma need.
unsafe extern "C" fn math_gamma(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    let (r, dom, range) = m_tgamma(x);
    if dom {
        domain_error();
        return core::ptr::null_mut();
    }
    if r.is_nan() && !x.is_nan() {
        domain_error();
        return core::ptr::null_mut();
    }
    if range && x.is_finite() {
        range_error();
        return core::ptr::null_mut();
    }
    float_::PyFloat_FromDouble(r)
}

unsafe extern "C" fn math_lgamma(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    let (r, dom, range) = m_lgamma(x);
    if dom {
        domain_error();
        return core::ptr::null_mut();
    }
    if r.is_nan() && !x.is_nan() {
        domain_error();
        return core::ptr::null_mut();
    }
    if range && x.is_finite() {
        range_error();
        return core::ptr::null_mut();
    }
    float_::PyFloat_FromDouble(r)
}

/// `m_atan2`: CPython's platform-independent atan2 specials.
fn m_atan2(y: f64, x: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if y.is_infinite() {
        if x.is_infinite() {
            if 1.0f64.copysign(x) == 1.0 {
                return (0.25 * PI).copysign(y);
            }
            return (0.75 * PI).copysign(y);
        }
        return (0.5 * PI).copysign(y);
    }
    if x.is_infinite() || y == 0.0 {
        if 1.0f64.copysign(x) == 1.0 {
            return 0.0f64.copysign(y);
        }
        return PI.copysign(y);
    }
    y.atan2(x)
}

/// `m_remainder`: CPython's IEEE 754-style remainder, tie-to-even.
fn m_remainder(x: f64, y: f64) -> f64 {
    if x.is_finite() && y.is_finite() {
        if y == 0.0 {
            return f64::NAN;
        }
        let (absx, absy) = (x.abs(), y.abs());
        let m = absx % absy;
        let c = absy - m;
        let r = if m < c {
            m
        } else if m > c {
            -c
        } else {
            m - 2.0 * (0.5 * (absx - m) % absy)
        };
        return 1.0f64.copysign(x) * r;
    }
    if x.is_nan() {
        return x;
    }
    if y.is_nan() {
        return y;
    }
    if x.is_infinite() {
        return f64::NAN;
    }
    // y is infinite
    x
}

// -------------------------------------------------- erf/erfc (FDLIBM port)

// FDLIBM's double erf/erfc (the rational approximations FreeBSD's msun,
// glibc and macOS ship), transcribed for the runtime without a libm
// erf; accuracy is that of the original (about half an ulp).
const ERX: f64 = 8.450_629_115_104_675e-1;
const EFX: f64 = 1.283_791_670_955_126e-1;
const EFX8: f64 = 1.027_033_336_764_100_7;
const PP: [f64; 5] = [
    1.283_791_670_955_125_6e-1,
    -3.250_421_072_470_015e-1,
    -2.848_174_957_559_851e-2,
    -5.770_270_296_489_442e-3,
    -2.376_301_665_665_016_3e-5,
];
const QQ: [f64; 6] = [
    1.0,
    3.979_172_239_591_553_5e-1,
    6.502_224_998_876_73e-2,
    5.081_306_281_875_766e-3,
    1.324_947_380_043_216_4e-4,
    -3.960_228_278_775_368e-6,
];
const PA: [f64; 7] = [
    -2.362_118_560_752_659_4e-3,
    4.148_561_186_837_483_3e-1,
    -3.722_078_760_357_013e-1,
    3.183_466_199_011_617_5e-1,
    -1.108_946_942_823_966_8e-1,
    3.547_830_432_561_823_6e-2,
    -2.166_375_594_868_791e-3,
];
const QA: [f64; 7] = [
    1.0,
    1.064_208_804_008_442_3e-1,
    5.403_979_177_021_71e-1,
    7.182_865_441_419_627e-2,
    1.261_712_198_087_616_4e-1,
    1.363_708_391_202_905e-2,
    1.198_449_984_679_910_7e-2,
];
const RA: [f64; 8] = [
    -9.864_944_034_847_148e-3,
    -6.938_585_727_071_818e-1,
    -1.055_862_622_532_329_1e1,
    -6.237_533_245_032_600_6e1,
    -1.623_966_694_625_734_7e2,
    -1.846_050_929_067_110_4e2,
    -8.128_743_550_630_66e1,
    -9.814_329_344_169_145,
];
const SA: [f64; 9] = [
    1.0,
    1.965_127_166_743_925_7e1,
    1.376_577_541_435_190_4e2,
    4.345_658_774_752_292_3e2,
    6.453_872_717_332_679e2,
    4.290_081_400_275_678_3e2,
    1.086_350_055_417_794_4e2,
    6.570_249_770_319_282,
    -6.042_441_521_485_81e-2,
];
const RB: [f64; 7] = [
    -9.864_942_924_700_1e-3,
    -7.992_832_376_805_23e-1,
    -1.775_795_491_775_475_2e1,
    -1.606_363_848_558_219_2e2,
    -6.375_664_433_683_896e2,
    -1.025_095_131_611_077_2e3,
    -4.835_191_916_086_514e2,
];
const SB: [f64; 8] = [
    1.0,
    3.033_806_074_348_246e1,
    3.257_925_129_965_739e2,
    1.536_729_586_084_437e3,
    3.199_858_219_508_595_5e3,
    2.553_050_406_433_164_4e3,
    4.745_285_412_069_553_7e2,
    -2.244_095_244_658_582e1,
];

fn polevl(x: f64, coef: &[f64]) -> f64 {
    let mut r = coef[coef.len() - 1];
    for &c in coef[..coef.len() - 1].iter().rev() {
        r = r * x + c;
    }
    r
}

/// FDLIBM's `erf`.
pub(crate) fn erf(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return 1.0f64.copysign(x);
    }
    let ix = (x.to_bits() >> 32) as u32 & 0x7fff_ffff;
    // C reads the high word as a signed int; the sign tests below
    // depend on that.
    let hx = (x.to_bits() >> 32) as i32;
    if ix < 0x3feb0000 {
        // |x| < 0.84375
        if ix < 0x3e300000 {
            // |x| < 2**-28
            if ix < 0x00800000 {
                return (8.0 * x + EFX8 * x) / 8.0;
            }
            return x + EFX * x;
        }
        let z = x * x;
        let r = polevl(z, &PP);
        let s = polevl(z, &QQ);
        let y = r / s;
        return x + x * y;
    }
    if ix < 0x3ff40000 {
        // 0.84375 <= |x| < 1.25
        let s = x.abs() - 1.0;
        let p = polevl(s, &PA);
        let q = polevl(s, &QA);
        if hx >= 0 {
            return ERX + p / q;
        }
        return -ERX - p / q;
    }
    if ix >= 0x40180000 {
        // |x| >= 6
        const TINY: f64 = 1e-300;
        if hx >= 0 {
            return 1.0 - TINY;
        }
        return TINY - 1.0;
    }
    let x = x.abs();
    let s = 1.0 / (x * x);
    let (r, sn) = if ix < 0x4006db6e {
        (polevl(s, &RA), polevl(s, &SA))
    } else {
        (polevl(s, &RB), polevl(s, &SB))
    };
    let z = f64::from_bits(x.to_bits() & 0xffff_ffff_0000_0000);
    let r = (-z * z - 0.5625).exp() * ((z - x) * (z + x) + r / sn).exp();
    if hx >= 0 {
        return 1.0 - r / x;
    }
    r / x - 1.0
}

/// FDLIBM's `erfc`.
pub(crate) fn erfc(x: f64) -> f64 {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        if x > 0.0 {
            return 0.0;
        }
        return 2.0;
    }
    let ix = (x.to_bits() >> 32) as u32 & 0x7fff_ffff;
    // C reads the high word as a signed int; the sign tests below
    // depend on that.
    let hx = (x.to_bits() >> 32) as i32;
    if ix < 0x3feb0000 {
        // |x| < 0.84375
        if ix < 0x3c700000 {
            // |x| < 2**-56
            return 1.0 - x;
        }
        let z = x * x;
        let r = polevl(z, &PP);
        let s = polevl(z, &QQ);
        let y = r / s;
        if hx < 0x3fd00000 {
            // x < 1/4
            return 1.0 - (x + x * y);
        }
        let mut r = x * y;
        r += x - 0.5;
        return 0.5 - r;
    }
    if ix < 0x3ff40000 {
        // 0.84375 <= |x| < 1.25
        let s = x.abs() - 1.0;
        let p = polevl(s, &PA);
        let q = polevl(s, &QA);
        if hx >= 0 {
            let z = 1.0 - ERX;
            return z - p / q;
        }
        let z = ERX + p / q;
        return 1.0 + z;
    }
    const TINY: f64 = 1e-300;
    if ix < 0x403c0000 {
        // |x| < 28
        let x = x.abs();
        let s = 1.0 / (x * x);
        if ix < 0x4006db6d {
            let r = polevl(s, &RA);
            let sn = polevl(s, &SA);
            let z = f64::from_bits(x.to_bits() & 0xffff_ffff_0000_0000);
            let r = (-z * z - 0.5625).exp() * ((z - x) * (z + x) + r / sn).exp();
            if hx > 0 {
                return r / x;
            }
            return 2.0 - r / x;
        }
        if hx < 0 && ix >= 0x40180000 {
            return 2.0 - TINY;
        }
        let r = polevl(s, &RB);
        let sn = polevl(s, &SB);
        let z = f64::from_bits(x.to_bits() & 0xffff_ffff_0000_0000);
        let r = (-z * z - 0.5625).exp() * ((z - x) * (z + x) + r / sn).exp();
        if hx > 0 {
            return r / x;
        }
        return 2.0 - r / x;
    }
    if hx > 0 {
        return TINY * TINY;
    }
    2.0 - TINY
}

unsafe extern "C" fn math_erf(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    unsafe { math_1(arg, erf, false) }
}

unsafe extern "C" fn math_erfc(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    unsafe { math_1(arg, erfc, false) }
}

// ------------------------------------------------------- simple wrappers

unsafe extern "C" fn math_fabs(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    unsafe { math_1(arg, f64::abs, false) }
}

/// `math.degrees`: the argument-clinic form, a plain conversion and
/// multiply with no `math_1` error checking (an overflowing product
/// is an infinity, not an error).
unsafe extern "C" fn math_degrees(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    float_::PyFloat_FromDouble(x * RAD_TO_DEG)
}

/// `math.radians`: as `math_degrees`.
unsafe extern "C" fn math_radians(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    float_::PyFloat_FromDouble(x * DEG_TO_RAD)
}

unsafe extern "C" fn math_atan2(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe { math_2(args, m_atan2, "atan2") }
}

unsafe extern "C" fn math_copysign(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: as `math_atan2`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe { math_2(args, |x, y| x.copysign(y), "copysign") }
}

unsafe extern "C" fn math_remainder(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: as `math_atan2`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    unsafe { math_2(args, m_remainder, "remainder") }
}

unsafe extern "C" fn math_fmod(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: as `math_atan2`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    let Some((x, y)) = (unsafe { math_args2(args, "fmod") }) else {
        return core::ptr::null_mut();
    };
    // fmod(x, +/-inf) returns x for finite x.
    if y.is_infinite() && x.is_finite() {
        return float_::PyFloat_FromDouble(x);
    }
    let r = x % y;
    if r.is_nan() {
        if !x.is_nan() && !y.is_nan() {
            domain_error();
            return core::ptr::null_mut();
        }
        return float_::PyFloat_FromDouble(r);
    }
    float_::PyFloat_FromDouble(r)
}

/// `math.pow`'s wrapper: IEEE specials decided up front, then the finite
/// case through libm with the 0**negative domain rule.
unsafe extern "C" fn math_pow(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: as `math_atan2`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    let Some((x, y)) = (unsafe { math_args2(args, "pow") }) else {
        return core::ptr::null_mut();
    };
    let r: f64;
    if !x.is_finite() || !y.is_finite() {
        if x.is_nan() {
            r = if y == 0.0 { 1.0 } else { x };
        } else if y.is_nan() {
            r = if x == 1.0 { 1.0 } else { y };
        } else if x.is_infinite() {
            let odd_y = y.is_finite() && y.abs() % 2.0 == 1.0;
            if y > 0.0 {
                r = if odd_y { x } else { x.abs() };
            } else if y == 0.0 {
                r = 1.0;
            } else {
                r = if odd_y { 0.0f64.copysign(x) } else { 0.0 };
            }
        } else {
            // y is infinite, x finite
            if x.abs() == 1.0 {
                r = 1.0;
            } else if y > 0.0 && x.abs() > 1.0 {
                r = y;
            } else if y < 0.0 && x.abs() < 1.0 {
                r = -y;
            } else {
                r = 0.0;
            }
        }
    } else {
        let result = x.powf(y);
        if !result.is_finite() {
            if result.is_nan() || x == 0.0 {
                domain_error();
                return core::ptr::null_mut();
            }
            range_error();
            return core::ptr::null_mut();
        }
        r = result;
    }
    float_::PyFloat_FromDouble(r)
}

// ------------------------------------------------------------- log family

/// `loghelper`: ints convert exactly, a too-large int through
/// `_PyLong_Frexp` (`log(x * 2**e) == log(x) + log(2) * e`), everything
/// else through `math_1`.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn loghelper(arg: *mut PyObject, func: impl Fn(f64) -> f64) -> *mut PyObject {
    // SAFETY: the type read is within the object.
    if unsafe { long::is_long(arg) } {
        let v = arg.cast::<long::PyLongObject>();
        // SAFETY: the sign reads follow the int layout.
        if unsafe { long::is_negative(v) } || unsafe { long::is_zero(v) } {
            domain_error();
            return core::ptr::null_mut();
        }
        // SAFETY: the conversion reads the int layout.
        let mut x = unsafe { long::PyLong_AsDouble(arg) };
        if x == -1.0 && !tstate::occurred().is_null() {
            if !core::ptr::eq(tstate::occurred(), exceptions::overflow_error()) {
                return core::ptr::null_mut();
            }
            tstate::clear();
            let mut e: isize = 0;
            // SAFETY: the frexp call reads the int layout.
            x = unsafe { long::_PyLong_Frexp(arg, &raw mut e) };
            if x == -1.0 && !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            return float_::PyFloat_FromDouble(func(x) + func(2.0) * e as f64);
        }
        return float_::PyFloat_FromDouble(func(x));
    }
    // SAFETY: the argument is a valid object.
    unsafe { math_1(arg, func, false) }
}

unsafe extern "C" fn math_log(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if !(1..=2).contains(&nargs) {
        positional_error("log", 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the argument is a valid object.
    let num = unsafe { loghelper(args[0], f64::ln) };
    if num.is_null() || nargs == 1 {
        return num;
    }
    // SAFETY: the argument is a valid object.
    let den = unsafe { loghelper(args[1], f64::ln) };
    if den.is_null() {
        // SAFETY: `num` is an owned float result.
        unsafe { exceptions::release_object(num) };
        return core::ptr::null_mut();
    }
    // Both sides are floats; `PyNumber_TrueDivide` never fails on them.
    // SAFETY: the two reads follow the float layout of owned objects.
    let quotient = unsafe { float_::value(num) / float_::value(den) };
    // SAFETY: both temporaries are owned.
    unsafe {
        exceptions::release_object(num);
        exceptions::release_object(den);
    }
    float_::PyFloat_FromDouble(quotient)
}

unsafe extern "C" fn math_log2(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    unsafe { loghelper(arg, f64::log2) }
}

unsafe extern "C" fn math_log10(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    unsafe { loghelper(arg, f64::log10) }
}

// ----------------------------------------------------- special predicates

/// `isfinite`/`isinf`/`isnan` share one body over a predicate.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn predicate(arg: *mut PyObject, func: impl Fn(f64) -> bool) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    if func(x) {
        singletons::true_()
    } else {
        singletons::false_()
    }
}

unsafe extern "C" fn math_isfinite(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    unsafe { predicate(arg, |x| x.is_finite()) }
}

unsafe extern "C" fn math_isinf(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    unsafe { predicate(arg, |x| x.is_infinite()) }
}

unsafe extern "C" fn math_isnan(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    unsafe { predicate(arg, |x| x.is_nan()) }
}

// ------------------------------------------------------- floor/ceil/trunc

/// `math.floor`: floats exactly; ints (and their subtypes) through
/// `__floor__`, which returns them unchanged; then the float protocol.
unsafe extern "C" fn math_floor(_self: *mut PyObject, number: *mut PyObject) -> *mut PyObject {
    // SAFETY: the type reads are within the object.
    unsafe {
        if is_exact_float(number) {
            return long::PyLong_FromDouble(float_::value(number).floor());
        }
        // The `__floor__` special lookup: only int defines one in this
        // runtime, and int.__floor__ returns the int itself.
        if long::is_long(number) {
            exceptions::retain_object(number);
            return number;
        }
        let x = float_::PyFloat_AsDouble(number);
        if x == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        long::PyLong_FromDouble(x.floor())
    }
}

unsafe extern "C" fn math_ceil(_self: *mut PyObject, number: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `math_floor`.
    unsafe {
        if long::is_long(number) {
            exceptions::retain_object(number);
            return number;
        }
        let x = float_::PyFloat_AsDouble(number);
        if x == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        long::PyLong_FromDouble(x.ceil())
    }
}

unsafe extern "C" fn math_trunc(_self: *mut PyObject, number: *mut PyObject) -> *mut PyObject {
    // SAFETY: as `math_floor`.
    unsafe {
        if is_exact_float(number) {
            return long::PyLong_FromDouble(float_::value(number).trunc());
        }
        if long::is_long(number) {
            exceptions::retain_object(number);
            return number;
        }
        let name = crate::util::cstr((*crate::object::header::type_of(number)).tp_name);
        tstate::set_string(
            exceptions::type_error(),
            format!("type {name} doesn't define __trunc__ method").as_bytes(),
        );
        core::ptr::null_mut()
    }
}

// -------------------------------------------------------- frexp/ldexp/modf

unsafe extern "C" fn math_frexp(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let mut x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    let i: i32;
    if x.is_nan() || x.is_infinite() || x == 0.0 {
        i = 0;
    } else {
        let (m, e) = long::frexp(x);
        x = m;
        i = e;
    }
    let mantissa = float_::PyFloat_FromDouble(x);
    let exp = long::PyLong_FromLong(i);
    let pair = tuple::from_array(&[mantissa, exp]);
    // SAFETY: both temporaries are owned (small ints are immortal and
    // skipped).
    unsafe {
        exceptions::release_object(mantissa);
        exceptions::release_object(exp);
    }
    pair
}

unsafe extern "C" fn math_ldexp(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs != 2 {
        positional_error("ldexp", 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(args[0]) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: the index conversion reads the int layout.
    // SAFETY: the layout check reads the header.
    let exp: i64 = if unsafe { long::is_long(args[1]) } {
        let mut overflow = 0;
        // SAFETY: the conversion reads the int layout.
        let value = unsafe { long::PyLong_AsLongLongAndOverflow(args[1], &raw mut overflow) };
        if overflow < 0 {
            i64::MIN
        } else if overflow > 0 {
            i64::MAX
        } else {
            value
        }
    } else {
        tstate::set_string(
            exceptions::type_error(),
            b"Expected an int as second argument to ldexp.",
        );
        return core::ptr::null_mut();
    };
    let r = if x == 0.0 || !x.is_finite() {
        x
    } else if exp > i32::MAX as i64 {
        range_error();
        return core::ptr::null_mut();
    } else if exp < i32::MIN as i64 {
        0.0f64.copysign(x)
    } else {
        let scaled = long::ldexp(x, exp as i32);
        if scaled.is_infinite() {
            range_error();
            return core::ptr::null_mut();
        }
        scaled
    };
    float_::PyFloat_FromDouble(r)
}

unsafe extern "C" fn math_modf(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    let (frac, intpart) = if !x.is_finite() {
        if x.is_infinite() {
            (0.0f64.copysign(x), x)
        } else {
            (x, x)
        }
    } else {
        let y = x.trunc();
        (x - y, y)
    };
    let f = float_::PyFloat_FromDouble(frac);
    let i = float_::PyFloat_FromDouble(intpart);
    let pair = tuple::from_array(&[f, i]);
    // SAFETY: both temporaries are owned.
    unsafe {
        exceptions::release_object(f);
        exceptions::release_object(i);
    }
    pair
}

// ------------------------------------------------------------------- fsum

/// `math.fsum`: Shewchuk's exact partials (`msum4.py` as CPython has it).
unsafe extern "C" fn math_fsum(_self: *mut PyObject, seq: *mut PyObject) -> *mut PyObject {
    // SAFETY: the iterable protocol reads stay within the objects.
    unsafe {
        let iter = modules::get_iter(seq);
        if iter.is_null() {
            return core::ptr::null_mut();
        }
        let mut partials: Vec<f64> = Vec::new();
        let mut special_sum = 0.0;
        let mut inf_sum = 0.0;
        loop {
            let item = modules::iter_next(iter);
            if item.is_null() {
                if !tstate::occurred().is_null() {
                    return core::ptr::null_mut();
                }
                break;
            }
            let x = float_::PyFloat_AsDouble(item);
            let failed = x == -1.0 && !tstate::occurred().is_null();
            exceptions::release_object(item);
            if failed {
                return core::ptr::null_mut();
            }
            let xsave = x;
            let mut kept: Vec<f64> = Vec::with_capacity(partials.len());
            let mut x = x;
            for &y in &partials {
                let (mut x2, mut y2) = (x, y);
                if x2.abs() < y2.abs() {
                    core::mem::swap(&mut x2, &mut y2);
                }
                let hi = x2 + y2;
                let yr = hi - x2;
                let lo = y2 - yr;
                if lo != 0.0 {
                    kept.push(lo);
                }
                x = hi;
            }
            partials = kept;
            if x != 0.0 {
                if !x.is_finite() {
                    if xsave.is_finite() {
                        tstate::set_string(
                            exceptions::overflow_error(),
                            b"intermediate overflow in fsum",
                        );
                        return core::ptr::null_mut();
                    }
                    if xsave.is_infinite() {
                        inf_sum += xsave;
                    }
                    special_sum += xsave;
                    partials.clear();
                } else {
                    partials.push(x);
                }
            }
        }
        if special_sum != 0.0 {
            if inf_sum.is_nan() {
                tstate::set_string(exceptions::value_error(), b"-inf + inf in fsum");
                return core::ptr::null_mut();
            }
            return float_::PyFloat_FromDouble(special_sum);
        }
        let mut hi = 0.0;
        let mut lo = 0.0;
        if !partials.is_empty() {
            let mut n = partials.len();
            hi = partials[n - 1];
            n -= 1;
            while n > 0 {
                let x = hi;
                n -= 1;
                let y = partials[n];
                hi = x + y;
                let yr = hi - x;
                lo = y - yr;
                if lo != 0.0 {
                    break;
                }
            }
            if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0))
            {
                let y = lo * 2.0;
                let x = hi + y;
                let yr = x - hi;
                if y == yr {
                    hi = x;
                }
            }
        }
        float_::PyFloat_FromDouble(hi)
    }
}

// -------------------------------------------------- hypot/dist/DoubleLength

#[derive(Clone, Copy)]
struct DoubleLength {
    hi: f64,
    lo: f64,
}

/// `dl_sum`: the error-free transformation of a sum (Algorithm 3.1).
fn dl_sum(a: f64, b: f64) -> DoubleLength {
    let x = a + b;
    let z = x - a;
    let y = (a - (x - z)) + (b - z);
    DoubleLength { hi: x, lo: y }
}

/// `dl_fast_sum`: compensated summation of two floats (Algorithm 1.1),
/// the caller guaranteeing `|a| >= |b|`.
fn dl_fast_sum(a: f64, b: f64) -> DoubleLength {
    let x = a + b;
    let y = (a - x) + b;
    DoubleLength { hi: x, lo: y }
}

/// `dl_mul`: the error-free transformation of a product through fma
/// (Algorithm 3.5).
fn dl_mul(x: f64, y: f64) -> DoubleLength {
    let z = x * y;
    let zz = x.mul_add(y, -z);
    DoubleLength { hi: z, lo: zz }
}

#[derive(Clone, Copy)]
struct TripleLength {
    hi: f64,
    lo: f64,
    tiny: f64,
}

const TL_ZERO: TripleLength = TripleLength {
    hi: 0.0,
    lo: 0.0,
    tiny: 0.0,
};

/// `tl_fma` (Algorithm 5.10 with SumKVert for K=3).
fn tl_fma(x: f64, y: f64, total: TripleLength) -> TripleLength {
    let pr = dl_mul(x, y);
    let sm = dl_sum(total.hi, pr.hi);
    let r1 = dl_sum(total.lo, pr.lo);
    let r2 = dl_sum(r1.hi, sm.lo);
    TripleLength {
        hi: sm.hi,
        lo: r2.hi,
        tiny: total.tiny + r1.lo + r2.lo,
    }
}

/// `tl_to_d`.
fn tl_to_d(total: TripleLength) -> f64 {
    let last = dl_sum(total.lo, total.hi);
    total.tiny + last.lo + last.hi
}

/// `vector_norm`: the accurate n-dimensional norm (scaling, exact
/// squaring, compensated summation, sqrt differential correction).
fn vector_norm(n: usize, vec: &mut [f64], max: f64, found_nan: bool) -> f64 {
    if max.is_infinite() {
        return max;
    }
    if found_nan {
        return f64::NAN;
    }
    if max == 0.0 || n <= 1 {
        return max;
    }
    let max_e = long::frexp(max).1;
    if max_e < -1023 {
        // ldexp(1.0, -max_e) would overflow; convert subnormals to normals.
        for x in vec.iter_mut() {
            *x /= f64::MIN_POSITIVE;
        }
        return f64::MIN_POSITIVE * vector_norm(n, vec, max / f64::MIN_POSITIVE, found_nan);
    }
    let scale = long::ldexp(1.0, -max_e);
    let mut csum = 1.0;
    let mut frac1 = 0.0;
    let mut frac2 = 0.0;
    for &raw in vec.iter() {
        let x = raw * scale;
        let pr = dl_mul(x, x);
        let sm = dl_fast_sum(csum, pr.hi);
        csum = sm.hi;
        frac1 += pr.lo;
        frac2 += sm.lo;
    }
    let mut h = (csum - 1.0 + (frac1 + frac2)).sqrt();
    let pr = dl_mul(-h, h);
    let sm = dl_fast_sum(csum, pr.hi);
    csum = sm.hi;
    frac1 += pr.lo;
    frac2 += sm.lo;
    let x = csum - 1.0 + (frac1 + frac2);
    h += x / (2.0 * h);
    h / scale
}

/// Collect a coordinate vector and its maximum and NaN flag; the
/// `name` shapes nothing here (the errors carry up from the float
/// protocol).
///
/// # Safety
///
/// `objs` are valid objects.
unsafe fn coordinates(objs: &[*mut PyObject]) -> Result<(Vec<f64>, f64, bool), ()> {
    let mut coords = Vec::with_capacity(objs.len());
    let mut max = 0.0;
    let mut found_nan = false;
    for &obj in objs {
        // SAFETY: the conversion follows the float protocol.
        let mut x = unsafe { float_::PyFloat_AsDouble(obj) };
        if x == -1.0 && !tstate::occurred().is_null() {
            return Err(());
        }
        x = x.abs();
        found_nan |= x.is_nan();
        if x > max {
            max = x;
        }
        coords.push(x);
    }
    Ok((coords, max, found_nan))
}

unsafe extern "C" fn math_hypot(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the arguments are valid objects.
    let (mut coords, max, found_nan) = match unsafe { coordinates(args) } {
        Ok(triple) => triple,
        Err(()) => return core::ptr::null_mut(),
    };
    let result = vector_norm(args.len(), &mut coords, max, found_nan);
    float_::PyFloat_FromDouble(result)
}

unsafe extern "C" fn math_dist(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs != 2 {
        positional_error("dist", 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the pair handling reads the sequences through their
    // protocols.
    unsafe {
        // `PySequence_Tuple` for both points: iterables are materialized
        // first, with the items kept as new references for the norm below.
        let p_items = match modules::collect_iterable(args[0]) {
            Ok(items) => items,
            Err(()) => return core::ptr::null_mut(),
        };
        let q_items = match modules::collect_iterable(args[1]) {
            Ok(items) => items,
            Err(()) => {
                for item in &p_items {
                    exceptions::release_object(*item);
                }
                return core::ptr::null_mut();
            }
        };
        let fail = || {
            for item in &p_items {
                exceptions::release_object(*item);
            }
            for item in &q_items {
                exceptions::release_object(*item);
            }
        };
        if p_items.len() != q_items.len() {
            fail();
            tstate::set_string(
                exceptions::value_error(),
                b"both points must have the same number of dimensions",
            );
            return core::ptr::null_mut();
        }
        let mut diffs = Vec::with_capacity(p_items.len());
        let mut max = 0.0;
        let mut found_nan = false;
        let mut bad = false;
        for i in 0..p_items.len() {
            let px = float_::PyFloat_AsDouble(p_items[i]);
            if px == -1.0 && !tstate::occurred().is_null() {
                bad = true;
                break;
            }
            let qx = float_::PyFloat_AsDouble(q_items[i]);
            if qx == -1.0 && !tstate::occurred().is_null() {
                bad = true;
                break;
            }
            let x = (px - qx).abs();
            found_nan |= x.is_nan();
            if x > max {
                max = x;
            }
            diffs.push(x);
        }
        fail();
        if bad {
            return core::ptr::null_mut();
        }
        let result = vector_norm(diffs.len(), &mut diffs, max, found_nan);
        float_::PyFloat_FromDouble(result)
    }
}

// ---------------------------------------------------------------- sumprod

/// `math.sumprod`: the int fast path in machine integers, the float
/// path in triple-length arithmetic, everything else through the
/// number slots.
unsafe extern "C" fn math_sumprod(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs != 2 {
        positional_error("sumprod", 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the iterable protocol reads stay within the objects.
    unsafe {
        let p_it = modules::get_iter(args[0]);
        if p_it.is_null() {
            return core::ptr::null_mut();
        }
        let q_it = modules::get_iter(args[1]);
        if q_it.is_null() {
            return core::ptr::null_mut();
        }
        let mut total = singletons::get_small_int(0);
        let mut int_total: i64 = 0;
        let mut int_in_use = false;
        let mut int_enabled = true;
        let mut flt_total = TL_ZERO;
        let mut flt_in_use = false;
        let mut flt_enabled = true;
        loop {
            let p_i = modules::iter_next(p_it);
            let p_stopped = p_i.is_null();
            if p_stopped && !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            let q_i = modules::iter_next(q_it);
            let q_stopped = q_i.is_null();
            if q_stopped && !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            if p_stopped != q_stopped {
                tstate::set_string(exceptions::value_error(), b"Inputs are not the same length");
                return core::ptr::null_mut();
            }
            let finished = p_stopped;

            if int_enabled {
                if !finished && is_exact_long(p_i) && is_exact_long(q_i) {
                    let mut overflow = 0;
                    let int_p = long::PyLong_AsLongLongAndOverflow(p_i, &raw mut overflow);
                    if overflow != 0 {
                        // fall through to the finalize below
                    } else {
                        let int_q = long::PyLong_AsLongLongAndOverflow(q_i, &raw mut overflow);
                        if overflow != 0 {
                            // fall through
                        } else if let Some(product) = int_p.checked_mul(int_q)
                            && let Some(summed) = int_total.checked_add(product)
                        {
                            int_total = summed;
                            int_in_use = true;
                            exceptions::release_object(p_i);
                            exceptions::release_object(q_i);
                            continue;
                        }
                    }
                }
                int_enabled = false;
                if int_in_use {
                    let term = long::PyLong_FromLongLong(int_total);
                    if term.is_null() {
                        return core::ptr::null_mut();
                    }
                    let new_total = number_add(total, term);
                    exceptions::release_object(term);
                    exceptions::release_object(total);
                    if new_total.is_null() {
                        return core::ptr::null_mut();
                    }
                    total = new_total;
                    int_total = 0;
                    int_in_use = false;
                }
            }

            if flt_enabled {
                if !finished {
                    let p_is_float = is_exact_float(p_i);
                    let q_is_float = is_exact_float(q_i);
                    let pair = if p_is_float && q_is_float {
                        Some((float_::value(p_i), float_::value(q_i)))
                    } else if p_is_float
                        && (is_exact_long(q_i) || crate::types::bool_::is_bool(q_i))
                    {
                        // SAFETY: the conversion reads the int layout.
                        let v = long::PyLong_AsDouble(q_i);
                        if v == -1.0 && !tstate::occurred().is_null() {
                            tstate::clear();
                            None
                        } else {
                            Some((float_::value(p_i), v))
                        }
                    } else if q_is_float
                        && (is_exact_long(p_i) || crate::types::bool_::is_bool(p_i))
                    {
                        // SAFETY: the conversion reads the int layout.
                        let v = long::PyLong_AsDouble(p_i);
                        if v == -1.0 && !tstate::occurred().is_null() {
                            tstate::clear();
                            None
                        } else {
                            Some((v, float_::value(q_i)))
                        }
                    } else {
                        None
                    };
                    if let Some((flt_p, flt_q)) = pair {
                        let new_total = tl_fma(flt_p, flt_q, flt_total);
                        if new_total.hi.is_finite() {
                            flt_total = new_total;
                            flt_in_use = true;
                            exceptions::release_object(p_i);
                            exceptions::release_object(q_i);
                            continue;
                        }
                    }
                }
                flt_enabled = false;
                if flt_in_use {
                    let term = float_::PyFloat_FromDouble(tl_to_d(flt_total));
                    if term.is_null() {
                        return core::ptr::null_mut();
                    }
                    let new_total = number_add(total, term);
                    exceptions::release_object(term);
                    exceptions::release_object(total);
                    if new_total.is_null() {
                        return core::ptr::null_mut();
                    }
                    total = new_total;
                    flt_total = TL_ZERO;
                    flt_in_use = false;
                }
            }

            if finished {
                return total;
            }
            let term = number_multiply(p_i, q_i);
            exceptions::release_object(p_i);
            exceptions::release_object(q_i);
            if term.is_null() {
                exceptions::release_object(total);
                return core::ptr::null_mut();
            }
            let new_total = number_add(total, term);
            exceptions::release_object(term);
            exceptions::release_object(total);
            if new_total.is_null() {
                return core::ptr::null_mut();
            }
            total = new_total;
        }
    }
}

// ------------------------------------------------------------------ prod

/// `math.prod`: int and float fast paths in machine arithmetic, then
/// the number slots. `start` is keyword-only with default 1, and no
/// keyword can reach a module function in this runtime yet.
unsafe extern "C" fn math_prod(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs != 1 {
        keywords_arity_error("prod", 1, 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the iterable and number protocol reads stay within the
    // objects.
    unsafe {
        let iter = modules::get_iter(args[0]);
        if iter.is_null() {
            return core::ptr::null_mut();
        }
        // The product starts as the int 1 (the keyword-only `start`
        // default); the paths below replace it on the first element.
        let out = prod_rest(iter);
        exceptions::release_object(iter);
        out
    }
}

/// The running product from `iter`, which the caller owns. CPython
/// drives this with `PyNumber_Multiply` after int and float fast paths;
/// the fast paths here keep exact ints and float order like theirs.
///
/// # Safety
///
/// `iter` is a valid iterator object.
unsafe fn prod_rest(iter: *mut PyObject) -> *mut PyObject {
    // Phase one: exact ints in machine arithmetic.
    let mut i_result: i64 = 1;
    loop {
        // SAFETY: the iterator protocol reads stay within the object.
        let item = unsafe { modules::iter_next(iter) };
        if item.is_null() {
            if !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            return long::PyLong_FromLongLong(i_result);
        }
        let mut overflow = 0;
        // SAFETY: the layout checks and conversions read the headers
        // and int layouts.
        if unsafe { is_exact_long(item) } {
            let b = unsafe { long::PyLong_AsLongLongAndOverflow(item, &raw mut overflow) };
            if overflow == 0
                && let Some(product) = i_result.checked_mul(b)
            {
                i_result = product;
                // SAFETY: the item is an owned reference.
                unsafe { exceptions::release_object(item) };
                continue;
            }
        }
        // SAFETY: the constructor only reads the machine int.
        let head = long::PyLong_FromLongLong(i_result);
        // SAFETY: the multiply goes through the number slots; both
        // sides are owned.
        let product = unsafe { number_multiply(head, item) };
        // SAFETY: both operands were owned here.
        unsafe {
            exceptions::release_object(head);
            exceptions::release_object(item);
        }
        if product.is_null() {
            return core::ptr::null_mut();
        }
        // The accumulator's type picks the next phase: a float product
        // continues in machine floats, anything else in the number
        // slots (True * 1 stays int, as CPython's does).
        if unsafe { is_exact_float(product) } {
            // SAFETY: the iterator is valid and the product owned.
            return unsafe { prod_rest_float(iter, product) };
        }
        // SAFETY: the iterator is valid and the product owned.
        return unsafe { prod_rest_generic(iter, product) };
    }
}

/// Phase two: a float accumulator in machine arithmetic.
///
/// # Safety
///
/// `iter` is a valid iterator; `start` an owned float.
unsafe fn prod_rest_float(iter: *mut PyObject, start: *mut PyObject) -> *mut PyObject {
    let mut f_result = unsafe { float_::value(start) };
    // SAFETY: `start` is an owned float.
    unsafe { exceptions::release_object(start) };
    loop {
        // SAFETY: the iterator protocol reads stay within the object.
        let item = unsafe { modules::iter_next(iter) };
        if item.is_null() {
            if !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            return float_::PyFloat_FromDouble(f_result);
        }
        let mut overflow = 0;
        // SAFETY: the layout checks and conversions read the headers
        // and int layouts.
        if unsafe { is_exact_float(item) } {
            f_result *= unsafe { float_::value(item) };
            // SAFETY: the item is an owned reference.
            unsafe { exceptions::release_object(item) };
            continue;
        }
        if unsafe { is_exact_long(item) } {
            let b = unsafe { long::PyLong_AsLongLongAndOverflow(item, &raw mut overflow) };
            if overflow == 0 {
                f_result *= b as f64;
                // SAFETY: the item is an owned reference.
                unsafe { exceptions::release_object(item) };
                continue;
            }
        }
        // SAFETY: the constructor only reads the machine float.
        let head = float_::PyFloat_FromDouble(f_result);
        // SAFETY: the multiply goes through the number slots; both
        // sides are owned.
        let product = unsafe { number_multiply(head, item) };
        // SAFETY: both operands were owned here.
        unsafe {
            exceptions::release_object(head);
            exceptions::release_object(item);
        }
        if product.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: the iterator is valid and the product owned.
        return unsafe { prod_rest_generic(iter, product) };
    }
}

/// Phase three: everything through the number slots.
///
/// # Safety
///
/// `iter` is a valid iterator; `result` an owned object.
unsafe fn prod_rest_generic(iter: *mut PyObject, mut result: *mut PyObject) -> *mut PyObject {
    loop {
        // SAFETY: the iterator protocol reads stay within the object.
        let item = unsafe { modules::iter_next(iter) };
        if item.is_null() {
            if !tstate::occurred().is_null() {
                // SAFETY: the partial product is owned here.
                unsafe { exceptions::release_object(result) };
                return core::ptr::null_mut();
            }
            return result;
        }
        // SAFETY: the multiply goes through the number slots; both
        // sides are owned.
        let product = unsafe { number_multiply(result, item) };
        // SAFETY: both operands were owned here.
        unsafe {
            exceptions::release_object(result);
            exceptions::release_object(item);
        }
        if product.is_null() {
            return core::ptr::null_mut();
        }
        result = product;
    }
}

// --------------------------------------------------------------- isclose

/// `math.isclose`: the weak Boost test; `rel_tol`/`abs_tol` are
/// keyword-only with their defaults, which is what this runtime can
/// apply with keywords unreachable.
unsafe extern "C" fn math_isclose(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 2 {
        keywords_arity_error("isclose", 2, 2, 4, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    if nargs < 1 {
        missing_argument_error("isclose", "a", 1);
        return core::ptr::null_mut();
    }
    if nargs < 2 {
        missing_argument_error("isclose", "b", 2);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the conversions follow the float protocol.
    unsafe {
        let a = float_::PyFloat_AsDouble(args[0]);
        if a == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let b = float_::PyFloat_AsDouble(args[1]);
        if b == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let rel_tol = 1e-09;
        let abs_tol = 0.0;
        if a == b {
            return singletons::true_();
        }
        if a.is_infinite() || b.is_infinite() {
            return singletons::false_();
        }
        let diff = (b - a).abs();
        let close = diff <= (rel_tol * b).abs() || diff <= (rel_tol * a).abs() || diff <= abs_tol;
        if close {
            singletons::true_()
        } else {
            singletons::false_()
        }
    }
}

// ------------------------------------------------------- nextafter and ulp

/// `nextafter`'s single step in bit space, C99's `nextafter`.
/// FDLIBM's `nextafter`, which CPython's `math.nextafter` and `ulp`
/// call: the raw bit pattern walk with NaN propagation and the
/// zero-magnitude special case.
fn next_after_one(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return x + y;
    }
    if x == y {
        return y;
    }
    if x == 0.0 {
        // The smallest denormal with y's sign.
        return f64::from_bits(1).copysign(y);
    }
    let x_bits = x.to_bits();
    let y_bits = y.to_bits();
    let hx = (x_bits >> 32) as u32 as i32;
    let hy = (y_bits >> 32) as u32 as i32;
    let lx = x_bits as u32;
    // The overflow at the exponent limit raises a range error in
    // CPython's wrapper; the single-step call never reaches it.
    let (hx, lx) = if hx >= 0 {
        if hx > hy || (hx == hy && lx > (y_bits as u32)) {
            if lx == 0 {
                (hx - 1, u32::MAX)
            } else {
                (hx, lx - 1)
            }
        } else if lx == u32::MAX {
            (hx + 1, 0)
        } else {
            (hx, lx + 1)
        }
    } else if hy >= 0 || hx > hy || (hx == hy && lx > (y_bits as u32)) {
        if lx == 0 {
            (hx - 1, u32::MAX)
        } else {
            (hx, lx - 1)
        }
    } else if lx == u32::MAX {
        (hx + 1, 0)
    } else {
        (hx, lx + 1)
    };
    f64::from_bits(((hx as u64) << 32) | lx as u64)
}

/// `math.nextafter`: the bit-space walk for `steps` steps. `steps` is
/// keyword-only with default None (one step).
unsafe extern "C" fn math_nextafter(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs != 2 {
        keywords_arity_error("nextafter", 2, 2, 3, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the kwds contract holds when non-null.
    if unsafe { modules::reject_keywords(kwds) }.is_err() {
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the conversions follow the float protocol.
    unsafe {
        let x = float_::PyFloat_AsDouble(args[0]);
        if x == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let y = float_::PyFloat_AsDouble(args[1]);
        if y == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        // steps is None: the default single step.
        float_::PyFloat_FromDouble(next_after_one(x, y))
    }
}

/// `math.ulp`.
unsafe extern "C" fn math_ulp(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the float protocol.
    let mut x = unsafe { float_::PyFloat_AsDouble(arg) };
    if x == -1.0 && !tstate::occurred().is_null() {
        return core::ptr::null_mut();
    }
    if x.is_nan() {
        return float_::PyFloat_FromDouble(x);
    }
    x = x.abs();
    if x.is_infinite() {
        return float_::PyFloat_FromDouble(x);
    }
    let x2 = next_after_one(x, f64::INFINITY);
    let ulp = if x2.is_infinite() {
        // x is the largest float: ulp is the gap to its predecessor.
        x - next_after_one(x, f64::NEG_INFINITY)
    } else {
        x2 - x
    };
    float_::PyFloat_FromDouble(ulp)
}

// ------------------------------------------------------------- magnitude

/// The 30-bit digit arithmetic the integer functions share with
/// `long.rs`, in vector form.
const SHIFT: usize = 30;
const MASK: u32 = (1 << SHIFT) - 1;

fn mag_trim(mut mag: Vec<u32>) -> Vec<u32> {
    while let Some(&0) = mag.last() {
        mag.pop();
    }
    mag
}

fn mag_from_u64(mut v: u64) -> Vec<u32> {
    let mut mag = Vec::new();
    while v > 0 {
        mag.push((v & MASK as u64) as u32);
        v >>= SHIFT;
    }
    mag
}

fn mag_to_u128(mag: &[u32]) -> Option<u128> {
    let mut v: u128 = 0;
    for &d in mag.iter().rev() {
        v = v.checked_shl(SHIFT as u32)? | d as u128;
        if v > u128::from(u64::MAX) {
            return None;
        }
    }
    Some(v)
}

/// `_PyLong_NumBits`.
fn mag_numbits(mag: &[u32]) -> usize {
    match mag.last() {
        None => 0,
        Some(&top) => (mag.len() - 1) * SHIFT + (32 - top.leading_zeros() as usize),
    }
}

fn mag_lshift(mag: &[u32], bits: usize) -> Vec<u32> {
    let wordshift = bits / SHIFT;
    let remshift = bits % SHIFT;
    let mut out = vec![0u32; mag.len() + wordshift + 1];
    let mut carry: u32 = 0;
    for i in 0..mag.len() {
        let combined = (mag[i] as u64) << remshift | carry as u64;
        out[i + wordshift] = (combined & MASK as u64) as u32;
        carry = (combined >> SHIFT) as u32;
    }
    out[mag.len() + wordshift] = carry;
    mag_trim(out)
}

fn mag_rshift(mag: &[u32], bits: usize) -> Vec<u32> {
    let wordshift = bits / SHIFT;
    let remshift = bits % SHIFT;
    if wordshift >= mag.len() {
        return Vec::new();
    }
    let mut out = vec![0u32; mag.len() - wordshift];
    for i in 0..out.len() {
        let mut v = (mag[i + wordshift] >> remshift) as u64;
        if remshift > 0 && i + wordshift + 1 < mag.len() {
            v |= (mag[i + wordshift + 1] as u64) << (SHIFT - remshift);
        }
        out[i] = (v & MASK as u64) as u32;
    }
    mag_trim(out)
}

/// The magnitude of an int object (`PyLongObject`'s digits, copied).
///
/// # Safety
///
/// `obj` must be an int.
unsafe fn mag_of(obj: *mut PyObject) -> Vec<u32> {
    // SAFETY: the digit read follows the int layout.
    unsafe { long::digits(obj.cast::<long::PyLongObject>()) }.to_vec()
}

/// An int object from a magnitude (`PyLong_FromUnsignedLongLong`'s role
/// for the arbitrary case).
///
/// # Safety
///
/// The caller passes digits below `MASK`.
unsafe fn from_mag(mag: &[u32]) -> *mut PyObject {
    // SAFETY: `from_mag` normalizes and allocates.
    unsafe { long::from_mag(1, mag) }
}

/// `_approximate_isqrt`: an `a` with `(a-1)**2 < n < (a+1)**2` for
/// `2**62 <= n < 2**64`.
fn approximate_isqrt(n: u64) -> u32 {
    const TAB: [u8; 192] = [
        128, 129, 130, 131, 132, 133, 134, 135, 136, 137, 138, 139, 140, 141, 142, 143, 144, 144,
        145, 146, 147, 148, 149, 150, 151, 151, 152, 153, 154, 155, 156, 156, 157, 158, 159, 160,
        160, 161, 162, 163, 164, 164, 165, 166, 167, 167, 168, 169, 170, 170, 171, 172, 173, 173,
        174, 175, 176, 176, 177, 178, 179, 179, 180, 181, 181, 182, 183, 183, 184, 185, 186, 186,
        187, 188, 188, 189, 190, 190, 191, 192, 192, 193, 194, 194, 195, 196, 196, 197, 198, 198,
        199, 200, 200, 201, 201, 202, 203, 203, 204, 205, 205, 206, 206, 207, 208, 208, 209, 210,
        210, 211, 211, 212, 213, 213, 214, 214, 215, 216, 216, 217, 217, 218, 219, 219, 220, 220,
        221, 221, 222, 223, 223, 224, 224, 225, 225, 226, 227, 227, 228, 228, 229, 229, 230, 230,
        231, 232, 232, 233, 233, 234, 234, 235, 235, 236, 237, 237, 238, 238, 239, 239, 240, 240,
        241, 241, 242, 242, 243, 243, 244, 244, 245, 246, 246, 247, 247, 248, 248, 249, 249, 250,
        250, 251, 251, 252, 252, 253, 253, 254, 254, 255, 255, 255,
    ];
    let mut u = TAB[(n >> 56) as usize - 64] as u32;
    u = (u << 7) + (n >> 41) as u32 / u;
    (u << 15) + ((n >> 17) / u as u64) as u32
}

/// `math.isqrt`: CPython's exact algorithm — the direct approximation
/// below 2**64, Newton's method in magnitudes above.
unsafe extern "C" fn math_isqrt(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the index protocol reads the object.
    unsafe {
        let n = number_index(arg);
        if n.is_null() {
            return core::ptr::null_mut();
        }
        let negative = long::is_negative(n.cast::<long::PyLongObject>());
        let zero = long::is_zero(n.cast::<long::PyLongObject>());
        if negative {
            exceptions::release_object(n);
            tstate::set_string(
                exceptions::value_error(),
                b"isqrt() argument must be nonnegative",
            );
            return core::ptr::null_mut();
        }
        if zero {
            exceptions::release_object(n);
            return long::PyLong_FromLong(0);
        }
        let mag = mag_of(n);
        let c = (mag_numbits(&mag) - 1) / 2;
        if c <= 31 {
            let shift = 31 - c;
            // SAFETY: the magnitude fits u64 by the bit count.
            let m = mag_to_u128(&mag).unwrap() as u64;
            exceptions::release_object(n);
            let mut u = approximate_isqrt(m << (2 * shift)) >> shift;
            if u as u64 * u as u64 > m {
                u -= 1;
            }
            return from_mag(&mag_from_u64(u as u64));
        }
        // Slow path: the first five iterations in machine arithmetic,
        // then Newton's method over magnitudes.
        let mut c_bit_length = 6;
        while (c >> c_bit_length) > 0 {
            c_bit_length += 1;
        }
        let mut d = c >> (c_bit_length - 5);
        let b = mag_rshift(&mag, 2 * c - 62);
        let m = mag_to_u128(&b).unwrap() as u64;
        let u = approximate_isqrt(m) >> (31 - d);
        let mut a = mag_from_u64(u as u64);
        for s in (0..=c_bit_length - 6).rev() {
            let e = d;
            d = c >> s;
            // q = (n >> 2*c - d - e + 1) // a
            let num = mag_rshift(&mag, 2 * c - d - e + 1);
            let q = mag_div(&num, &a);
            // a = (a << d - 1 - e) + q
            a = mag_trim(long::mag_add(&mag_lshift(&a, d - 1 - e), &q));
        }
        // The answer is a or a - 1.
        let sq = long::mag_mul(&a, &a);
        exceptions::release_object(n);
        if long::mag_cmp(&sq, &mag) == core::cmp::Ordering::Greater {
            return from_mag(&mag_sub_one(&a));
        }
        from_mag(&a)
    }
}

/// Floor division of normalized magnitudes (`l_divmod`'s quotient for
/// the non-negative case).
fn mag_div(a: &[u32], b: &[u32]) -> Vec<u32> {
    use core::cmp::Ordering::*;
    match long::mag_cmp(a, b) {
        Less => Vec::new(),
        Equal => vec![1],
        Greater => {
            if b.len() == 1 {
                let mut q = vec![0u32; a.len()];
                long::mag_divrem1(&mut q, a, b[0]);
                mag_trim(q)
            } else {
                long::mag_divrem(a, b).0
            }
        }
    }
}

/// Subtract one from a nonzero magnitude.
fn mag_sub_one(mag: &[u32]) -> Vec<u32> {
    let mut out = mag.to_vec();
    for d in out.iter_mut() {
        if *d > 0 {
            *d -= 1;
            break;
        }
        *d = MASK;
    }
    mag_trim(out)
}

// -------------------------------------------------------------- factorial

/// `factorial_partial_product`: product(range(start, stop, 2)) by
/// divide and conquer, `start`/`stop` odd, `stop > start`.
fn factorial_partial_product(start: u64, stop: u64, max_bits: u32) -> Vec<u32> {
    let num_operands = (stop - start) / 2;
    // The C fast path guards its u64 product with `num_operands *
    // max_bits <= 64`; u128 arithmetic gives the same bound headroom.
    if num_operands <= 8 * 64 && num_operands * max_bits as u64 <= 128 {
        let mut total: u128 = start as u128;
        let mut j = start + 2;
        while j < stop {
            total *= j as u128;
            j += 2;
        }
        let mut mag = Vec::new();
        while total > 0 {
            mag.push((total & MASK as u128) as u32);
            total >>= SHIFT;
        }
        return mag;
    }
    let midpoint = (start + num_operands) | 1;
    let left = factorial_partial_product(start, midpoint, (midpoint - 2).max(1).ilog2() + 1);
    let right = factorial_partial_product(midpoint, stop, max_bits);
    long::mag_mul(&left, &right)
}

/// `factorial_odd_part`: the product of every odd j <= n, by the
/// bit-splitting formula.
fn factorial_odd_part(n: u64) -> Vec<u32> {
    let mut inner: Vec<u32> = vec![1];
    let mut outer: Vec<u32> = vec![1];
    let mut upper: u64 = 3;
    let nbits = 64 - n.leading_zeros();
    for i in (0..nbits.saturating_sub(1)).rev() {
        let v = n >> i;
        if v <= 2 {
            continue;
        }
        let lower = upper;
        upper = (v + 1) | 1;
        let partial = factorial_partial_product(lower, upper, (upper - 2).max(1).ilog2() + 1);
        inner = long::mag_mul(&inner, &partial);
        outer = long::mag_mul(&outer, &inner);
    }
    outer
}

const SMALL_FACTORIALS: [u64; 21] = [
    1,
    1,
    2,
    6,
    24,
    120,
    720,
    5040,
    40320,
    362880,
    3628800,
    39916800,
    479001600,
    6227020800,
    87178291200,
    1307674368000,
    20922789888000,
    355687428096000,
    6402373705728000,
    121645100408832000,
    2432902008176640000,
];

/// `math.factorial`. The overflow bound is the C `long long`'s,
/// consistent on every platform this runtime builds for.
unsafe extern "C" fn math_factorial(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the index protocol.
    unsafe { factorial_impl(arg) }
}

/// The body of `math.factorial`, shared with `perm`'s `k is None` case.
///
/// # Safety
///
/// `arg` must be a valid object.
unsafe fn factorial_impl(arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the conversion follows the index protocol.
    unsafe {
        let mut overflow = 0;
        let x = long::PyLong_AsLongLongAndOverflow(arg, &raw mut overflow);
        if x == -1 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        if overflow > 0 {
            tstate::set_string(
                exceptions::overflow_error(),
                b"factorial() argument should not exceed 9223372036854775807",
            );
            return core::ptr::null_mut();
        }
        if overflow < 0 || x < 0 {
            tstate::set_string(
                exceptions::value_error(),
                b"factorial() not defined for negative values",
            );
            return core::ptr::null_mut();
        }
        if (x as usize) < SMALL_FACTORIALS.len() {
            return long::PyLong_FromUnsignedLongLong(SMALL_FACTORIALS[x as usize]);
        }
        let odd_part = factorial_odd_part(x as u64);
        let two_valuation = x as u64 - x.count_ones() as u64;
        from_mag(&mag_lshift(&odd_part, two_valuation as usize))
    }
}

// ------------------------------------------------------------ comb and perm

/// `perm_comb`: P(n, k) or C(n, k) by the recursive formulas, over
/// magnitudes; CPython's u64 fast paths are a speed device with the
/// same values, so the recursion serves both.
fn perm_comb(n: &[u32], k: u64, iscomb: bool) -> Vec<u32> {
    if k == 0 {
        return vec![1];
    }
    if k == 1 {
        return n.to_vec();
    }
    let j = k / 2;
    let mut a = perm_comb(n, j, iscomb);
    let n_minus_j = mag_sub(n, &mag_from_u64(j));
    let b = perm_comb(&n_minus_j, k - j, iscomb);
    a = long::mag_mul(&a, &b);
    if iscomb {
        let denom = perm_comb(&mag_from_u64(k), j, true);
        a = mag_div(&a, &denom);
    }
    a
}

/// Subtract `b` from `a`, which the callers guarantee to be `>= b`.
fn mag_sub(a: &[u32], b: &[u32]) -> Vec<u32> {
    debug_assert!(long::mag_cmp(a, b) != core::cmp::Ordering::Less);
    long::mag_sub(a, b)
}

/// `math.perm`: `n! / (n - k)!`, or `n!` when k is None.
unsafe extern "C" fn math_perm(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if !(1..=2).contains(&nargs) {
        positional_error("perm", 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the index protocol reads the objects.
    unsafe {
        if nargs == 1 || core::ptr::eq(args[1], singletons::none()) {
            return factorial_impl(args[0]);
        }
        let n = number_index(args[0]);
        if n.is_null() {
            return core::ptr::null_mut();
        }
        let k = number_index(args[1]);
        if k.is_null() {
            exceptions::release_object(n);
            return core::ptr::null_mut();
        }
        let negative_n = long::is_negative(n.cast::<long::PyLongObject>());
        let negative_k = long::is_negative(k.cast::<long::PyLongObject>());
        if negative_n {
            perm_error(n, k, b"n must be a non-negative integer");
            return core::ptr::null_mut();
        }
        if negative_k {
            perm_error(n, k, b"k must be a non-negative integer");
            return core::ptr::null_mut();
        }
        let n_mag = mag_of(n);
        let k_mag = mag_of(k);
        if long::mag_cmp(&n_mag, &k_mag) == core::cmp::Ordering::Less {
            release_pair(n, k);
            return long::PyLong_FromLong(0);
        }
        // SAFETY: the conversion reads the int layout.
        let mut overflow = 0;
        let ki = long::PyLong_AsLongLongAndOverflow(k, &raw mut overflow);
        if overflow > 0 {
            release_pair(n, k);
            tstate::set_string(
                exceptions::overflow_error(),
                b"k must not exceed 9223372036854775807",
            );
            return core::ptr::null_mut();
        }
        let result = perm_comb(&n_mag, ki as u64, false);
        release_pair(n, k);
        from_mag(&result)
    }
}

/// `math.comb`: the binomial coefficient.
unsafe extern "C" fn math_comb(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs != 2 {
        positional_error("comb", 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the index protocol reads the objects.
    unsafe {
        let n = number_index(args[0]);
        if n.is_null() {
            return core::ptr::null_mut();
        }
        let k = number_index(args[1]);
        if k.is_null() {
            exceptions::release_object(n);
            return core::ptr::null_mut();
        }
        if long::is_negative(n.cast::<long::PyLongObject>()) {
            perm_error(n, k, b"n must be a non-negative integer");
            return core::ptr::null_mut();
        }
        if long::is_negative(k.cast::<long::PyLongObject>()) {
            perm_error(n, k, b"k must be a non-negative integer");
            return core::ptr::null_mut();
        }
        let n_mag = mag_of(n);
        let k_mag = mag_of(k);
        // SAFETY: the conversions read the int layout.
        let mut overflow = 0;
        let ni = long::PyLong_AsLongLongAndOverflow(n, &raw mut overflow);
        if overflow == 0 {
            let ki = long::PyLong_AsLongLongAndOverflow(k, &raw mut overflow);
            if overflow != 0 || ki > ni {
                release_pair(n, k);
                return long::PyLong_FromLong(0);
            }
            let ki = ki.min(ni - ki);
            if ki == 0 || ki == 1 {
                // C(2n, 1) and friends: the original n, by perm_comb's
                // k == 1 base case.
                let result = if ki == 0 {
                    long::PyLong_FromLong(1)
                } else {
                    retain_copy(n)
                };
                release_pair(n, k);
                return result;
            }
            let result = perm_comb(&n_mag, ki as u64, true);
            release_pair(n, k);
            return from_mag(&result);
        }
        // n overflows i64: k = min(k, n - k)
        let diff = long::mag_sub(&n_mag, &k_mag);
        if long::mag_cmp(&diff, &k_mag) == core::cmp::Ordering::Less {
            // n - k < k, so min is n - k; it fits unless huge
            if mag_numbits(&diff) > 63 {
                release_pair(n, k);
                tstate::set_string(
                    exceptions::overflow_error(),
                    b"min(n - k, k) must not exceed 9223372036854775807",
                );
                return core::ptr::null_mut();
            }
            let result = perm_comb(&n_mag, mag_to_u128(&diff).unwrap() as u64, true);
            release_pair(n, k);
            return from_mag(&result);
        }
        if mag_numbits(&k_mag) > 63 {
            release_pair(n, k);
            tstate::set_string(
                exceptions::overflow_error(),
                b"min(n - k, k) must not exceed 9223372036854775807",
            );
            return core::ptr::null_mut();
        }
        let result = perm_comb(&n_mag, mag_to_u128(&k_mag).unwrap() as u64, true);
        release_pair(n, k);
        from_mag(&result)
    }
}

/// Set a perm/comb validation error after taking two index results.
///
/// # Safety
///
/// `n` and `k` are owned objects.
unsafe fn perm_error(n: *mut PyObject, k: *mut PyObject, message: &[u8]) {
    // SAFETY: both are owned objects.
    unsafe { release_pair(n, k) };
    // SAFETY: the message outlives the call.
    tstate::set_string(exceptions::value_error(), message);
}

/// Drop two owned objects together.
///
/// # Safety
///
/// Both are owned objects.
unsafe fn release_pair(n: *mut PyObject, k: *mut PyObject) {
    // SAFETY: both are owned objects.
    unsafe {
        exceptions::release_object(n);
        exceptions::release_object(k);
    }
}

/// A retained copy of an int.
///
/// # Safety
///
/// `n` is an owned object.
unsafe fn retain_copy(n: *mut PyObject) -> *mut PyObject {
    // SAFETY: `n` is an owned object.
    unsafe { exceptions::retain_object(n) };
    n
}

// -------------------------------------------------------------- gcd and lcm

/// Euclid on normalized magnitudes (`_PyLong_GCD`'s values; CPython
/// accelerates it with Lehmer's algorithm).
fn mag_gcd(mut a: Vec<u32>, mut b: Vec<u32>) -> Vec<u32> {
    use core::cmp::Ordering::*;
    if long::mag_cmp(&a, &b) == Less {
        core::mem::swap(&mut a, &mut b);
    }
    while !b.is_empty() {
        let r = if a.is_empty() {
            Vec::new()
        } else if b.len() == 1 {
            let mut q = vec![0u32; a.len()];
            let rem = long::mag_divrem1(&mut q, &a, b[0]);
            if rem == 0 { Vec::new() } else { vec![rem] }
        } else if long::mag_cmp(&a, &b) == Less {
            core::mem::swap(&mut a, &mut b);
            continue;
        } else {
            long::mag_divrem(&a, &b).1
        };
        a = b;
        b = r;
    }
    a
}

/// `math.gcd`: `__index__` on every argument, absolute values folded.
unsafe extern "C" fn math_gcd(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs == 0 {
        return long::PyLong_FromLong(0);
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the index protocol reads the objects.
    unsafe {
        let mut res = number_index(args[0]);
        if res.is_null() {
            return core::ptr::null_mut();
        }
        if nargs == 1 {
            let abs = abs_in_place(&mut res);
            if abs.is_null() {
                return core::ptr::null_mut();
            }
            return abs;
        }
        for &arg in &args[1..] {
            let x = number_index(arg);
            if x.is_null() {
                exceptions::release_object(res);
                return core::ptr::null_mut();
            }
            // gcd(x, 1) shortcuts the arithmetic for the rest.
            if is_small_one(res) {
                exceptions::release_object(x);
                continue;
            }
            let res_mag = mag_of(res);
            let x_mag = mag_of(x);
            let g = mag_gcd(res_mag, x_mag);
            exceptions::release_object(res);
            exceptions::release_object(x);
            res = from_mag(&g);
            if res.is_null() {
                return core::ptr::null_mut();
            }
        }
        res
    }
}

/// `math.lcm`.
unsafe extern "C" fn math_lcm(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs == 0 {
        return long::PyLong_FromLong(1);
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`.
    let args = unsafe { core::slice::from_raw_parts(args, nargs as usize) };
    // SAFETY: the index protocol reads the objects.
    unsafe {
        let mut res = number_index(args[0]);
        if res.is_null() {
            return core::ptr::null_mut();
        }
        if nargs == 1 {
            let abs = abs_in_place(&mut res);
            if abs.is_null() {
                return core::ptr::null_mut();
            }
            return abs;
        }
        for &arg in &args[1..] {
            let x = number_index(arg);
            if x.is_null() {
                exceptions::release_object(res);
                return core::ptr::null_mut();
            }
            if is_small_zero(res) {
                exceptions::release_object(x);
                continue;
            }
            let folded = long_lcm(res, x);
            if folded.is_null() {
                return core::ptr::null_mut();
            }
            res = folded;
        }
        res
    }
}

/// `long_lcm`: zero short-circuits, then `|a / gcd(a, b) * b|`.
///
/// # Safety
///
/// `a` and `b` are owned int objects; both are consumed.
unsafe fn long_lcm(a: *mut PyObject, b: *mut PyObject) -> *mut PyObject {
    // SAFETY: the sign reads follow the int layout.
    unsafe {
        if long::is_zero(a.cast::<long::PyLongObject>())
            || long::is_zero(b.cast::<long::PyLongObject>())
        {
            release_pair(a, b);
            return long::PyLong_FromLong(0);
        }
        let a_mag = mag_of(a);
        let b_mag = mag_of(b);
        let g = mag_gcd(a_mag.clone(), b_mag.clone());
        let f = mag_div(&a_mag, &g);
        let m = long::mag_mul(&f, &b_mag);
        release_pair(a, b);
        from_mag(&m)
    }
}

/// Whether an int is the small one.
///
/// # Safety
///
/// `obj` is an owned int.
unsafe fn is_small_one(obj: *mut PyObject) -> bool {
    // SAFETY: the compact read follows the int layout.
    unsafe {
        long::is_compact(obj.cast::<long::PyLongObject>())
            && long::compact_value(obj.cast::<long::PyLongObject>()) == 1
    }
}

/// Whether an int is the small zero.
///
/// # Safety
///
/// `obj` is an owned int.
unsafe fn is_small_zero(obj: *mut PyObject) -> bool {
    // SAFETY: as `is_small_one`.
    unsafe {
        long::is_compact(obj.cast::<long::PyLongObject>())
            && long::compact_value(obj.cast::<long::PyLongObject>()) == 0
    }
}

/// Replace an int with its absolute value (PyNumber_Absolute on one).
///
/// # Safety
///
/// `obj` is an owned pointer to an owned int.
unsafe fn abs_in_place(obj: &mut *mut PyObject) -> *mut PyObject {
    // SAFETY: the sign read follows the int layout.
    if unsafe { long::is_negative((*obj).cast::<long::PyLongObject>()) } {
        // SAFETY: the digit read follows the int layout.
        let mag = unsafe { mag_of(*obj) };
        // SAFETY: the object is owned by the caller.
        unsafe { exceptions::release_object(*obj) };
        // SAFETY: the digits were below `MASK`.
        *obj = unsafe { from_mag(&mag) };
    }
    *obj
}

// ------------------------------------------------------------ method table

macro_rules! method_def {
    // The plain form: the function's signature already matches the
    // `PyCFunction` arm of the union, so a typed local pins it and the
    // pointer coerces without a cast.
    ($name:literal, $func:ident, $flags:expr) => {{
        let func: PyCFunction = $func;
        PyMethodDef {
            ml_name: concat!($name, "\0").as_ptr() as *const core::ffi::c_char,
            ml_meth: Some(func),
            ml_flags: $flags,
            ml_doc: core::ptr::null(),
        }
    }};
    // The typed form for the other calling conventions: the local pins
    // the signature CPython compiled the function with, and transmute
    // reinterprets that pointer for the union field, matching the
    // call-site dispatch by ml_flags.
    ($name:literal, $func:ident, $ty:ty, $flags:expr) => {{
        let func: $ty = $func;
        PyMethodDef {
            ml_name: concat!($name, "\0").as_ptr() as *const core::ffi::c_char,
            ml_meth: Some(unsafe {
                // SAFETY: the dispatch in pycfunction.rs reads this
                // field back through exactly the $ty signature chosen
                // here, so every call happens at the function's real
                // type.
                core::mem::transmute::<$ty, PyCFunction>(func)
            }),
            ml_flags: $flags,
            ml_doc: core::ptr::null(),
        }
    }};
}

/// The `math` functions as CPython orders them, in the calling
/// conventions their signatures select; module creation binds them.
pub static MATH_METHODS: modules::MethodTable = modules::MethodTable(&[
    method_def!("acos", math_acos, METH_O),
    method_def!("acosh", math_acosh, METH_O),
    method_def!("asin", math_asin, METH_O),
    method_def!("asinh", math_asinh, METH_O),
    method_def!("atan", math_atan, METH_O),
    method_def!("atan2", math_atan2, _PyCFunctionFast, METH_FASTCALL),
    method_def!("atanh", math_atanh, METH_O),
    method_def!("cbrt", math_cbrt, METH_O),
    method_def!("ceil", math_ceil, METH_O),
    method_def!("copysign", math_copysign, _PyCFunctionFast, METH_FASTCALL),
    method_def!("cos", math_cos, METH_O),
    method_def!("cosh", math_cosh, METH_O),
    method_def!("degrees", math_degrees, METH_O),
    method_def!("dist", math_dist, _PyCFunctionFast, METH_FASTCALL),
    method_def!("erf", math_erf, METH_O),
    method_def!("erfc", math_erfc, METH_O),
    method_def!("exp", math_exp, METH_O),
    method_def!("exp2", math_exp2, METH_O),
    method_def!("expm1", math_expm1, METH_O),
    method_def!("fabs", math_fabs, METH_O),
    method_def!("factorial", math_factorial, METH_O),
    method_def!("floor", math_floor, METH_O),
    method_def!("fmod", math_fmod, _PyCFunctionFast, METH_FASTCALL),
    method_def!("frexp", math_frexp, METH_O),
    method_def!("fsum", math_fsum, METH_O),
    method_def!("gamma", math_gamma, METH_O),
    method_def!("gcd", math_gcd, _PyCFunctionFast, METH_FASTCALL),
    method_def!("hypot", math_hypot, _PyCFunctionFast, METH_FASTCALL),
    method_def!(
        "isclose",
        math_isclose,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!("isfinite", math_isfinite, METH_O),
    method_def!("isinf", math_isinf, METH_O),
    method_def!("isnan", math_isnan, METH_O),
    method_def!("isqrt", math_isqrt, METH_O),
    method_def!("lcm", math_lcm, _PyCFunctionFast, METH_FASTCALL),
    method_def!("ldexp", math_ldexp, _PyCFunctionFast, METH_FASTCALL),
    method_def!("lgamma", math_lgamma, METH_O),
    method_def!("log", math_log, _PyCFunctionFast, METH_FASTCALL),
    method_def!("log1p", math_log1p, METH_O),
    method_def!("log10", math_log10, METH_O),
    method_def!("log2", math_log2, METH_O),
    method_def!("modf", math_modf, METH_O),
    method_def!(
        "nextafter",
        math_nextafter,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!("pow", math_pow, _PyCFunctionFast, METH_FASTCALL),
    method_def!(
        "prod",
        math_prod,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!("radians", math_radians, METH_O),
    method_def!("remainder", math_remainder, _PyCFunctionFast, METH_FASTCALL),
    method_def!("sin", math_sin, METH_O),
    method_def!("sinh", math_sinh, METH_O),
    method_def!("sqrt", math_sqrt, METH_O),
    method_def!("sumprod", math_sumprod, _PyCFunctionFast, METH_FASTCALL),
    method_def!("tan", math_tan, METH_O),
    method_def!("tanh", math_tanh, METH_O),
    method_def!("trunc", math_trunc, METH_O),
    method_def!("ulp", math_ulp, METH_O),
    method_def!("perm", math_perm, _PyCFunctionFast, METH_FASTCALL),
    method_def!("comb", math_comb, _PyCFunctionFast, METH_FASTCALL),
]);

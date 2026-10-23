//! `cmath`: CPython 3.12's `cmathmodule.c` — every function over the
//! complex protocol (`PyComplex_AsCComplex`, so `__float__`/`__index__`
//! arguments included), C99 Annex G special-value tables for the
//! infinities, NaNs and signed zeros, and the same `errno` discipline
//! the clinic wrapper applies: `EDOM` is `ValueError: math domain
//! error` and `ERANGE` is `OverflowError: math range error`. `isclose`
//! carries CPython's keyword-only tolerances at their defaults for the
//! same reason as `math.isclose` (module `mod.rs`).

use coil_capi_sys::{
    _PyCFunctionFast, _PyCFunctionFastWithKeywords, METH_FASTCALL, METH_KEYWORDS, METH_O,
    PyCFunction, PyMethodDef, PyObject,
};

use crate::modules::{self, missing_argument_error, positional_error, positional_error_kw};
use crate::object::singletons;
use crate::tstate;
use crate::types::complex::{self, Py_complex};
use crate::types::exc as exceptions;
use crate::types::float_;
use crate::types::tuple;

const PI: f64 = core::f64::consts::PI;
const INF: f64 = f64::INFINITY;
const NAN: f64 = f64::NAN;
const P: f64 = PI;
const P14: f64 = 0.25 * PI;
const P12: f64 = 0.5 * PI;
const P34: f64 = 0.75 * PI;
/// FDLIBM-style placeholder for table cells no input reaches.
const U: f64 = -9.542631940771103e33;
const M_LN2: f64 = core::f64::consts::LN_2;
const M_LN10: f64 = core::f64::consts::LN_10;

const CM_LARGE_DOUBLE: f64 = f64::MAX / 4.0;
const CM_SQRT_LARGE_DOUBLE: f64 = 6.703903964971298e153;
const CM_LOG_LARGE_DOUBLE: f64 = 708.3964185322641;
const CM_SQRT_DBL_MIN: f64 = 1.4916681462400413e-154;

// The impl functions return the `errno` their C originals set: 0 for
// none, 1 for `EDOM`, 2 for `ERANGE`.
const EDOM: u8 = 1;
const ERANGE: u8 = 2;

fn domain_error() {
    tstate::set_string(exceptions::value_error(), b"math domain error");
}

fn range_error() {
    tstate::set_string(exceptions::overflow_error(), b"math range error");
}

// ---------------------------------------------- C99 Annex G value tables

/// `special_type`: the row/column index of a part's class (negative
/// infinity, negative finite, -0, +0, positive finite, positive
/// infinity, NaN).
fn special_type(d: f64) -> usize {
    let positive = 1.0f64.copysign(d) == 1.0;
    if d.is_finite() {
        if d != 0.0 {
            if positive { 4 } else { 1 }
        } else if positive {
            3
        } else {
            2
        }
    } else if d.is_nan() {
        6
    } else if positive {
        5
    } else {
        0
    }
}

macro_rules! table {
    [ $( [ $( ($re:expr, $im:expr) ),* ] ),* ] => {
        [ $( [ $( Py_complex { real: $re, imag: $im } ),* ] ),* ]
    };
}

static ACOS_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (P34, INF),
        (P, INF),
        (P, INF),
        (P, -INF),
        (P, -INF),
        (P34, -INF),
        (NAN, INF)
    ],
    [
        (P12, INF),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (P12, -INF),
        (NAN, NAN)
    ],
    [
        (P12, INF),
        (U, U),
        (P12, 0.0),
        (P12, -0.0),
        (U, U),
        (P12, -INF),
        (P12, NAN)
    ],
    [
        (P12, INF),
        (U, U),
        (P12, 0.0),
        (P12, -0.0),
        (U, U),
        (P12, -INF),
        (P12, NAN)
    ],
    [
        (P12, INF),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (P12, -INF),
        (NAN, NAN)
    ],
    [
        (P14, INF),
        (0.0, INF),
        (0.0, INF),
        (0.0, -INF),
        (0.0, -INF),
        (P14, -INF),
        (NAN, INF)
    ],
    [
        (NAN, INF),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, -INF),
        (NAN, NAN)
    ]
];

static ACOSH_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (INF, -P34),
        (INF, -P),
        (INF, -P),
        (INF, P),
        (INF, P),
        (INF, P34),
        (INF, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (0.0, -P12),
        (0.0, P12),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (0.0, -P12),
        (0.0, P12),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P14),
        (INF, -0.0),
        (INF, -0.0),
        (INF, 0.0),
        (INF, 0.0),
        (INF, P14),
        (INF, NAN)
    ],
    [
        (INF, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (INF, NAN),
        (NAN, NAN)
    ]
];

static ASINH_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (-INF, -P14),
        (-INF, -0.0),
        (-INF, -0.0),
        (-INF, 0.0),
        (-INF, 0.0),
        (-INF, P14),
        (-INF, NAN)
    ],
    [
        (-INF, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (-INF, P12),
        (NAN, NAN)
    ],
    [
        (-INF, -P12),
        (U, U),
        (-0.0, -0.0),
        (-0.0, 0.0),
        (U, U),
        (-INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P14),
        (INF, -0.0),
        (INF, -0.0),
        (INF, 0.0),
        (INF, 0.0),
        (INF, P14),
        (INF, NAN)
    ],
    [
        (INF, NAN),
        (NAN, NAN),
        (NAN, -0.0),
        (NAN, 0.0),
        (NAN, NAN),
        (INF, NAN),
        (NAN, NAN)
    ]
];

static ATANH_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (-0.0, -P12),
        (-0.0, -P12),
        (-0.0, -P12),
        (-0.0, P12),
        (-0.0, P12),
        (-0.0, P12),
        (-0.0, NAN)
    ],
    [
        (-0.0, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (-0.0, P12),
        (NAN, NAN)
    ],
    [
        (-0.0, -P12),
        (U, U),
        (-0.0, -0.0),
        (-0.0, 0.0),
        (U, U),
        (-0.0, P12),
        (-0.0, NAN)
    ],
    [
        (0.0, -P12),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (0.0, P12),
        (0.0, NAN)
    ],
    [
        (0.0, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (0.0, P12),
        (NAN, NAN)
    ],
    [
        (0.0, -P12),
        (0.0, -P12),
        (0.0, -P12),
        (0.0, P12),
        (0.0, P12),
        (0.0, P12),
        (0.0, NAN)
    ],
    [
        (0.0, -P12),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (0.0, P12),
        (NAN, NAN)
    ]
];

static COSH_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (INF, NAN),
        (U, U),
        (INF, 0.0),
        (INF, -0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, 0.0),
        (U, U),
        (1.0, 0.0),
        (1.0, -0.0),
        (U, U),
        (NAN, 0.0),
        (NAN, 0.0)
    ],
    [
        (NAN, 0.0),
        (U, U),
        (1.0, -0.0),
        (1.0, 0.0),
        (U, U),
        (NAN, 0.0),
        (NAN, 0.0)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (INF, NAN),
        (U, U),
        (INF, -0.0),
        (INF, 0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (NAN, NAN),
        (NAN, 0.0),
        (NAN, 0.0),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN)
    ]
];

static EXP_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (0.0, 0.0),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (0.0, 0.0),
        (0.0, 0.0)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (1.0, -0.0),
        (1.0, 0.0),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (1.0, -0.0),
        (1.0, 0.0),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (INF, NAN),
        (U, U),
        (INF, -0.0),
        (INF, 0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (NAN, NAN),
        (NAN, -0.0),
        (NAN, 0.0),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN)
    ]
];

static LOG_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (INF, -P34),
        (INF, -P),
        (INF, -P),
        (INF, P),
        (INF, P),
        (INF, P34),
        (INF, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (-INF, -P),
        (-INF, P),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (-INF, -0.0),
        (-INF, 0.0),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P12),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, P12),
        (NAN, NAN)
    ],
    [
        (INF, -P14),
        (INF, -0.0),
        (INF, -0.0),
        (INF, 0.0),
        (INF, 0.0),
        (INF, P14),
        (INF, NAN)
    ],
    [
        (INF, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (INF, NAN),
        (NAN, NAN)
    ]
];

static SINH_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (INF, NAN),
        (U, U),
        (-INF, -0.0),
        (-INF, 0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (0.0, NAN),
        (U, U),
        (-0.0, -0.0),
        (-0.0, 0.0),
        (U, U),
        (0.0, NAN),
        (0.0, NAN)
    ],
    [
        (0.0, NAN),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (0.0, NAN),
        (0.0, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (INF, NAN),
        (U, U),
        (INF, -0.0),
        (INF, 0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (NAN, NAN),
        (NAN, -0.0),
        (NAN, 0.0),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN)
    ]
];

static SQRT_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (INF, -INF),
        (0.0, -INF),
        (0.0, -INF),
        (0.0, INF),
        (0.0, INF),
        (INF, INF),
        (NAN, INF)
    ],
    [
        (INF, -INF),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, INF),
        (NAN, NAN)
    ],
    [
        (INF, -INF),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (INF, INF),
        (NAN, NAN)
    ],
    [
        (INF, -INF),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (INF, INF),
        (NAN, NAN)
    ],
    [
        (INF, -INF),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (INF, INF),
        (NAN, NAN)
    ],
    [
        (INF, -INF),
        (INF, -0.0),
        (INF, -0.0),
        (INF, 0.0),
        (INF, 0.0),
        (INF, INF),
        (INF, NAN)
    ],
    [
        (INF, -INF),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN),
        (INF, INF),
        (NAN, NAN)
    ]
];

static TANH_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (-1.0, 0.0),
        (U, U),
        (-1.0, -0.0),
        (-1.0, 0.0),
        (U, U),
        (-1.0, 0.0),
        (-1.0, 0.0)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (-0.0, -0.0),
        (-0.0, 0.0),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (1.0, 0.0),
        (U, U),
        (1.0, -0.0),
        (1.0, 0.0),
        (U, U),
        (1.0, 0.0),
        (1.0, 0.0)
    ],
    [
        (NAN, NAN),
        (NAN, NAN),
        (NAN, -0.0),
        (NAN, 0.0),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN)
    ]
];

static RECT_SPECIAL: [[Py_complex; 7]; 7] = table![
    [
        (INF, NAN),
        (U, U),
        (-INF, 0.0),
        (-INF, -0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (0.0, 0.0),
        (U, U),
        (-0.0, 0.0),
        (-0.0, -0.0),
        (U, U),
        (0.0, 0.0),
        (0.0, 0.0)
    ],
    [
        (0.0, 0.0),
        (U, U),
        (0.0, -0.0),
        (0.0, 0.0),
        (U, U),
        (0.0, 0.0),
        (0.0, 0.0)
    ],
    [
        (NAN, NAN),
        (U, U),
        (U, U),
        (U, U),
        (U, U),
        (NAN, NAN),
        (NAN, NAN)
    ],
    [
        (INF, NAN),
        (U, U),
        (INF, -0.0),
        (INF, 0.0),
        (U, U),
        (INF, NAN),
        (INF, NAN)
    ],
    [
        (NAN, NAN),
        (NAN, NAN),
        (NAN, 0.0),
        (NAN, 0.0),
        (NAN, NAN),
        (NAN, NAN),
        (NAN, NAN)
    ]
];

// --------------------------------------------------------- the c_* work

/// `cmath_sqrt_impl`.
fn c_sqrt(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        return (SQRT_SPECIAL[special_type(z.real)][special_type(z.imag)], 0);
    }
    if z.real == 0.0 && z.imag == 0.0 {
        return (
            Py_complex {
                real: 0.0,
                imag: z.imag,
            },
            0,
        );
    }
    let ax = z.real.abs();
    let ay = z.imag.abs();
    let s = if ax < f64::MIN_POSITIVE && ay < f64::MIN_POSITIVE {
        // Rescale so `hypot` keeps full precision: `ldexp` by 2**53
        // and back by 2**-27 turns both subnormals normal.
        const CM_SCALE_UP: f64 = 9007199254740992.0;
        const CM_SCALE_DOWN: f64 = 7.450580596923828e-9;
        let ax = ax * CM_SCALE_UP;
        (ax + ax.hypot(ay * CM_SCALE_UP)).sqrt() * CM_SCALE_DOWN
    } else {
        let ax8 = ax / 8.0;
        2.0 * (ax8 + ax8.hypot(ay / 8.0)).sqrt()
    };
    let d = ay / (2.0 * s);
    let r = if z.real >= 0.0 {
        Py_complex {
            real: s,
            imag: d.copysign(z.imag),
        }
    } else {
        Py_complex {
            real: d,
            imag: s.copysign(z.imag),
        }
    };
    (r, 0)
}

/// `cmath_acos_impl`.
fn c_acos(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        return (ACOS_SPECIAL[special_type(z.real)][special_type(z.imag)], 0);
    }
    let r = if z.real.abs() > CM_LARGE_DOUBLE || z.imag.abs() > CM_LARGE_DOUBLE {
        let real = z.imag.abs().atan2(z.real);
        let t = (z.real / 2.0).hypot(z.imag / 2.0).ln() + 2.0 * M_LN2;
        let imag = if z.real < 0.0 {
            -t.copysign(z.imag)
        } else {
            t.copysign(-z.imag)
        };
        Py_complex { real, imag }
    } else {
        let s1 = c_sqrt(Py_complex {
            real: 1.0 - z.real,
            imag: -z.imag,
        })
        .0;
        let s2 = c_sqrt(Py_complex {
            real: 1.0 + z.real,
            imag: z.imag,
        })
        .0;
        Py_complex {
            real: 2.0 * s1.real.atan2(s2.real),
            imag: (s2.real * s1.imag - s2.imag * s1.real).asinh(),
        }
    };
    (r, 0)
}

/// `cmath_acosh_impl`.
fn c_acosh(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        return (ACOSH_SPECIAL[special_type(z.real)][special_type(z.imag)], 0);
    }
    let r = if z.real.abs() > CM_LARGE_DOUBLE || z.imag.abs() > CM_LARGE_DOUBLE {
        Py_complex {
            real: (z.real / 2.0).hypot(z.imag / 2.0).ln() + 2.0 * M_LN2,
            imag: z.imag.atan2(z.real),
        }
    } else {
        let s1 = c_sqrt(Py_complex {
            real: z.real - 1.0,
            imag: z.imag,
        })
        .0;
        let s2 = c_sqrt(Py_complex {
            real: z.real + 1.0,
            imag: z.imag,
        })
        .0;
        Py_complex {
            real: (s1.real * s2.real + s1.imag * s2.imag).asinh(),
            imag: 2.0 * s1.imag.atan2(s2.real),
        }
    };
    (r, 0)
}

/// `cmath_asinh_impl`.
fn c_asinh(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        return (ASINH_SPECIAL[special_type(z.real)][special_type(z.imag)], 0);
    }
    let r = if z.real.abs() > CM_LARGE_DOUBLE || z.imag.abs() > CM_LARGE_DOUBLE {
        let t = (z.real / 2.0).hypot(z.imag / 2.0).ln() + 2.0 * M_LN2;
        let real = if z.imag >= 0.0 {
            t.copysign(z.real)
        } else {
            -t.copysign(-z.real)
        };
        Py_complex {
            real,
            imag: z.imag.atan2(z.real.abs()),
        }
    } else {
        let s1 = c_sqrt(Py_complex {
            real: 1.0 + z.imag,
            imag: -z.real,
        })
        .0;
        let s2 = c_sqrt(Py_complex {
            real: 1.0 - z.imag,
            imag: z.real,
        })
        .0;
        Py_complex {
            real: (s1.real * s2.imag - s2.real * s1.imag).asinh(),
            imag: z.imag.atan2(s1.real * s2.real - s1.imag * s2.imag),
        }
    };
    (r, 0)
}

/// `cmath_atanh_impl`.
fn c_atanh(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        return (ATANH_SPECIAL[special_type(z.real)][special_type(z.imag)], 0);
    }
    if z.real < 0.0 {
        let (r, errno) = c_atanh(Py_complex {
            real: -z.real,
            imag: -z.imag,
        });
        return (
            Py_complex {
                real: -r.real,
                imag: -r.imag,
            },
            errno,
        );
    }
    let ay = z.imag.abs();
    if z.real > CM_SQRT_LARGE_DOUBLE || ay > CM_SQRT_LARGE_DOUBLE {
        let h = (z.real / 2.0).hypot(z.imag / 2.0);
        return (
            Py_complex {
                real: z.real / 4.0 / h / h,
                imag: -(PI / 2.0).copysign(-z.imag),
            },
            0,
        );
    }
    if z.real == 1.0 && ay < CM_SQRT_DBL_MIN {
        if ay == 0.0 {
            return (
                Py_complex {
                    real: INF,
                    imag: z.imag,
                },
                EDOM,
            );
        }
        return (
            Py_complex {
                real: -(ay.sqrt() / ay.hypot(2.0).sqrt()).ln(),
                imag: (2.0f64.atan2(-ay) / 2.0).copysign(z.imag),
            },
            0,
        );
    }
    let r = Py_complex {
        real: m_log1p(4.0 * z.real / ((1.0 - z.real) * (1.0 - z.real) + ay * ay)) / 4.0,
        imag: -(-2.0 * z.imag).atan2((1.0 - z.real) * (1.0 + z.real) - ay * ay) / 2.0,
    };
    (r, 0)
}

/// `_Py_log1p`: the platform `log1p` with signed zeros fixed at the
/// source.
fn m_log1p(x: f64) -> f64 {
    if x == 0.0 { x } else { x.ln_1p() }
}

/// `cmath_asin_impl`: `-i asinh(iz)`.
fn c_asin(z: Py_complex) -> (Py_complex, u8) {
    let (s, errno) = c_asinh(Py_complex {
        real: -z.imag,
        imag: z.real,
    });
    (
        Py_complex {
            real: s.imag,
            imag: -s.real,
        },
        errno,
    )
}

/// `cmath_atan_impl`: `-i atanh(iz)`.
fn c_atan(z: Py_complex) -> (Py_complex, u8) {
    let (s, errno) = c_atanh(Py_complex {
        real: -z.imag,
        imag: z.real,
    });
    (
        Py_complex {
            real: s.imag,
            imag: -s.real,
        },
        errno,
    )
}

/// `cmath_cos_impl`: `cosh(iz)`.
fn c_cos(z: Py_complex) -> (Py_complex, u8) {
    c_cosh(Py_complex {
        real: -z.imag,
        imag: z.real,
    })
}

/// `cmath_cosh_impl`.
fn c_cosh(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        let r = if z.real.is_infinite() && z.imag.is_finite() && z.imag != 0.0 {
            if z.real > 0.0 {
                Py_complex {
                    real: INF.copysign(z.imag.cos()),
                    imag: INF.copysign(z.imag.sin()),
                }
            } else {
                Py_complex {
                    real: INF.copysign(z.imag.cos()),
                    imag: -INF.copysign(z.imag.sin()),
                }
            }
        } else {
            COSH_SPECIAL[special_type(z.real)][special_type(z.imag)]
        };
        let errno = if z.imag.is_infinite() && !z.real.is_nan() {
            EDOM
        } else {
            0
        };
        return (r, errno);
    }
    let r = if z.real.abs() > CM_LOG_LARGE_DOUBLE {
        let x_minus_one = z.real - 1.0f64.copysign(z.real);
        Py_complex {
            real: z.imag.cos() * x_minus_one.cosh() * core::f64::consts::E,
            imag: z.imag.sin() * x_minus_one.sinh() * core::f64::consts::E,
        }
    } else {
        Py_complex {
            real: z.imag.cos() * z.real.cosh(),
            imag: z.imag.sin() * z.real.sinh(),
        }
    };
    let errno = if r.real.is_infinite() || r.imag.is_infinite() {
        ERANGE
    } else {
        0
    };
    (r, errno)
}

/// `cmath_exp_impl`.
fn c_exp(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        let r = if z.real.is_infinite() && z.imag.is_finite() && z.imag != 0.0 {
            if z.real > 0.0 {
                Py_complex {
                    real: INF.copysign(z.imag.cos()),
                    imag: INF.copysign(z.imag.sin()),
                }
            } else {
                Py_complex {
                    real: 0.0f64.copysign(z.imag.cos()),
                    imag: 0.0f64.copysign(z.imag.sin()),
                }
            }
        } else {
            EXP_SPECIAL[special_type(z.real)][special_type(z.imag)]
        };
        let errno = if z.imag.is_infinite()
            && (z.real.is_finite() || (z.real.is_infinite() && z.real > 0.0))
        {
            EDOM
        } else {
            0
        };
        return (r, errno);
    }
    let r = if z.real > CM_LOG_LARGE_DOUBLE {
        let l = (z.real - 1.0).exp();
        Py_complex {
            real: l * z.imag.cos() * core::f64::consts::E,
            imag: l * z.imag.sin() * core::f64::consts::E,
        }
    } else {
        let l = z.real.exp();
        Py_complex {
            real: l * z.imag.cos(),
            imag: l * z.imag.sin(),
        }
    };
    let errno = if r.real.is_infinite() || r.imag.is_infinite() {
        ERANGE
    } else {
        0
    };
    (r, errno)
}

/// `c_log`, the workhorse of `cmath.log` and `cmath.log10`.
fn c_log(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        return (LOG_SPECIAL[special_type(z.real)][special_type(z.imag)], 0);
    }
    let ax = z.real.abs();
    let ay = z.imag.abs();
    let r;
    if ax > CM_LARGE_DOUBLE || ay > CM_LARGE_DOUBLE {
        r = Py_complex {
            real: (ax / 2.0).hypot(ay / 2.0).ln() + M_LN2,
            imag: z.imag.atan2(z.real),
        };
        return (r, 0);
    }
    if ax < f64::MIN_POSITIVE && ay < f64::MIN_POSITIVE {
        if ax > 0.0 || ay > 0.0 {
            const SCALE: f64 = 9007199254740992.0; // 2**53
            r = Py_complex {
                real: (ax * SCALE).hypot(ay * SCALE).ln() - 53.0 * M_LN2,
                imag: z.imag.atan2(z.real),
            };
        } else {
            return (
                Py_complex {
                    real: -INF,
                    imag: z.imag.atan2(z.real),
                },
                EDOM,
            );
        }
    } else {
        let h = ax.hypot(ay);
        let real = if (0.71..=1.73).contains(&h) {
            let (am, an) = if ax > ay { (ax, ay) } else { (ay, ax) };
            m_log1p((am - 1.0) * (am + 1.0) + an * an) / 2.0
        } else {
            h.ln()
        };
        r = Py_complex {
            real,
            imag: z.imag.atan2(z.real),
        };
    }
    (r, 0)
}

/// `cmath_log10_impl`.
fn c_log10(z: Py_complex) -> (Py_complex, u8) {
    let (r, errno) = c_log(z);
    (
        Py_complex {
            real: r.real / M_LN10,
            imag: r.imag / M_LN10,
        },
        errno,
    )
}

/// `cmath_sin_impl`: `-i sinh(iz)`.
fn c_sin(z: Py_complex) -> (Py_complex, u8) {
    let (s, errno) = c_sinh(Py_complex {
        real: -z.imag,
        imag: z.real,
    });
    (
        Py_complex {
            real: s.imag,
            imag: -s.real,
        },
        errno,
    )
}

/// `cmath_sinh_impl`.
fn c_sinh(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        let r = if z.real.is_infinite() && z.imag.is_finite() && z.imag != 0.0 {
            if z.real > 0.0 {
                Py_complex {
                    real: INF.copysign(z.imag.cos()),
                    imag: INF.copysign(z.imag.sin()),
                }
            } else {
                Py_complex {
                    real: -INF.copysign(z.imag.cos()),
                    imag: INF.copysign(z.imag.sin()),
                }
            }
        } else {
            SINH_SPECIAL[special_type(z.real)][special_type(z.imag)]
        };
        let errno = if z.imag.is_infinite() && !z.real.is_nan() {
            EDOM
        } else {
            0
        };
        return (r, errno);
    }
    let r = if z.real.abs() > CM_LOG_LARGE_DOUBLE {
        let x_minus_one = z.real - 1.0f64.copysign(z.real);
        Py_complex {
            real: z.imag.cos() * x_minus_one.sinh() * core::f64::consts::E,
            imag: z.imag.sin() * x_minus_one.cosh() * core::f64::consts::E,
        }
    } else {
        Py_complex {
            real: z.imag.cos() * z.real.sinh(),
            imag: z.imag.sin() * z.real.cosh(),
        }
    };
    let errno = if r.real.is_infinite() || r.imag.is_infinite() {
        ERANGE
    } else {
        0
    };
    (r, errno)
}

/// `cmath_tan_impl`: `-i tanh(iz)`.
fn c_tan(z: Py_complex) -> (Py_complex, u8) {
    let (s, errno) = c_tanh(Py_complex {
        real: -z.imag,
        imag: z.real,
    });
    (
        Py_complex {
            real: s.imag,
            imag: -s.real,
        },
        errno,
    )
}

/// `cmath_tanh_impl`.
fn c_tanh(z: Py_complex) -> (Py_complex, u8) {
    if !z.real.is_finite() || !z.imag.is_finite() {
        let r = if z.real.is_infinite() && z.imag.is_finite() && z.imag != 0.0 {
            let imag = 0.0f64.copysign(2.0 * z.imag.sin() * z.imag.cos());
            if z.real > 0.0 {
                Py_complex { real: 1.0, imag }
            } else {
                Py_complex { real: -1.0, imag }
            }
        } else {
            TANH_SPECIAL[special_type(z.real)][special_type(z.imag)]
        };
        let errno = if z.imag.is_infinite() && z.real.is_finite() {
            EDOM
        } else {
            0
        };
        return (r, errno);
    }
    let r = if z.real.abs() > CM_LOG_LARGE_DOUBLE {
        Py_complex {
            real: 1.0f64.copysign(z.real),
            imag: 4.0 * z.imag.sin() * z.imag.cos() * (-2.0 * z.real.abs()).exp(),
        }
    } else {
        let tx = z.real.tanh();
        let ty = z.imag.tan();
        let cx = 1.0 / z.real.cosh();
        let txty = tx * ty;
        let denom = 1.0 + txty * txty;
        Py_complex {
            real: tx * (1.0 + ty * ty) / denom,
            imag: (ty / denom) * cx * cx,
        }
    };
    (r, 0)
}

/// `c_atan2`: the system `atan2` with the inf/NaN corners C99 leaves
/// to the platform, fixed.
fn c_atan2(z: Py_complex) -> f64 {
    if z.real.is_nan() || z.imag.is_nan() {
        return NAN;
    }
    if z.imag.is_infinite() {
        if z.real.is_infinite() {
            if 1.0f64.copysign(z.real) == 1.0 {
                return P14.copysign(z.imag);
            }
            return P34.copysign(z.imag);
        }
        return P12.copysign(z.imag);
    }
    if z.real.is_infinite() || z.imag == 0.0 {
        if 1.0f64.copysign(z.real) == 1.0 {
            return 0.0f64.copysign(z.imag);
        }
        return PI.copysign(z.imag);
    }
    z.imag.atan2(z.real)
}

// ------------------------------------------------------------ wrappers

macro_rules! cmath1 {
    ($name:ident, $func:ident) => {
        unsafe extern "C" fn $name(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
            // SAFETY: the argument is a valid object and the results
            // come from this module's constructors.
            unsafe {
                let z = complex::PyComplex_AsCComplex(arg);
                if !tstate::occurred().is_null() {
                    return core::ptr::null_mut();
                }
                let (r, errno) = $func(z);
                if errno == EDOM {
                    domain_error();
                    return core::ptr::null_mut();
                } else if errno == ERANGE {
                    range_error();
                    return core::ptr::null_mut();
                }
                complex::PyComplex_FromCComplex(r)
            }
        }
    };
}

cmath1!(cmath_acos, c_acos);
cmath1!(cmath_acosh, c_acosh);
cmath1!(cmath_asin, c_asin);
cmath1!(cmath_asinh, c_asinh);
cmath1!(cmath_atan, c_atan);
cmath1!(cmath_atanh, c_atanh);
cmath1!(cmath_cos, c_cos);
cmath1!(cmath_cosh, c_cosh);
cmath1!(cmath_exp, c_exp);
cmath1!(cmath_log10, c_log10);
cmath1!(cmath_sin, c_sin);
cmath1!(cmath_sinh, c_sinh);
cmath1!(cmath_sqrt, c_sqrt);
cmath1!(cmath_tan, c_tan);
cmath1!(cmath_tanh, c_tanh);

macro_rules! cmath_predicate {
    ($name:ident, $test:expr) => {
        unsafe extern "C" fn $name(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
            // SAFETY: the argument is a valid object.
            unsafe {
                let z = complex::PyComplex_AsCComplex(arg);
                if !tstate::occurred().is_null() {
                    return core::ptr::null_mut();
                }
                if $test(&z) {
                    singletons::true_()
                } else {
                    singletons::false_()
                }
            }
        }
    };
}

cmath_predicate!(cmath_isfinite, |z: &Py_complex| {
    z.real.is_finite() && z.imag.is_finite()
});
cmath_predicate!(cmath_isinf, |z: &Py_complex| z.real.is_infinite()
    || z.imag.is_infinite());
cmath_predicate!(cmath_isnan, |z: &Py_complex| z.real.is_nan()
    || z.imag.is_nan());

/// `cmath.phase`.
unsafe extern "C" fn cmath_phase(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object.
    unsafe {
        let z = complex::PyComplex_AsCComplex(arg);
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        float_::PyFloat_FromDouble(c_atan2(z))
    }
}

/// `cmath.polar`.
unsafe extern "C" fn cmath_polar(_self: *mut PyObject, arg: *mut PyObject) -> *mut PyObject {
    // SAFETY: the argument is a valid object and the tuple is fresh.
    unsafe {
        let z = complex::PyComplex_AsCComplex(arg);
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let phi = c_atan2(z);
        let (r, errno) = complex::c_abs(z);
        if errno != 0 {
            range_error();
            return core::ptr::null_mut();
        }
        let result = tuple::PyTuple_New(2);
        if result.is_null() {
            return core::ptr::null_mut();
        }
        tuple::PyTuple_SetItem(result, 0, float_::PyFloat_FromDouble(r));
        tuple::PyTuple_SetItem(result, 1, float_::PyFloat_FromDouble(phi));
        result
    }
}

/// `cmath.rect`.
unsafe extern "C" fn cmath_rect(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if nargs != 2 {
        positional_error("rect", 2, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`,
    // and the results come from this module's constructors.
    unsafe {
        let args = core::slice::from_raw_parts(args, nargs as usize);
        let r = float_::PyFloat_AsDouble(args[0]);
        if r == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let phi = float_::PyFloat_AsDouble(args[1]);
        if phi == -1.0 && !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let (z, errno) = rect_impl(r, phi);
        if errno == EDOM {
            domain_error();
            return core::ptr::null_mut();
        } else if errno == ERANGE {
            range_error();
            return core::ptr::null_mut();
        }
        complex::PyComplex_FromCComplex(z)
    }
}

/// `cmath_rect_impl`.
fn rect_impl(r: f64, phi: f64) -> (Py_complex, u8) {
    let z;
    let errno;
    if !r.is_finite() || !phi.is_finite() {
        if r.is_infinite() && phi.is_finite() && phi != 0.0 {
            if r > 0.0 {
                z = Py_complex {
                    real: INF.copysign(phi.cos()),
                    imag: INF.copysign(phi.sin()),
                };
            } else {
                z = Py_complex {
                    real: -INF.copysign(phi.cos()),
                    imag: -INF.copysign(phi.sin()),
                };
            }
        } else {
            z = RECT_SPECIAL[special_type(r)][special_type(phi)];
        }
        errno = if r != 0.0 && !r.is_nan() && phi.is_infinite() {
            EDOM
        } else {
            0
        };
    } else if phi == 0.0 {
        // A signed-zero workaround CPython keeps for buggy libm.
        z = Py_complex {
            real: r,
            imag: r * phi,
        };
        errno = 0;
    } else {
        z = Py_complex {
            real: r * phi.cos(),
            imag: r * phi.sin(),
        };
        errno = 0;
    }
    (z, errno)
}

/// `cmath.log`.
unsafe extern "C" fn cmath_log(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
) -> *mut PyObject {
    if !(1..=2).contains(&nargs) {
        positional_error("log", 1, 2, nargs as usize);
        return core::ptr::null_mut();
    }
    // SAFETY: the calling machinery passes `nargs` objects at `args`,
    // and the results come from this module's constructors.
    unsafe {
        let args = core::slice::from_raw_parts(args, nargs as usize);
        let mut x = complex::PyComplex_AsCComplex(args[0]);
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let (log_x, mut errno) = c_log(x);
        x = log_x;
        if nargs == 2 {
            let y = complex::PyComplex_AsCComplex(args[1]);
            if !tstate::occurred().is_null() {
                return core::ptr::null_mut();
            }
            // `c_log` always resets the flag; `_Py_c_quot` only sets it.
            let (y, errno_y) = c_log(y);
            errno = errno_y;
            let (q, errno_q) = complex::c_quot(x, y);
            if errno_q == EDOM {
                errno = EDOM;
            }
            x = q;
        }
        if errno == EDOM {
            domain_error();
            return core::ptr::null_mut();
        } else if errno == ERANGE {
            range_error();
            return core::ptr::null_mut();
        }
        complex::PyComplex_FromCComplex(x)
    }
}

/// `cmath.isclose`.
unsafe extern "C" fn cmath_isclose(
    _self: *mut PyObject,
    args: *const *mut PyObject,
    nargs: isize,
    kwds: *mut PyObject,
) -> *mut PyObject {
    if nargs > 2 {
        positional_error_kw("isclose", 2, 2, nargs as usize);
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
    unsafe {
        let args = core::slice::from_raw_parts(args, nargs as usize);
        let a = complex::PyComplex_AsCComplex(args[0]);
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let b = complex::PyComplex_AsCComplex(args[1]);
        if !tstate::occurred().is_null() {
            return core::ptr::null_mut();
        }
        let rel_tol = 1e-09;
        let abs_tol = 0.0;
        if rel_tol < 0.0 || abs_tol < 0.0 {
            tstate::set_string(
                exceptions::value_error(),
                b"tolerances must be non-negative",
            );
            return core::ptr::null_mut();
        }
        let close = isclose_impl(a, b, rel_tol, abs_tol);
        if close {
            singletons::true_()
        } else {
            singletons::false_()
        }
    }
}

/// `cmath_isclose_impl` past its tolerance check (the keyword seam
/// pins the tolerances at CPython's defaults, so the wrapper checks
/// the values it passes).
fn isclose_impl(a: Py_complex, b: Py_complex, rel_tol: f64, abs_tol: f64) -> bool {
    if a.real == b.real && a.imag == b.imag {
        return true;
    }
    if a.real.is_infinite() || a.imag.is_infinite() || b.real.is_infinite() || b.imag.is_infinite()
    {
        return false;
    }
    let d = Py_complex {
        real: a.real - b.real,
        imag: a.imag - b.imag,
    };
    let diff = complex::_Py_c_abs(d);
    diff <= rel_tol * complex::_Py_c_abs(b)
        || diff <= rel_tol * complex::_Py_c_abs(a)
        || diff <= abs_tol
}

macro_rules! method_def {
    ($name:literal, $func:ident, $flags:expr) => {{
        let func: PyCFunction = $func;
        PyMethodDef {
            ml_name: concat!($name, "\0").as_ptr() as *const core::ffi::c_char,
            ml_meth: Some(func),
            ml_flags: $flags,
            ml_doc: core::ptr::null(),
        }
    }};
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

/// The `cmath` functions as CPython orders them.
pub static CMATH_METHODS: modules::MethodTable = modules::MethodTable(&[
    method_def!("acos", cmath_acos, METH_O),
    method_def!("acosh", cmath_acosh, METH_O),
    method_def!("asin", cmath_asin, METH_O),
    method_def!("asinh", cmath_asinh, METH_O),
    method_def!("atan", cmath_atan, METH_O),
    method_def!("atanh", cmath_atanh, METH_O),
    method_def!("cos", cmath_cos, METH_O),
    method_def!("cosh", cmath_cosh, METH_O),
    method_def!("exp", cmath_exp, METH_O),
    method_def!(
        "isclose",
        cmath_isclose,
        _PyCFunctionFastWithKeywords,
        METH_FASTCALL | METH_KEYWORDS
    ),
    method_def!("isfinite", cmath_isfinite, METH_O),
    method_def!("isinf", cmath_isinf, METH_O),
    method_def!("isnan", cmath_isnan, METH_O),
    method_def!("log", cmath_log, _PyCFunctionFast, METH_FASTCALL),
    method_def!("log10", cmath_log10, METH_O),
    method_def!("phase", cmath_phase, METH_O),
    method_def!("polar", cmath_polar, METH_O),
    method_def!("rect", cmath_rect, _PyCFunctionFast, METH_FASTCALL),
    method_def!("sin", cmath_sin, METH_O),
    method_def!("sinh", cmath_sinh, METH_O),
    method_def!("sqrt", cmath_sqrt, METH_O),
    method_def!("tan", cmath_tan, METH_O),
    method_def!("tanh", cmath_tanh, METH_O),
]);

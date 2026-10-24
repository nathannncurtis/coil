//! Oracle-driven tests for the `cmath` module (spec 10.6), over the
//! C99 Annex G lattice of `cmath_module.tsv` (CPython 3.12 via
//! `tools/gen_module_oracle.py`). Functions whose results come from the
//! platform libm components get the documented `common::close` latitude
//! per part (four ulp of the larger magnitude or an absolute 1e-13,
//! whichever is looser; a signed zero may stand in for the same-sign
//! subnormal another platform's atan2 keeps). The predicates, the
//! errors, and every signed zero and NaN component match exactly.

mod common;

use coil_capi_sys::PyObject;
use common::{call, encode_ptr, init, rows, token, ulp_agrees};
use python312::modules::cmath;
use python312::tstate;

/// The functions whose parts the platform libm computes.
const ULP_FUNCTIONS: &[&str] = &[
    "acos", "acosh", "asin", "asinh", "atan", "atanh", "cos", "cosh", "exp", "log", "log10",
    "phase", "polar", "rect", "sin", "sinh", "sqrt", "tan", "tanh",
];

#[test]
fn cmath_module_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("cmath_module.tsv") {
        tstate::clear();
        let name = row[0].clone();
        let args: Vec<*mut PyObject> = row[1..row.len() - 1].iter().map(|t| token(t)).collect();
        let result = call(&cmath::CMATH_METHODS, &name, &args);
        let got = encode_ptr(result);
        let want = row.last().unwrap();
        let agrees =
            got == *want || (ULP_FUNCTIONS.contains(&name.as_str()) && ulp_agrees(&got, want));
        if !agrees {
            mismatches.push(format!(
                "line {lineno}: {}({}) = {got}, want {want}",
                name,
                row[1..row.len() - 1].join(", ")
            ));
        }
        if mismatches.len() == 20 {
            mismatches.push("(stopping after 20)".to_string());
            break;
        }
    }
    assert!(mismatches.is_empty(), "\n{}", mismatches.join("\n"));
}

/// The ulp latitude never relaxes the zero signs or NaNs: IEEE
/// equality compares +0.0 and -0.0 equal, so the allowance must be
/// checked against the bit patterns, and a zero may stand in only for
/// a same-sign subnormal (the MSVC atan2 underflow).
#[test]
fn ulp_latitude_keeps_zeros_and_nans_exact() {
    assert!(common::close(0.0, 0.0));
    assert!(common::close(-0.0, -0.0));
    assert!(!common::close(0.0, -0.0));
    assert!(!common::close(-0.0, 0.0));
    assert!(common::close(0.0, f64::from_bits(1)));
    assert!(common::close(-0.0, -f64::from_bits(1)));
    assert!(!common::close(f64::NAN, f64::NAN));
    assert!(!common::close(1.0, -1.0));
}

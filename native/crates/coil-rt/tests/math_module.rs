//! Oracle-driven tests for the `math` module (spec 10.6). Every row in
//! `math_module.tsv` was computed by CPython 3.12 through
//! `tools/gen_module_oracle.py`; the encodings of the argument and result
//! columns are shared with that script. Functions whose C implementation
//! is the platform libm carry a one-ulp allowance (see `common::close`);
//! every other row must match bit for bit, error messages included.

mod common;

use coil_capi_sys::PyObject;
use common::{call, encode_ptr, init, rows, token, ulp_agrees};
use python312::modules::math;
use python312::tstate;

/// The functions the platform libm computes (`m_atan2` handles the
/// infinities and zeros itself and calls atan2 for the rest); one ulp
/// per component.
const ULP_FUNCTIONS: &[&str] = &[
    "acos", "acosh", "asin", "asinh", "atan", "atan2", "atanh", "cbrt", "cos", "cosh", "erf",
    "erfc", "exp", "exp2", "expm1", "gamma", "lgamma", "log", "log1p", "log2", "log10", "pow",
    "sin", "sinh", "tan", "tanh",
];

#[test]
fn math_module_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("math_module.tsv") {
        tstate::clear();
        let name = row[0].clone();
        let args: Vec<*mut PyObject> = row[1..row.len() - 1].iter().map(|t| token(t)).collect();
        let result = call(&math::MATH_METHODS, &name, &args);
        let got = encode_ptr(result);
        let want = row.last().unwrap();
        let ok = got == *want || (ULP_FUNCTIONS.contains(&name.as_str()) && ulp_agrees(&got, want));
        if !ok {
            mismatches.push(format!(
                "line {lineno}: {} -> got {got:?}, want {want:?}",
                row.join("\t")
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatched rows, first ones:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

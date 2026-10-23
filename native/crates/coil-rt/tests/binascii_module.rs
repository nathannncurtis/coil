//! Oracle-driven tests for the `binascii` module (spec 10.6). Every row
//! in `binascii_module.tsv` was computed by CPython 3.12 through
//! `tools/gen_module_oracle.py`; the codecs and checksums are exact, so
//! every result and error message matches byte for byte. The
//! keyword-only flags `backtick`, `strict_mode` and `newline` cannot be
//! passed without module objects and run at their defaults in both
//! builds, so their rows are generated at those defaults.

mod common;

use coil_capi_sys::PyObject;
use common::{call, encode_ptr, init, rows, token};
use python312::modules::binascii;
use python312::tstate;

#[test]
fn binascii_module_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("binascii_module.tsv") {
        tstate::clear();
        let name = row[0].clone();
        let args: Vec<*mut PyObject> = row[1..row.len() - 1].iter().map(|t| token(t)).collect();
        let result = call(&binascii::BINASCII_METHODS, &name, &args);
        let got = encode_ptr(result);
        let want = row.last().unwrap();
        if got != *want {
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

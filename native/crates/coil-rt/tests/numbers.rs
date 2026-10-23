//! Oracle-driven tests for the number types (spec section 8.5). Every row
//! in `tests/data` was computed by CPython 3.12 through
//! `tools/gen_number_oracle.py`; the encoding of the last column is shared
//! with that script. `pow` rows carry a one-ulp allowance, see
//! `pow_agrees`; every other row must match bit for bit.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::Once;

use core::ffi::c_int;

use coil_capi_sys::PyObject;
use python312::object::{exceptions, header, singletons, typeobject};
use python312::tstate;
use python312::types::{bool_, complex, float_, long};

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

/// The data file's rows with their 1-based line numbers, `\t`/`\n`
/// escapes in the inputs undone.
fn rows(name: &str) -> Vec<(usize, Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(i, line)| {
            (
                i + 1,
                line.split('\t')
                    .map(|field| field.replace("\\t", "\t").replace("\\n", "\n"))
                    .collect(),
            )
        })
        .collect()
}

/// Undo nothing here: floats arrive as their 16-digit hex bit pattern.
fn from_bits(hex: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(hex, 16).unwrap())
}

/// A float in the shared encoding; a NaN is the bare word `nan`, since its
/// payload and sign bits are unspecified and vary between builds.
fn bits_hex(value: f64) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else {
        format!("{:016X}", value.to_bits())
    }
}

/// Build an int operand; the generator writes huge operands in hex.
fn parse_int(s: &str) -> *mut PyObject {
    let (digits, base) = match s.strip_prefix("0x") {
        Some(rest) => (rest, 16),
        None => (s, 10),
    };
    let c = CString::new(digits).unwrap();
    // SAFETY: `c` is a null-terminated string the call only reads.
    unsafe { long::PyLong_FromString(c.as_ptr(), std::ptr::null_mut(), base) }
}

/// The set error as the oracle's `e:Type:message` encoding.
fn error_text() -> String {
    let exc = tstate::occurred();
    assert!(!exc.is_null(), "expected an error to be set");
    let name = if core::ptr::eq(exc, exceptions::type_error()) {
        "TypeError"
    } else if core::ptr::eq(exc, exceptions::value_error()) {
        "ValueError"
    } else if core::ptr::eq(exc, exceptions::overflow_error()) {
        "OverflowError"
    } else if core::ptr::eq(exc, exceptions::zero_division_error()) {
        "ZeroDivisionError"
    } else if core::ptr::eq(exc, exceptions::memory_error()) {
        "MemoryError"
    } else if core::ptr::eq(exc, exceptions::system_error()) {
        "SystemError"
    } else if core::ptr::eq(exc, exceptions::not_implemented_error()) {
        "NotImplementedError"
    } else {
        "BaseException"
    };
    let message = String::from_utf8_lossy(&tstate::message()).into_owned();
    tstate::clear();
    format!("e:{name}:{message}")
}

/// A `String`-returning entry point's result in the shared encoding.
fn encode_opt(result: Option<String>) -> String {
    match result {
        Some(text) => text,
        None => error_text(),
    }
}

/// A result object in the shared encoding.
fn encode_ptr(result: *mut PyObject) -> String {
    if result.is_null() {
        return error_text();
    }
    // SAFETY: the object came from this runtime's constructors, so its
    // header and payload read true.
    unsafe {
        let ty = header::type_of(result);
        if core::ptr::eq(ty, &raw mut bool_::PyBool_Type) {
            let value = usize::from(core::ptr::eq(result, singletons::true_()));
            return format!("b:{value}");
        }
        if core::ptr::eq(ty, &raw mut long::PyLong_Type) {
            // The to-string digit limit can reject the encoding itself.
            return match long::format(result.cast(), 10, false) {
                Some(text) => format!("i:{text}"),
                None => error_text(),
            };
        }
        if core::ptr::eq(ty, &raw mut float_::PyFloat_Type) {
            return format!("f:{}", bits_hex(float_::PyFloat_AsDouble(result)));
        }
        let real = complex::PyComplex_RealAsDouble(result);
        let imag = complex::PyComplex_ImagAsDouble(result);
        format!("c:{},{}", bits_hex(real), bits_hex(imag))
    }
}

/// Whether two non-NaN floats are bit-identical or adjacent values of one
/// sign; the bit patterns of same-sign finite floats are ordered, so a
/// bit distance of one is one ulp.
fn close(got: f64, want: f64) -> bool {
    got == want
        || (got.is_finite()
            && want.is_finite()
            && got != 0.0
            && want != 0.0
            && got.signum() == want.signum()
            && got.to_bits().abs_diff(want.to_bits()) <= 1)
}

/// One float field of the `f:`/`c:` encodings; a NaN is the bare word
/// `nan` because its payload bits are unspecified.
fn float_field(s: &str) -> Option<f64> {
    if s == "nan" { None } else { Some(from_bits(s)) }
}

fn parts_close(got: Option<f64>, want: Option<f64>) -> bool {
    match (got, want) {
        (Some(got), Some(want)) => close(got, want),
        (None, None) => true,
        _ => false,
    }
}

/// Whether a float or complex `pow` row agrees. CPython computes these
/// with the platform libm (`pow`, and `hypot`, `atan2`, `exp`, `log`,
/// `cos`, `sin` for complex exponents), which disagree by one ulp on
/// half-way inputs between builds -- CPython's own results for them are
/// not bit-reproducible across platforms. Such rows allow one ulp per
/// component; errors, zeros, infinities and NaNs must still match.
fn pow_agrees(got: &str, want: &str) -> bool {
    if let (Some(g), Some(w)) = (got.strip_prefix("f:"), want.strip_prefix("f:")) {
        return parts_close(float_field(g), float_field(w));
    }
    if let (Some(g), Some(w)) = (got.strip_prefix("c:"), want.strip_prefix("c:"))
        && let (Some((gr, gi)), Some((wr, wi))) = (g.split_once(','), w.split_once(','))
    {
        return parts_close(float_field(gr), float_field(wr))
            && parts_close(float_field(gi), float_field(wi));
    }
    got == want
}

fn check(mismatches: &mut Vec<String>, table: &str, lineno: usize, row: &[String], got: String) {
    let want = row.last().unwrap();
    let ok = got == *want
        || (row[0] == "pow"
            && matches!(table, "float_arithmetic" | "complex_arithmetic")
            && pow_agrees(&got, want));
    if !ok {
        mismatches.push(format!(
            "{table} line {lineno}: {} -> got {got:?}, want {want:?}",
            row.join("\t")
        ));
    }
}

fn finish(table: &str, mismatches: Vec<String>) {
    assert!(
        mismatches.is_empty(),
        "{table}: {} mismatched rows, first ones:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Run a predicate over every row of a table and collect mismatches.
fn run_table(table: &str, case: &dyn Fn(usize, &[String]) -> Option<String>) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (lineno, row) in rows(table) {
        if let Some(got) = case(lineno, &row) {
            check(&mut mismatches, table, lineno, &row, got);
        }
    }
    mismatches
}

#[test]
fn int_arithmetic_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("int_arithmetic.tsv") {
        tstate::clear();
        let a = parse_int(&row[1]);
        assert!(!a.is_null(), "line {lineno}: operand {}", row[1]);
        let b = if row[2].is_empty() {
            core::ptr::null_mut()
        } else {
            parse_int(&row[2])
        };
        // SAFETY: the slots take any objects; the operands are ints.
        let result = unsafe {
            let table = long::PyLong_Type.tp_as_number;
            match row[0].as_str() {
                "+" => (*table).nb_add.unwrap()(a, b),
                "-" => (*table).nb_subtract.unwrap()(a, b),
                "*" => (*table).nb_multiply.unwrap()(a, b),
                "&" => (*table).nb_and.unwrap()(a, b),
                "|" => (*table).nb_or.unwrap()(a, b),
                "^" => (*table).nb_xor.unwrap()(a, b),
                "//" => (*table).nb_floor_divide.unwrap()(a, b),
                "%" => (*table).nb_remainder.unwrap()(a, b),
                "<<" => (*table).nb_lshift.unwrap()(a, b),
                ">>" => (*table).nb_rshift.unwrap()(a, b),
                "~" => (*table).nb_invert.unwrap()(a),
                "neg" => (*table).nb_negative.unwrap()(a),
                "abs" => (*table).nb_absolute.unwrap()(a),
                "pow" => (*table).nb_power.unwrap()(a, b, singletons::none()),
                "pow3" => {
                    let m = parse_int(&row[3]);
                    (*table).nb_power.unwrap()(a, b, m)
                }
                cmp @ ("lt" | "le" | "eq" | "ne" | "gt" | "ge") => {
                    let op = match cmp {
                        "lt" => typeobject::Py_LT,
                        "le" => typeobject::Py_LE,
                        "eq" => typeobject::Py_EQ,
                        "ne" => typeobject::Py_NE,
                        "gt" => typeobject::Py_GT,
                        _ => typeobject::Py_GE,
                    };
                    let compare = long::PyLong_Type.tp_richcompare.unwrap();
                    compare(a, b, op)
                }
                other => panic!("unknown op {other}"),
            }
        };
        check(
            &mut mismatches,
            "int_arithmetic",
            lineno,
            &row,
            encode_ptr(result),
        );
    }
    finish("int_arithmetic", mismatches);
}

#[test]
fn int_conversion_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("int_convert.tsv") {
        tstate::clear();
        let got = match row[0].as_str() {
            "to_str" => {
                let a = parse_int(&row[1]);
                let base = row[2].parse::<u32>().unwrap();
                // SAFETY: `a` is an int this runtime built.
                unsafe { encode_opt(long::format(a.cast(), base, row[3] == "1")) }
            }
            "from_str" => {
                let base: c_int = row[1].parse().unwrap();
                let c = CString::new(row[2].as_str()).unwrap();
                // SAFETY: `c` is a null-terminated string the call only reads.
                unsafe {
                    encode_ptr(long::PyLong_FromString(
                        c.as_ptr(),
                        std::ptr::null_mut(),
                        base,
                    ))
                }
            }
            "from_double" => encode_ptr(long::PyLong_FromDouble(from_bits(&row[1]))),
            "as_ul_mask" => {
                let a = parse_int(&row[1]);
                // SAFETY: `a` is an int this runtime built.
                unsafe { long::PyLong_AsUnsignedLongMask(a).to_string() }
            }
            "as_ull_mask" => {
                let a = parse_int(&row[1]);
                // SAFETY: `a` is an int this runtime built.
                unsafe { long::PyLong_AsUnsignedLongLongMask(a).to_string() }
            }
            "as_double" => {
                let a = parse_int(&row[1]);
                // SAFETY: `a` is an int this runtime built.
                let value = unsafe { long::PyLong_AsDouble(a) };
                if tstate::occurred().is_null() {
                    format!("f:{}", bits_hex(value))
                } else {
                    error_text()
                }
            }
            other => panic!("unknown kind {other}"),
        };
        check(&mut mismatches, "int_convert", lineno, &row, got);
    }
    finish("int_convert", mismatches);
}

#[test]
fn int_hashes_match_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("int_hash.tsv") {
        tstate::clear();
        let a = parse_int(&row[1]);
        // SAFETY: the slot takes the int we just built.
        let got = unsafe { (long::PyLong_Type.tp_hash.unwrap())(a) }.to_string();
        check(&mut mismatches, "int_hash", lineno, &row, got);
    }
    finish("int_hash", mismatches);
}

#[test]
fn float_arithmetic_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("float_arithmetic.tsv") {
        tstate::clear();
        let a = float_::PyFloat_FromDouble(from_bits(&row[1]));
        let b = float_::PyFloat_FromDouble(from_bits(&row[2]));
        // SAFETY: the slots take any objects; the operands are floats.
        let result = unsafe {
            let table = float_::PyFloat_Type.tp_as_number;
            match row[0].as_str() {
                "+" => (*table).nb_add.unwrap()(a, b),
                "-" => (*table).nb_subtract.unwrap()(a, b),
                "*" => (*table).nb_multiply.unwrap()(a, b),
                "/" => (*table).nb_true_divide.unwrap()(a, b),
                "%" => (*table).nb_remainder.unwrap()(a, b),
                "//" => (*table).nb_floor_divide.unwrap()(a, b),
                "pow" => (*table).nb_power.unwrap()(a, b, singletons::none()),
                other => panic!("unknown op {other}"),
            }
        };
        check(
            &mut mismatches,
            "float_arithmetic",
            lineno,
            &row,
            encode_ptr(result),
        );
    }
    finish("float_arithmetic", mismatches);
}

#[test]
fn float_repr_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("float_repr.tsv") {
        tstate::clear();
        let value = from_bits(&row[1]);
        let got = match row[0].as_str() {
            // repr uses Py_DTSF_ADD_DOT_0 (floatobject.c's float_repr).
            "repr" => encode_opt(float_::double_to_string(
                value,
                b'r',
                0,
                float_::PY_DTSF_ADD_DOT_0,
                std::ptr::null_mut(),
            )),
            "pyos" => {
                let code = row[2].as_bytes()[0];
                let precision = row[3].parse::<i32>().unwrap();
                encode_opt(float_::double_to_string(
                    value,
                    code,
                    precision,
                    0,
                    std::ptr::null_mut(),
                ))
            }
            other => panic!("unknown kind {other}"),
        };
        check(&mut mismatches, "float_repr", lineno, &row, got);
    }
    finish("float_repr", mismatches);
}

#[test]
fn float_format_matches_cpython() {
    init();
    let mismatches = run_table("float_format.tsv", &|_lineno, row| {
        tstate::clear();
        Some(encode_opt(float_::format_value(
            from_bits(&row[1]),
            &row[2],
        )))
    });
    finish("float_format", mismatches);
}

#[test]
fn float_hashes_match_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("float_hash.tsv") {
        tstate::clear();
        let a = float_::PyFloat_FromDouble(from_bits(&row[1]));
        // SAFETY: the slot takes the float we just built.
        let got = unsafe { (float_::PyFloat_Type.tp_hash.unwrap())(a) }.to_string();
        check(&mut mismatches, "float_hash", lineno, &row, got);
    }
    finish("float_hash", mismatches);
}

#[test]
fn complex_arithmetic_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("complex_arithmetic.tsv") {
        tstate::clear();
        let a = complex::PyComplex_FromDoubles(from_bits(&row[1]), from_bits(&row[2]));
        let b = complex::PyComplex_FromDoubles(from_bits(&row[3]), from_bits(&row[4]));
        // SAFETY: the slots take any objects; the operands are complex.
        let result = unsafe {
            let table = complex::PyComplex_Type.tp_as_number;
            match row[0].as_str() {
                "+" => (*table).nb_add.unwrap()(a, b),
                "-" => (*table).nb_subtract.unwrap()(a, b),
                "*" => (*table).nb_multiply.unwrap()(a, b),
                "/" => (*table).nb_true_divide.unwrap()(a, b),
                "pow" => (*table).nb_power.unwrap()(a, b, singletons::none()),
                other => panic!("unknown op {other}"),
            }
        };
        check(
            &mut mismatches,
            "complex_arithmetic",
            lineno,
            &row,
            encode_ptr(result),
        );
    }
    finish("complex_arithmetic", mismatches);
}

#[test]
fn complex_repr_and_format_match_cpython() {
    init();
    let mismatches = run_table("complex_repr_format.tsv", &|_lineno, row| {
        tstate::clear();
        let real = from_bits(&row[1]);
        let imag = from_bits(&row[2]);
        match row[0].as_str() {
            "repr" => {
                let v = complex::PyComplex_FromDoubles(real, imag);
                // SAFETY: `v` is a complex this runtime built.
                Some(unsafe { complex::repr_string(v) })
            }
            "format" => Some(encode_opt(complex::format_value(real, imag, &row[3]))),
            other => panic!("unknown kind {other}"),
        }
    });
    finish("complex_repr_format", mismatches);
}

#[test]
fn complex_hashes_match_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("complex_hash.tsv") {
        tstate::clear();
        let a = complex::PyComplex_FromDoubles(from_bits(&row[1]), from_bits(&row[2]));
        // SAFETY: the slot takes the complex we just built.
        let got = unsafe { (complex::PyComplex_Type.tp_hash.unwrap())(a) }.to_string();
        check(&mut mismatches, "complex_hash", lineno, &row, got);
    }
    finish("complex_hash", mismatches);
}

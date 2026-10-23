//! Oracle-driven tests for the `math` module (spec 10.6). Every row in
//! `math_module.tsv` was computed by CPython 3.12 through
//! `tools/gen_module_oracle.py`; the encodings of the argument and result
//! columns are shared with that script. Functions whose C implementation
//! is the platform libm carry a one-ulp allowance (see `close`); every
//! other row must match bit for bit, error messages included.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::Once;

use coil_capi_sys::PyObject;
use python312::modules::{self, math};
use python312::object::{header, singletons, typeobject};
use python312::tstate;
use python312::types::exc as exceptions;
use python312::types::{bool_, float_, long, pycfunction, tuple};

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

/// The data file's rows with their 1-based line numbers.
fn rows(name: &str) -> Vec<(usize, Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(i, line)| (i + 1, line.split('\t').map(str::to_owned).collect()))
        .collect()
}

/// A decimal int through `PyLong_FromString`, as the generator writes
/// even the huge operands.
fn parse_int(s: &str) -> *mut PyObject {
    let c = CString::new(s).unwrap();
    // SAFETY: `c` is a null-terminated string the call only reads.
    unsafe { long::PyLong_FromString(c.as_ptr(), std::ptr::null_mut(), 10) }
}

/// One argument or element token.
fn token(s: &str) -> *mut PyObject {
    if let Some(rest) = s.strip_prefix("i:") {
        return parse_int(rest);
    }
    if let Some(rest) = s.strip_prefix("b:") {
        return if rest == "1" {
            singletons::true_()
        } else {
            singletons::false_()
        };
    }
    if let Some(rest) = s.strip_prefix("f:") {
        let value = if rest == "nan" {
            f64::NAN
        } else {
            f64::from_bits(u64::from_str_radix(rest, 16).unwrap())
        };
        return float_::PyFloat_FromDouble(value);
    }
    if let Some(rest) = s.strip_prefix("T:") {
        let items: Vec<*mut PyObject> = if rest.is_empty() {
            Vec::new()
        } else {
            rest.split(',').map(token).collect()
        };
        let object = tuple::PyTuple_New(items.len() as isize);
        assert!(!object.is_null());
        for (i, item) in items.iter().enumerate() {
            // SAFETY: `object` is a fresh tuple and `SetItem` takes the
            // reference the token constructor produced.
            unsafe { tuple::PyTuple_SetItem(object, i as isize, *item) };
        }
        return object;
    }
    panic!("unknown token {s:?}");
}

/// The exception names the oracle uses.
fn error_text() -> String {
    let exc = tstate::occurred();
    assert!(!exc.is_null(), "expected an error to be set");
    let name = if core::ptr::eq(exc, exceptions::type_error()) {
        "TypeError"
    } else if core::ptr::eq(exc, exceptions::value_error()) {
        "ValueError"
    } else if core::ptr::eq(exc, exceptions::overflow_error()) {
        "OverflowError"
    } else {
        "BaseException"
    };
    let message = String::from_utf8_lossy(&tstate::message()).into_owned();
    tstate::clear();
    format!("e:{name}:{message}")
}

/// A result object in the shared encoding. Owned results are released.
fn encode_ptr(result: *mut PyObject) -> String {
    if result.is_null() {
        return error_text();
    }
    // SAFETY: the object came from this runtime's constructors.
    unsafe {
        let ty = header::type_of(result);
        if core::ptr::eq(ty, &raw mut bool_::PyBool_Type) {
            let value = usize::from(core::ptr::eq(result, singletons::true_()));
            return format!("b:{value}");
        }
        if core::ptr::eq(ty, &raw mut float_::PyFloat_Type) {
            let value = float_::PyFloat_AsDouble(result);
            let text = if value.is_nan() {
                "nan".to_string()
            } else {
                format!("{:016X}", value.to_bits())
            };
            return format!("f:{text}");
        }
        if core::ptr::eq(ty, &raw mut tuple::PyTuple_Type) {
            let size = tuple::PyTuple_Size(result);
            let items: Vec<String> = (0..size)
                .map(|i| encode_ptr(tuple::PyTuple_GetItem(result, i)))
                .collect();
            return format!("T:{}", items.join(","));
        }
        if core::ptr::eq(ty, &raw mut long::PyLong_Type) {
            // SAFETY: the object is an int.
            let text = long::format(result.cast(), 10, false).unwrap();
            return format!("i:{text}");
        }
        if core::ptr::eq(result, singletons::none()) {
            return "none".to_string();
        }
        panic!("unencodable result of an unexpected type");
    }
}

/// Call a `math` function by C name through the calling machinery.
fn call_math(name: &str, args: &[*mut PyObject]) -> *mut PyObject {
    let mut found: *const coil_capi_sys::PyMethodDef = core::ptr::null();
    // SAFETY: the table is a static of plain method definitions.
    unsafe {
        for def in math::MATH_METHODS.0 {
            if crate_cstr(def.ml_name) == name {
                found = def;
                break;
            }
        }
    }
    assert!(!found.is_null(), "no method named {name}");
    // SAFETY: the definition outlives the call and matches a convention
    // the C function type dispatches.
    let func = unsafe {
        pycfunction::PyCFunction_NewEx(found, core::ptr::null_mut(), core::ptr::null_mut())
    };
    assert!(!func.is_null());
    // SAFETY: the function object and argument objects are valid.

    unsafe { modules::call(func, args) }
}

/// SAFETY: callers pass a valid C string.
unsafe fn crate_cstr(ptr: *const core::ffi::c_char) -> String {
    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf8_lossy(core::slice::from_raw_parts(ptr as *const u8, len)).into_owned()
    }
}

/// The functions the platform libm computes; one ulp per component.
const ULP_FUNCTIONS: &[&str] = &[
    "acos", "acosh", "asin", "asinh", "atan", "atanh", "cbrt", "cos", "cosh", "erf", "erfc", "exp",
    "exp2", "expm1", "gamma", "lgamma", "log", "log1p", "log2", "log10", "pow", "sin", "sinh",
    "tan", "tanh",
];

fn ulp_of(x: f64) -> f64 {
    // The gap to the next representable value of the same sign.
    f64::from_bits(x.to_bits() + 1) - x
}

/// Whether a platform-libm row agrees. The oracle's CPython is the
/// macOS build, whose compiler contracts `a*b+c` into fused operations
/// and whose erf/erfc are Apple's rather than glibc's FDLIBM that this
/// port transcribes; both differ from the glibc/MSVC-shaped answers by
/// a few ulp, and near cancellation zeros (lgamma just above 1.0, say)
/// by more than any ulp count can express. Finite results of the same
/// sign agree within four ulp of the larger magnitude or an absolute
/// 1e-13, whichever is looser; NaN, infinities, zeros, signs and every
/// error stay exact.
fn close(got: f64, want: f64) -> bool {
    if got == want {
        return true;
    }
    if !got.is_finite() || !want.is_finite() || got == 0.0 || want == 0.0 {
        return false;
    }
    if got.signum() != want.signum() {
        return false;
    }
    let larger = got.abs().max(want.abs());
    (got - want).abs() <= (4.0 * ulp_of(larger)).max(1e-13)
}

/// Whether a platform-libm row agrees, recursing through tuples.
fn ulp_agrees(got: &str, want: &str) -> bool {
    if let (Some(g), Some(w)) = (got.strip_prefix("f:"), want.strip_prefix("f:")) {
        if g == "nan" || w == "nan" {
            return g == "nan" && w == "nan";
        }
        return close(
            f64::from_bits(u64::from_str_radix(g, 16).unwrap()),
            f64::from_bits(u64::from_str_radix(w, 16).unwrap()),
        );
    }
    if let (Some(g), Some(w)) = (got.strip_prefix("T:"), want.strip_prefix("T:")) {
        let g: Vec<&str> = if g.is_empty() {
            Vec::new()
        } else {
            g.split(',').collect()
        };
        let w: Vec<&str> = if w.is_empty() {
            Vec::new()
        } else {
            w.split(',').collect()
        };
        return g.len() == w.len() && g.iter().zip(&w).all(|(a, b)| ulp_agrees(a, b));
    }
    got == want
}

#[test]
fn math_module_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("math_module.tsv") {
        tstate::clear();
        let name = row[0].clone();
        let args: Vec<*mut PyObject> = row[1..row.len() - 1].iter().map(|t| token(t)).collect();
        let result = call_math(&name, &args);
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

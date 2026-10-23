//! Shared machinery for the oracle-driven native module tests (spec
//! 10.6): the data-file reader, the argument and result encodings and
//! the per-module calling helper. The encodings are shared with
//! `tools/gen_module_oracle.py`.

#![allow(dead_code)]

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::Once;

use coil_capi_sys::{PyMethodDef, PyObject};
use python312::modules::{self, MethodTable};
use python312::object::{header, singletons, typeobject};
use python312::tstate;
use python312::types::bytes;
use python312::types::exc as exceptions;
use python312::types::{bool_, complex, float_, long, pycfunction, tuple};

static INIT: Once = Once::new();

pub fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}
/// The data file's rows with their 1-based line numbers.
pub fn rows(name: &str) -> Vec<(usize, Vec<String>)> {
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
pub fn parse_int(s: &str) -> *mut PyObject {
    let c = CString::new(s).unwrap();
    // SAFETY: `c` is a null-terminated string the call only reads.
    unsafe { long::PyLong_FromString(c.as_ptr(), std::ptr::null_mut(), 10) }
}

/// One argument or element token.
pub fn token(s: &str) -> *mut PyObject {
    if s == "none" {
        return singletons::none();
    }
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
    if let Some(rest) = s.strip_prefix("y:") {
        let value: Vec<u8> = (0..rest.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&rest[i..i + 2], 16).unwrap())
            .collect();
        // SAFETY: the pointer/length pair describes `value`, which the
        // call copies into the new bytes object.
        return unsafe {
            bytes::PyBytes_FromStringAndSize(value.as_ptr().cast(), value.len() as isize)
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
    if let Some(rest) = s.strip_prefix("c:") {
        let mut parts = rest.splitn(2, ',');
        let real = parts.next().unwrap();
        let imag = parts.next().expect("complex tokens carry two parts");
        let parse = |t: &str| {
            if t == "nan" {
                f64::NAN
            } else {
                f64::from_bits(u64::from_str_radix(t, 16).unwrap())
            }
        };
        return complex::PyComplex_FromDoubles(parse(real), parse(imag));
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
pub fn error_text() -> String {
    let exc = tstate::occurred();
    assert!(!exc.is_null(), "expected an error to be set");
    let name = if core::ptr::eq(exc, exceptions::type_error()) {
        "TypeError"
    } else if core::ptr::eq(exc, exceptions::value_error()) {
        "ValueError"
    } else if core::ptr::eq(exc, exceptions::overflow_error()) {
        "OverflowError"
    } else if core::ptr::eq(exc, modules::binascii::error()) {
        "Error"
    } else {
        "BaseException"
    };
    let message = String::from_utf8_lossy(&tstate::message()).into_owned();
    tstate::clear();
    format!("e:{name}:{message}")
}

/// A result object in the shared encoding. Owned results are released.
pub fn encode_ptr(result: *mut PyObject) -> String {
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
        if core::ptr::eq(ty, &raw mut complex::PyComplex_Type) {
            let cval = complex::PyComplex_AsCComplex(result);
            let part = |v: f64| {
                if v.is_nan() {
                    "nan".to_string()
                } else {
                    format!("{:016X}", v.to_bits())
                }
            };
            return format!("c:{},{}", part(cval.real), part(cval.imag));
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
        if core::ptr::eq(ty, &raw mut bytes::PyBytes_Type) {
            // SAFETY: the object is a bytes value; the string is valid for
            // its size and the result is only read.
            let raw = bytes::PyBytes_AsString(result);
            let size = bytes::PyBytes_Size(result);
            let text: String = core::slice::from_raw_parts(raw as *const u8, size as usize)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            return format!("y:{text}");
        }
        if core::ptr::eq(result, singletons::none()) {
            return "none".to_string();
        }
        panic!("unencodable result of an unexpected type");
    }
}

/// Call a module function by C name through the calling machinery.
pub fn call(table: &MethodTable, name: &str, args: &[*mut PyObject]) -> *mut PyObject {
    let mut found: *const PyMethodDef = core::ptr::null();
    // SAFETY: the table is a static of plain method definitions.
    unsafe {
        for def in table.0 {
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
pub unsafe fn crate_cstr(ptr: *const core::ffi::c_char) -> String {
    unsafe {
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf8_lossy(core::slice::from_raw_parts(ptr as *const u8, len)).into_owned()
    }
}
pub fn ulp_of(x: f64) -> f64 {
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
/// 1e-13, whichever is looser — the absolute bound also admits the
/// subnormal underflow where MSVC's atan2 yields a signed zero for an
/// input the macOS build keeps subnormal — and NaN, infinities, zero
/// signs and every error stay exact.
pub fn close(got: f64, want: f64) -> bool {
    if got == want {
        return true;
    }
    if !got.is_finite() || !want.is_finite() {
        return false;
    }
    if got.signum() != want.signum() {
        return false;
    }
    let larger = got.abs().max(want.abs());
    (got - want).abs() <= (4.0 * ulp_of(larger)).max(1e-13)
}

/// Whether a platform-libm row agrees, recursing through tuples.
pub fn ulp_agrees(got: &str, want: &str) -> bool {
    if let (Some(g), Some(w)) = (got.strip_prefix("f:"), want.strip_prefix("f:")) {
        if g == "nan" || w == "nan" {
            return g == "nan" && w == "nan";
        }
        return close(
            f64::from_bits(u64::from_str_radix(g, 16).unwrap()),
            f64::from_bits(u64::from_str_radix(w, 16).unwrap()),
        );
    }
    if let (Some(g), Some(w)) = (got.strip_prefix("c:"), want.strip_prefix("c:")) {
        fn parts(t: &str) -> (&str, &str) {
            let mut it = t.splitn(2, ',');
            (it.next().unwrap(), it.next().expect("two parts"))
        }
        let (gr, gi) = parts(g);
        let (wr, wi) = parts(w);
        let agrees = |g: &str, w: &str| {
            g == "nan" && w == "nan"
                || g != "nan"
                    && w != "nan"
                    && close(
                        f64::from_bits(u64::from_str_radix(g, 16).unwrap()),
                        f64::from_bits(u64::from_str_radix(w, 16).unwrap()),
                    )
        };
        return agrees(gr, wr) && agrees(gi, wi);
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

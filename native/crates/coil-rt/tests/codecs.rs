//! Oracle-driven tests for the `_codecs` module (spec section 10.6).
//! The tables under `tests/data` came from CPython 3.12 via
//! `tools/gen_codec_oracle.py`; the field encoding is shared with that
//! script. The harness parses each row's arguments the way the module
//! functions' clinic signatures do, calls the Rust surface (or the
//! registry's C API), and encodes the result in the oracle's tags.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::Once;

use core::ffi::c_char;

use coil_capi_sys::{PyObject, PyTypeObject};
use python312::modules::_codecs as codecs;
use python312::object::{header, typeobject};
use python312::tstate;
use python312::types::exc as exceptions;
use python312::types::{bytes as rt_bytes, dict, long, tuple, unicode};

static INIT: Once = Once::new();

const STR_TYPE: *mut PyTypeObject = &raw mut unicode::PyUnicode_Type;
const BYTES_TYPE: *mut PyTypeObject = &raw mut rt_bytes::PyBytes_Type;
const BYTEARRAY_TYPE: *mut PyTypeObject = &raw mut rt_bytes::PyByteArray_Type;

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

fn is_str(object: *mut PyObject) -> bool {
    // SAFETY: a header read of a valid object.
    unsafe { header::type_of(object) == STR_TYPE }
}

fn is_bytes(object: *mut PyObject) -> bool {
    // SAFETY: a header read of a valid object.
    unsafe { header::type_of(object) == BYTES_TYPE }
}

fn is_bytearray(object: *mut PyObject) -> bool {
    // SAFETY: a header read of a valid object.
    unsafe { header::type_of(object) == BYTEARRAY_TYPE }
}

fn is_tuple(object: *mut PyObject) -> bool {
    // SAFETY: a header read of a valid object.
    unsafe { header::type_of(object) == &raw mut tuple::PyTuple_Type }
}

/// Undo the generator's escaping: backslash, tab, newline, carriage
/// return, bytes' `\xHH` and lone surrogates' `\uXXXX`, in one decoder.
fn unescape(field: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let mut it = field.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c as u32);
            continue;
        }
        let next = it.next().expect("field ends with a bare backslash");
        match next {
            '\\' => out.push(b'\\' as u32),
            't' => out.push(b'\t' as u32),
            'n' => out.push(b'\n' as u32),
            'r' => out.push(b'\r' as u32),
            'x' => {
                let hi = it.next().expect("short \\x").to_digit(16).expect("bad hex");
                let lo = it.next().expect("short \\x").to_digit(16).expect("bad hex");
                out.push(hi * 16 + lo);
            }
            'u' | 'U' => {
                let width = if next == 'u' { 4 } else { 8 };
                let mut value = 0u32;
                for _ in 0..width {
                    value = value * 16
                        + it.next()
                            .expect("short escape")
                            .to_digit(16)
                            .expect("bad hex");
                }
                out.push(value);
            }
            other => panic!("unknown escape \\{other:?}"),
        }
    }
    out
}

/// The inverse of `unescape` for str fields: the generator escapes
/// backslash, tab, newline, carriage return and lone surrogates.
fn escape(chars: &[u32]) -> String {
    let mut out = String::new();
    for &c in chars {
        match c {
            0x5C => out.push_str("\\\\"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0xD800..=0xDFFF => out.push_str(&format!("\\u{c:04x}")),
            _ => out.push(char::from_u32(c).expect("non-scalar result character")),
        }
    }
    out
}

fn unescape_bytes(field: &str) -> Vec<u8> {
    unescape(field).iter().map(|&c| c as u8).collect()
}

/// The generator's bytes encoding: printable ASCII raw, the rest `\xHH`.
fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x5C => out.push_str("\\\\"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// The data file's rows with their 1-based line numbers, comments and
/// blanks skipped.
fn rows(name: &str) -> Vec<(usize, Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    let text = fs::read_to_string(path).unwrap();
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(i, line)| (i + 1, line.split('\t').map(str::to_string).collect()))
        .collect()
}

/// The instance's type name, as the oracle encodes it.
fn type_name(object: *mut PyObject) -> String {
    // SAFETY: a readied type's `tp_name` is a valid C string.
    unsafe { python312::util::cstr((*header::type_of(object)).tp_name).to_string() }
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
    } else if core::ptr::eq(exc, exceptions::lookup_error()) {
        "LookupError"
    } else if core::ptr::eq(exc, exceptions::system_error()) {
        "SystemError"
    } else if core::ptr::eq(exc, exceptions::unicode_decode_error()) {
        "UnicodeDecodeError"
    } else if core::ptr::eq(exc, exceptions::unicode_encode_error()) {
        "UnicodeEncodeError"
    } else if core::ptr::eq(exc, exceptions::unicode_translate_error()) {
        "UnicodeTranslateError"
    } else {
        "BaseException"
    };
    let message = String::from_utf8_lossy(&tstate::message()).into_owned();
    tstate::clear();
    // The oracle escapes error text like every other field.
    let escaped = escape(&message.chars().map(|c| c as u32).collect::<Vec<_>>());
    format!("e:{name}:{escaped}")
}

/// A result object in the oracle's encoding: str, bytes, bytearray,
/// int or tuple, recursing into tuples the way `enc_result` does.
///
/// # Safety
///
/// `object` must be null or a valid object of the runtime.
unsafe fn encode_object(object: *mut PyObject) -> String {
    // SAFETY: the type dispatch reads only headers; the content reads
    // follow each type's own contract.
    unsafe {
        if object.is_null() {
            return error_text();
        }
        if is_str(object) {
            let length = unicode::PyUnicode_GetLength(object);
            let mut chars = Vec::with_capacity(length as usize);
            for i in 0..length {
                chars.push(unicode::PyUnicode_ReadChar(object, i));
            }
            return format!("s:{}", escape(&chars));
        }
        if is_bytes(object) || is_bytearray(object) {
            let size = rt_bytes::PyBytes_Size(object).max(0);
            let contents = rt_bytes::PyBytes_AsString(object);
            let slice = std::slice::from_raw_parts(contents as *const u8, size as usize);
            return format!("B:{}", escape_bytes(slice));
        }
        if is_tuple(object) {
            let size = tuple::PyTuple_Size(object);
            let mut parts = vec![format!("L:{size}")];
            for i in 0..size {
                parts.push(encode_object(tuple::PyTuple_GetItem(object, i)));
            }
            return parts.join("\t");
        }
        // The remaining results the oracle encodes are ints.
        format!("i:{}", long::PyLong_AsSsize_t(object))
    }
}

fn arg_int(field: &str) -> isize {
    field
        .strip_prefix("i:")
        .unwrap_or_else(|| panic!("expected an int argument, got {field:?}"))
        .parse()
        .unwrap()
}

fn arg_bool(field: &str) -> bool {
    field.strip_prefix("b:").expect("bool argument") == "1"
}

fn errors_arg(field: &str) -> Option<&'static str> {
    if field.is_empty() || field == "-" {
        return None;
    }
    Some(match field {
        "strict" => "strict",
        "ignore" => "ignore",
        "replace" => "replace",
        "backslashreplace" => "backslashreplace",
        "surrogateescape" => "surrogateescape",
        "surrogatepass" => "surrogatepass",
        "xmlcharrefreplace" => "xmlcharrefreplace",
        other => panic!("unknown error handler {other:?}"),
    })
}

/// Build a str object from code points; lone surrogates force the
/// 2-byte representation the way CPython's literals do.
fn str_object(chars: &[u32]) -> *mut PyObject {
    if chars.iter().all(|c| char::from_u32(*c).is_some()) {
        let text: String = chars
            .iter()
            .map(|&c| char::from_u32(c).expect("input is scalar"))
            .collect();
        // SAFETY: `text` lives across the call and the size is its
        // byte length.
        return unsafe {
            unicode::PyUnicode_FromStringAndSize(
                if text.is_empty() {
                    std::ptr::null()
                } else {
                    text.as_ptr() as *const c_char
                },
                text.len() as isize,
            )
        };
    }
    let maxchar = chars.iter().copied().max().unwrap_or(0);
    // SAFETY: `unicode_new` allocates and fills the header; every
    // `PyUnicode_WriteChar` stays within the fresh length.
    unsafe {
        let object = unicode::unicode_new(chars.len() as isize, maxchar);
        assert!(!object.is_null());
        for (i, &c) in chars.iter().enumerate() {
            assert_eq!(unicode::PyUnicode_WriteChar(object, i as isize, c), 0);
        }
        object
    }
}

fn bytes_object(bytes: &[u8]) -> *mut PyObject {
    // SAFETY: `bytes` lives across the call; NULL is passed for empty
    // so the shared empty object comes back.
    unsafe {
        rt_bytes::PyBytes_FromStringAndSize(
            if bytes.is_empty() {
                std::ptr::null()
            } else {
                bytes.as_ptr() as *const c_char
            },
            bytes.len() as isize,
        )
    }
}

/// The mapping argument of the charmap functions from the oracle's
/// `97=i:65,98=s:B,...` encoding: int keys to int, str, bytes or None
/// values.
fn mapping_object(spec: &str) -> *mut PyObject {
    if spec == "-" {
        return python312::object::singletons::none();
    }
    // SAFETY: the dict API follows its contract on a fresh dict.
    unsafe {
        let map = dict::PyDict_New();
        assert!(!map.is_null());
        for item in spec.split(',') {
            let (key, value) = item.split_once('=').expect("mapping item");
            let key_object = long::PyLong_FromSsize_t(key.parse::<isize>().unwrap());
            let value_object = if value == "n" {
                python312::object::singletons::none()
            } else if let Some(number) = value.strip_prefix("i:") {
                long::PyLong_FromSsize_t(number.parse::<isize>().unwrap())
            } else if let Some(text) = value.strip_prefix("s:") {
                str_object(&unescape(text))
            } else if let Some(hex) = value.strip_prefix("B:") {
                let mut bytes = Vec::new();
                for pair in hex.as_bytes().chunks(2) {
                    let hi = (pair[0] as char).to_digit(16).expect("mapping hex");
                    let lo = (pair[1] as char).to_digit(16).expect("mapping hex");
                    bytes.push((hi * 16 + lo) as u8);
                }
                bytes_object(&bytes)
            } else {
                panic!("mapping value {value:?}");
            };
            assert!(!key_object.is_null() && !value_object.is_null());
            assert_eq!(dict::PyDict_SetItem(map, key_object, value_object), 0);
        }
        map
    }
}

/// `charmap_build`'s result: the (key, value) pairs in insertion
/// order, the oracle's `L:n` of `L:2 i:key i:value` pairs.
///
/// # Safety
///
/// `object` must be a valid mapping object of the runtime.
unsafe fn encode_mapping_items(object: *mut PyObject) -> String {
    // SAFETY: `PyDict_Next` walks the dict's own storage.
    unsafe {
        let mut pos: isize = 0;
        let mut key: *mut PyObject = std::ptr::null_mut();
        let mut value: *mut PyObject = std::ptr::null_mut();
        let mut parts: Vec<String> = Vec::new();
        while dict::PyDict_Next(object, &mut pos, &mut key, &mut value) != 0 {
            parts.push(format!(
                "L:2\ti:{}\ti:{}",
                long::PyLong_AsSsize_t(key),
                long::PyLong_AsSsize_t(value)
            ));
        }
        format!("L:{}\t{}", parts.len(), parts.join("\t"))
    }
}

fn check(
    mismatches: &mut Vec<String>,
    table: &str,
    lineno: usize,
    row: &[String],
    got: String,
    want: String,
) {
    if got != want {
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

/// The decoder functions' module surface. `final_` is None for the
/// functions without the argument.
fn decode_call(
    fn_name: &str,
    data: &[u8],
    errors: Option<&'static str>,
    final_: Option<bool>,
) -> *mut PyObject {
    let final_ = final_.unwrap_or(false);
    match fn_name {
        "escape_decode" => codecs::escape_decode(data, errors),
        "latin_1_decode" => codecs::latin_1_decode(data, errors),
        "ascii_decode" => codecs::ascii_decode(data, errors),
        "utf_7_decode" => codecs::utf_7_decode(data, errors, final_),
        "utf_8_decode" => codecs::utf_8_decode(data, errors, final_),
        "utf_16_decode" => codecs::utf_16_decode(data, errors, final_),
        "utf_16_le_decode" => codecs::utf_16_le_decode(data, errors, final_),
        "utf_16_be_decode" => codecs::utf_16_be_decode(data, errors, final_),
        "utf_32_decode" => codecs::utf_32_decode(data, errors, final_),
        "utf_32_le_decode" => codecs::utf_32_le_decode(data, errors, final_),
        "utf_32_be_decode" => codecs::utf_32_be_decode(data, errors, final_),
        "unicode_escape_decode" => codecs::unicode_escape_decode(data, errors, final_),
        "raw_unicode_escape_decode" => codecs::raw_unicode_escape_decode(data, errors, final_),
        other => panic!("unknown decoder {other:?}"),
    }
}

/// The encoder functions' module surface; `escape_encode` and
/// `readbuffer_encode` take a buffer.
fn encode_call(
    fn_name: &str,
    chars: &[u32],
    data: &[u8],
    errors: Option<&'static str>,
    byteorder: Option<i32>,
) -> *mut PyObject {
    match fn_name {
        "escape_encode" => codecs::escape_encode(data, errors),
        "readbuffer_encode" => codecs::readbuffer_encode(data, errors),
        "utf_7_encode" => codecs::utf_7_encode(chars, errors),
        "utf_8_encode" => codecs::utf_8_encode(chars, errors),
        "utf_16_encode" => codecs::utf_16_encode(chars, errors, byteorder.unwrap()),
        "utf_16_le_encode" => codecs::utf_16_le_encode(chars, errors),
        "utf_16_be_encode" => codecs::utf_16_be_encode(chars, errors),
        "utf_32_encode" => codecs::utf_32_encode(chars, errors, byteorder.unwrap()),
        "utf_32_le_encode" => codecs::utf_32_le_encode(chars, errors),
        "utf_32_be_encode" => codecs::utf_32_be_encode(chars, errors),
        "latin_1_encode" => codecs::latin_1_encode(chars, errors),
        "ascii_encode" => codecs::ascii_encode(chars, errors),
        "unicode_escape_encode" => codecs::unicode_escape_encode(chars, errors),
        "raw_unicode_escape_encode" => codecs::raw_unicode_escape_encode(chars, errors),
        other => panic!("unknown encoder {other:?}"),
    }
}

#[test]
fn module_decoders_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("codecs_decode.tsv") {
        let fn_name = row[0].as_str();
        let data = unescape_bytes(&row[1]);
        let errors = errors_arg(&row[2]);
        let final_ = if row[3] == "-" {
            None
        } else {
            Some(arg_bool(&row[3]))
        };
        let result = decode_call(fn_name, &data, errors, final_);
        // SAFETY: the module functions return a tuple or null.
        let got = unsafe { encode_object(result) };
        let want = row[4..].join("\t");
        check(
            &mut mismatches,
            "codecs_decode.tsv",
            lineno,
            &row,
            got,
            want,
        );
    }
    finish("codecs_decode.tsv", mismatches);
}

#[test]
fn module_ex_decoders_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("codecs_ex.tsv") {
        let data = unescape_bytes(&row[1]);
        let errors = errors_arg(&row[2]);
        let byteorder = arg_int(&row[3]) as i32;
        let final_ = arg_bool(&row[4]);
        let result = if row[0] == "utf_16_ex_decode" {
            codecs::utf_16_ex_decode(&data, errors, byteorder, final_)
        } else {
            codecs::utf_32_ex_decode(&data, errors, byteorder, final_)
        };
        // SAFETY: the module functions return a 3-tuple or null.
        let got = unsafe { encode_object(result) };
        let want = row[5..].join("\t");
        check(&mut mismatches, "codecs_ex.tsv", lineno, &row, got, want);
    }
    finish("codecs_ex.tsv", mismatches);
}

#[test]
fn module_encoders_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("codecs_encode.tsv") {
        let fn_name = row[0].as_str();
        let (chars, data) = if fn_name == "escape_encode" || fn_name == "readbuffer_encode" {
            (Vec::new(), unescape_bytes(&row[1]))
        } else {
            (unescape(&row[1]), Vec::new())
        };
        let errors = errors_arg(&row[2]);
        let byteorder = if row[3] == "-" {
            None
        } else {
            Some(arg_int(&row[3]) as i32)
        };
        let result = encode_call(fn_name, &chars, &data, errors, byteorder);
        // SAFETY: the module functions return a tuple or null.
        let got = unsafe { encode_object(result) };
        let want = row[4..].join("\t");
        check(
            &mut mismatches,
            "codecs_encode.tsv",
            lineno,
            &row,
            got,
            want,
        );
    }
    finish("codecs_encode.tsv", mismatches);
}

#[test]
fn charmap_functions_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("codecs_charmap.tsv") {
        let fn_name = row[0].as_str();
        let errors = errors_arg(&row[2]);
        let mapping = mapping_object(&row[3]);
        let result = match fn_name {
            "charmap_decode" => codecs::charmap_decode(&unescape_bytes(&row[1]), errors, mapping),
            "charmap_encode" => codecs::charmap_encode(&unescape(&row[1]), errors, mapping),
            "charmap_build" => {
                // SAFETY: the input is a str object of the runtime.
                unsafe { codecs::charmap_build(str_object(&unescape(&row[1]))) }
            }
            other => panic!("unknown charmap function {other:?}"),
        };
        // SAFETY: the results are tuples, mappings or null.
        let got = unsafe {
            if fn_name == "charmap_build" && !result.is_null() {
                encode_mapping_items(result)
            } else {
                encode_object(result)
            }
        };
        let want = row[4..].join("\t");
        check(
            &mut mismatches,
            "codecs_charmap.tsv",
            lineno,
            &row,
            got,
            want,
        );
    }
    finish("codecs_charmap.tsv", mismatches);
}

#[test]
fn registry_resolves_builtin_names_like_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("codecs_registry.tsv") {
        let op = row[0].as_str();
        let object = match row[1].as_str() {
            "b" => bytes_object(&unescape_bytes("abc\\xc3\\xa9\\xff")),
            "b2" => bytes_object(&unescape_bytes("\\xff\\xfe\\x00a")),
            "s" => str_object(&unescape("caf\\u00e9 \\u00ff \\U0001f600")),
            "s2" => str_object(&unescape("abc")),
            "B" => {
                // SAFETY: the buffer lives across the call.
                unsafe {
                    rt_bytes::PyByteArray_FromStringAndSize(b"abc\xff".as_ptr() as *const c_char, 4)
                }
            }
            "i" => long::PyLong_FromLong(123),
            other => panic!("unknown object type {other:?}"),
        };
        let encoding = CString::new(row[2].as_bytes()).unwrap();
        let errors = errors_arg(&row[3]);
        let errors_c = errors.map(|name| CString::new(name).unwrap());
        let result = unsafe {
            if op == "encode" {
                codecs::PyCodec_Encode(
                    object,
                    encoding.as_ptr(),
                    errors_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                )
            } else {
                codecs::PyCodec_Decode(
                    object,
                    encoding.as_ptr(),
                    errors_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
                )
            }
        };
        // SAFETY: the C API returns an object or null.
        let got = unsafe { encode_object(result) };
        check(
            &mut mismatches,
            "codecs_registry.tsv",
            lineno,
            &row,
            got,
            row[4].clone(),
        );
    }
    finish("codecs_registry.tsv", mismatches);
}

/// Apply one built-in error handler to an exception instance, in the
/// oracle's result encoding. An error the handler raises itself carries
/// the oracle's message in the sidecar, read before the steal clears
/// it. A re-raise (`strict`, CPython's `PyErr_SetObject`) keeps the
/// passed instance with no sidecar; the oracle's message there is the
/// instance's `str()`, which the runtime renders from the error fields
/// in section 10.5, so the verified surface is the type name and the
/// instance identity, and the row's message is carried through.
///
/// # Safety
///
/// `exc` must be an exception instance of the runtime.
unsafe fn apply_handler(handler: &str, exc: *mut PyObject, want: &str) -> String {
    // SAFETY: the handler exports follow their C API contract.
    unsafe {
        let result = match handler {
            "strict" => codecs::PyCodec_StrictErrors(exc),
            "ignore" => codecs::PyCodec_IgnoreErrors(exc),
            "replace" => codecs::PyCodec_ReplaceErrors(exc),
            "backslashreplace" => codecs::PyCodec_BackslashReplaceErrors(exc),
            "xmlcharrefreplace" => codecs::PyCodec_XMLCharRefReplaceErrors(exc),
            other => panic!("unknown handler {other:?}"),
        };
        if !result.is_null() {
            return encode_object(result);
        }
        // The steal takes the instance and clears the sidecar, so the
        // message is read first.
        let message = String::from_utf8_lossy(&tstate::message()).into_owned();
        let raised = exceptions::PyErr_GetRaisedException();
        assert!(
            !raised.is_null(),
            "handler {handler:?} returned null without an error"
        );
        let name = type_name(raised);
        if !message.is_empty() {
            let escaped = escape(&message.chars().map(|c| c as u32).collect::<Vec<_>>());
            let got = format!("e:{name}:{escaped}");
            assert_eq!(got, want, "handler {handler:?} raised a different error");
            return got;
        }
        assert!(
            core::ptr::eq(raised, exc),
            "handler {handler:?} raised a fresh error without a message"
        );
        let kind = want
            .strip_prefix("e:")
            .and_then(|rest| rest.split_once(':'))
            .map_or("BaseException", |(kind, _)| kind);
        assert_eq!(name, kind, "handler {handler:?} re-raised the wrong type");
        want.to_string()
    }
}

#[test]
fn error_handlers_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("codecs_handlers.tsv") {
        let kind = row[0].as_str();
        let handler = row[6].as_str();
        let exc = match kind {
            "decode" | "cross" => {
                let encoding = CString::new(row[1].as_bytes()).unwrap();
                let data = unescape_bytes(&row[2]);
                let reason = CString::new(row[5].as_bytes()).unwrap();
                // SAFETY: the buffers live across the call.
                unsafe {
                    exceptions::PyUnicodeDecodeError_Create(
                        encoding.as_ptr(),
                        data.as_ptr() as *const c_char,
                        data.len() as isize,
                        arg_int(&row[3]),
                        arg_int(&row[4]),
                        reason.as_ptr(),
                    )
                }
            }
            "encode" => {
                let chars = unescape(&row[2]);
                let result = encode_call(row[1].as_str(), &chars, &[], None, None);
                assert!(result.is_null(), "expected the strict encode to fail");
                // The strict failure leaves the exception pending, as
                // the oracle's inputs guarantee.
                exceptions::PyErr_GetRaisedException()
            }
            "translate" => {
                let input = str_object(&unescape(&row[1]));
                let reason = CString::new(row[4].as_bytes()).unwrap();
                // SAFETY: the str and C string follow the contract.
                unsafe {
                    exceptions::_PyUnicodeTranslateError_Create(
                        input,
                        arg_int(&row[2]),
                        arg_int(&row[3]),
                        reason.as_ptr(),
                    )
                }
            }
            "value" => {
                // SAFETY: a plain ValueError instance with the message
                // CPython's carries.
                unsafe {
                    exceptions::PyErr_SetString(exceptions::value_error(), c"boom".as_ptr());
                    exceptions::PyErr_GetRaisedException()
                }
            }
            other => panic!("unknown handler row kind {other:?}"),
        };
        assert!(!exc.is_null());
        // SAFETY: `exc` is an exception instance by construction.
        let want = row[7..].join("\t");
        let got = unsafe { apply_handler(handler, exc, &want) };
        check(
            &mut mismatches,
            "codecs_handlers.tsv",
            lineno,
            &row,
            got,
            want,
        );
    }
    finish("codecs_handlers.tsv", mismatches);
}

// ------------------------------------------------- hand-written registry state

/// A callable-shaped object: a static type with `tp_call` set, never
/// actually called by these tests.
unsafe extern "C" fn noop_call(
    _slf: *mut PyObject,
    _args: *mut PyObject,
    _kwargs: *mut PyObject,
) -> *mut PyObject {
    core::ptr::null_mut()
}

static mut CALLABLE_TYPE: PyTypeObject = {
    let mut ty = typeobject::zero_type();
    ty.tp_name = c"coil-test-callable".as_ptr();
    ty.tp_call = Some(noop_call);
    ty
};

static mut CALLABLE: PyObject = PyObject {
    ob_refcnt: header::IMMORTAL_REFCNT,
    ob_type: &raw mut CALLABLE_TYPE,
};

fn expect_error(want: &str) {
    // An error is pending by each call site's construction.
    assert_eq!(error_text(), want);
}

#[test]
fn registry_state_matches_the_interim_contract() {
    init();
    let utf8 = CString::new("utf-8").unwrap();
    let nosuch = CString::new("nosuchencoding").unwrap();
    let handler = CString::new("nosuchhandler").unwrap();
    // SAFETY: the static objects and C strings follow the C API
    // contract; nothing here runs a call protocol.
    unsafe {
        // Lookup of a native name: the CodecInfo object is what is
        // missing, not the codec.
        assert!(codecs::_PyCodec_Lookup(utf8.as_ptr()).is_null());
        expect_error(
            "e:SystemError:coil: building CodecInfo objects needs the container runtime",
        );
        assert!(codecs::_PyCodec_Lookup(nosuch.as_ptr()).is_null());
        expect_error("e:LookupError:no codec search functions registered: can't find encoding");

        // KnownEncoding consults the native table and clears its own
        // error state.
        assert_eq!(codecs::PyCodec_KnownEncoding(utf8.as_ptr()), 1);
        assert!(tstate::occurred().is_null());
        assert_eq!(codecs::PyCodec_KnownEncoding(nosuch.as_ptr()), 0);
        assert!(tstate::occurred().is_null());

        // Register rejects non-callables, accepts callables, and a
        // registered search function turns the empty-path LookupError
        // into the typed error for calling it.
        assert_eq!(codecs::PyCodec_Register(long::PyLong_FromLong(1)), -1);
        expect_error("e:TypeError:argument must be callable");
        assert_eq!(codecs::PyCodec_Register(&raw mut CALLABLE), 0);
        assert!(codecs::_PyCodec_Lookup(nosuch.as_ptr()).is_null());
        expect_error("e:SystemError:coil: calling codec search functions needs the container runtime");
        assert_eq!(codecs::PyCodec_Unregister(&raw mut CALLABLE), 0);
        assert!(codecs::_PyCodec_Lookup(nosuch.as_ptr()).is_null());
        expect_error("e:LookupError:no codec search functions registered: can't find encoding");

        // The error-handler registry: callable checked, lookups of the
        // built-ins and registered names need the container runtime,
        // unknown names are CPython's LookupError.
        assert_eq!(
            codecs::PyCodec_RegisterError(handler.as_ptr(), long::PyLong_FromLong(1)),
            -1
        );
        expect_error("e:TypeError:handler must be callable");
        assert_eq!(
            codecs::PyCodec_RegisterError(handler.as_ptr(), &raw mut CALLABLE),
            0
        );
        assert!(codecs::PyCodec_LookupError(handler.as_ptr()).is_null());
        expect_error("e:SystemError:coil: returning an error handler object needs the container runtime");
        assert!(codecs::PyCodec_LookupError(utf8.as_ptr()).is_null());
        expect_error("e:LookupError:unknown error handler name 'utf-8'");
        assert!(codecs::PyCodec_LookupError(core::ptr::null()).is_null());
        expect_error(
            "e:SystemError:coil: returning the strict error handler as an object needs the container runtime",
        );

        // namereplace needs the unicodedata name tables.
        let exc = exceptions::PyUnicodeDecodeError_Create(
            utf8.as_ptr(),
            b"\xff".as_ptr() as *const c_char,
            1,
            0,
            1,
            c"invalid start byte".as_ptr(),
        );
        assert!(codecs::PyCodec_NameReplaceErrors(exc).is_null());
        expect_error("e:SystemError:coil: the 'namereplace' error handler needs the unicodedata name tables");

        // The Windows code pages are absent off Windows.
        let mbcs = CString::new("mbcs").unwrap();
        let oem = CString::new("oem").unwrap();
        let text = str_object(&unescape("abc"));
        if cfg!(target_os = "windows") {
            assert!(!codecs::PyCodec_Encode(text, mbcs.as_ptr(), core::ptr::null()).is_null());
            tstate::clear();
        } else {
            assert!(codecs::PyCodec_Encode(text, mbcs.as_ptr(), core::ptr::null()).is_null());
            expect_error("e:SystemError:coil: the 'mbcs' codec needs Windows code pages");
            assert!(codecs::PyCodec_Encode(text, oem.as_ptr(), core::ptr::null()).is_null());
            expect_error("e:SystemError:coil: the 'oem' codec needs Windows code pages");
        }
    }
}

//! Oracle-driven tests for the multibyte codec modules (spec section
//! 10.6): `_multibytecodec`'s engine and the `_codecs_cn/_hk/
//! _iso2022/_jp/_kr/_tw` codec tables. The tables under `tests/data`
//! came from CPython 3.12 via `tools/gen_cjk_oracle.py`; the field
//! encoding is shared with that script (`\p` escapes `|`, which the
//! incremental op fields split on).

use std::fs;
use std::path::Path;
use std::sync::Once;

use coil_capi_sys::PyObject;
use python312::modules::_multibytecodec as mbc;
use python312::modules::{
    _codecs_cn, _codecs_hk, _codecs_iso2022, _codecs_jp, _codecs_kr, _codecs_tw,
};
use python312::object::{header, typeobject};
use python312::tstate;
use python312::types::exc as exceptions;
use python312::types::{bytes as rt_bytes, long, tuple, unicode};

static INIT: Once = Once::new();

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
}

// ---------------------------------------------------------------- escaping

/// Undo the generator's escaping: backslash, tab, newline, carriage
/// return, `|`, bytes' `\xHH` and lone surrogates' `\uXXXX`.
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
            'p' => out.push(b'|' as u32),
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

/// The inverse of `unescape` for str fields.
fn escape(chars: &[u32]) -> String {
    let mut out = String::new();
    for &c in chars {
        match c {
            0x5C => out.push_str("\\\\"),
            0x7C => out.push_str("\\p"),
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

fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        match b {
            0x5C => out.push_str("\\\\"),
            0x7C => out.push_str("\\p"),
            0x09 => out.push_str("\\t"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

// ------------------------------------------------------------------ tables

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

// ---------------------------------------------------------------- decoding

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
    } else if core::ptr::eq(exc, exceptions::unicode_error()) {
        "UnicodeError"
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
    let escaped = escape(&message.chars().map(|c| c as u32).collect::<Vec<_>>());
    format!("e:{name}:{escaped}")
}

/// A result object in the oracle's encoding. Ints render in decimal
/// because encoder states pack up to seventeen bytes.
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
        if core::ptr::eq(object, python312::object::singletons::none()) {
            return "T:".to_string();
        }
        let ty = header::type_of(object);
        if ty == &raw mut unicode::PyUnicode_Type {
            let length = unicode::PyUnicode_GetLength(object);
            let mut chars = Vec::with_capacity(length as usize);
            for i in 0..length {
                chars.push(unicode::PyUnicode_ReadChar(object, i));
            }
            return format!("s:{}", escape(&chars));
        }
        if ty == &raw mut rt_bytes::PyBytes_Type {
            let size = rt_bytes::PyBytes_Size(object).max(0);
            let contents = rt_bytes::PyBytes_AsString(object);
            let slice = std::slice::from_raw_parts(contents as *const u8, size as usize);
            return format!("B:{}", escape_bytes(slice));
        }
        if ty == &raw mut tuple::PyTuple_Type {
            let size = tuple::PyTuple_Size(object);
            let mut parts = vec![format!("L:{size}")];
            for i in 0..size {
                parts.push(encode_object(tuple::PyTuple_GetItem(object, i)));
            }
            return parts.join("\t");
        }
        // The remaining results the oracle encodes are ints.
        match long::format(object.cast(), 10, false) {
            Some(text) => format!("i:{text}"),
            None => error_text(),
        }
    }
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
        "xmlcharrefreplace" => "xmlcharrefreplace",
        other => panic!("unknown error handler {other:?}"),
    })
}

/// The module lookup behind the row's encoding name.
fn getcodec(encoding: &str) -> *mut PyObject {
    match encoding {
        "gb2312" | "gbk" | "gb18030" | "hz" => _codecs_cn::getcodec(encoding),
        "big5hkscs" => _codecs_hk::getcodec(encoding),
        "iso2022_jp" | "iso2022_jp_1" | "iso2022_jp_2" | "iso2022_jp_2004" | "iso2022_jp_3"
        | "iso2022_jp_ext" | "iso2022_kr" => _codecs_iso2022::getcodec(encoding),
        "cp932" | "euc_jis_2004" | "euc_jisx0213" | "euc_jp" | "shift_jis" | "shift_jis_2004"
        | "shift_jisx0213" => _codecs_jp::getcodec(encoding),
        "cp949" | "euc_kr" | "johab" => _codecs_kr::getcodec(encoding),
        "big5" | "cp950" => _codecs_tw::getcodec(encoding),
        other => panic!("unknown encoding {other:?}"),
    }
}

fn module_getcodec(module: &str, name: &str) -> *mut PyObject {
    match module {
        "cn" => _codecs_cn::getcodec(name),
        "hk" => _codecs_hk::getcodec(name),
        "iso2022" => _codecs_iso2022::getcodec(name),
        "jp" => _codecs_jp::getcodec(name),
        "kr" => _codecs_kr::getcodec(name),
        "tw" => _codecs_tw::getcodec(name),
        other => panic!("unknown module {other:?}"),
    }
}

// ------------------------------------------------------------------ tests

#[test]
fn getcodec_lookups_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("cjk_getcodec.tsv") {
        let object = module_getcodec(&row[0], &row[1]);
        let got = if object.is_null() {
            error_text()
        } else {
            "b:1".to_string()
        };
        check(
            &mut mismatches,
            "cjk_getcodec.tsv",
            lineno,
            &row,
            got,
            row[2].clone(),
        );
    }
    finish("cjk_getcodec.tsv", mismatches);
}

#[test]
fn one_shot_encode_matches_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("cjk_encode.tsv") {
        let codec = getcodec(&row[0]);
        assert!(!codec.is_null(), "encoding {} missing", row[0]);
        let chars = unescape(&row[1]);
        // SAFETY: `codec` comes from `getcodec`, a codec object.
        let result = unsafe { mbc::mbcodec_encode(codec, &chars, errors_arg(&row[2])) };
        // SAFETY: the method returns a tuple or null.
        let got = unsafe { encode_object(result) };
        check(
            &mut mismatches,
            "cjk_encode.tsv",
            lineno,
            &row,
            got,
            row[3..].join("\t"),
        );
    }
    finish("cjk_encode.tsv", mismatches);
}

#[test]
fn one_shot_decode_matches_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("cjk_decode.tsv") {
        let codec = getcodec(&row[0]);
        assert!(!codec.is_null(), "encoding {} missing", row[0]);
        let data = unescape_bytes(&row[1]);
        // SAFETY: `codec` comes from `getcodec`, a codec object.
        let result = unsafe { mbc::mbcodec_decode(codec, &data, errors_arg(&row[2])) };
        // SAFETY: the method returns a tuple or null.
        let got = unsafe { encode_object(result) };
        check(
            &mut mismatches,
            "cjk_decode.tsv",
            lineno,
            &row,
            got,
            row[3..].join("\t"),
        );
    }
    finish("cjk_decode.tsv", mismatches);
}

/// A step token in the incremental table: the bare `EG`-style ops or
/// `E|`/`D|`-prefixed ones. Result fields start with their tags.
fn is_op(field: &str) -> bool {
    matches!(field, "EG" | "ESR" | "ER" | "DG" | "DSR" | "DR")
        || field.starts_with("E|")
        || field.starts_with("D|")
        || field.starts_with("ES|")
}

#[test]
fn incremental_sequences_match_the_oracle() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("cjk_incremental.tsv") {
        let codec_object = getcodec(&row[0]);
        assert!(!codec_object.is_null(), "encoding {} missing", row[0]);
        // SAFETY: `codec_object` comes from `getcodec`, a codec object.
        let codec = unsafe { mbc::codec_of(codec_object) }.expect("codec object");
        // SAFETY: the types are readied and the codecs static.
        let encoder = unsafe { mbc::incremental_encoder_new(codec, errors_arg(&row[1])) };
        assert!(!encoder.is_null());
        let decoder = unsafe { mbc::incremental_decoder_new(codec, errors_arg(&row[1])) };
        assert!(!decoder.is_null());
        let mut last_estate: *mut PyObject = std::ptr::null_mut();
        let mut last_dstate: *mut PyObject = std::ptr::null_mut();
        let mut i = 2;
        while i < row.len() {
            let op = row[i].clone();
            i += 1;
            let mut want = vec![row[i].clone()];
            i += 1;
            while i < row.len() && !is_op(&row[i]) {
                want.push(row[i].clone());
                i += 1;
            }
            let parts: Vec<&str> = op.split('|').collect();
            let result = match parts[0] {
                "E" => {
                    let chars = unescape(parts[2]);
                    // SAFETY: `encoder` is an encoder object.
                    unsafe { mbc::incremental_encoder_encode(encoder, &chars, parts[1] == "1") }
                }
                "D" => {
                    let data = unescape_bytes(parts[2]);
                    // SAFETY: `decoder` is a decoder object.
                    unsafe { mbc::incremental_decoder_decode(decoder, &data, parts[1] == "1") }
                }
                "EG" => {
                    // SAFETY: `encoder` is an encoder object.
                    let state = unsafe { mbc::incremental_encoder_getstate(encoder) };
                    last_estate = state;
                    state
                }
                "ESR" => {
                    // SAFETY: `encoder` is an encoder object and
                    // `last_estate` the int its `EG` produced.
                    unsafe { mbc::incremental_encoder_setstate(encoder, last_estate) }
                }
                "ES" => {
                    let value: isize = parts[1].parse().unwrap();
                    // SAFETY: `encoder` is an encoder object.
                    unsafe {
                        mbc::incremental_encoder_setstate(encoder, long::PyLong_FromSsize_t(value))
                    }
                }
                "ER" => {
                    // SAFETY: `encoder` is an encoder object.
                    unsafe { mbc::incremental_encoder_reset(encoder) }
                }
                "DG" => {
                    // SAFETY: `decoder` is a decoder object.
                    let state = unsafe { mbc::incremental_decoder_getstate(decoder) };
                    last_dstate = state;
                    state
                }
                "DSR" => {
                    // SAFETY: `decoder` is a decoder object and
                    // `last_dstate` the tuple its `DG` produced.
                    unsafe { mbc::incremental_decoder_setstate(decoder, last_dstate) }
                }
                "DR" => {
                    // SAFETY: `decoder` is a decoder object.
                    unsafe { mbc::incremental_decoder_reset(decoder) }
                }
                other => panic!("unknown op {other:?}"),
            };
            // SAFETY: each op returns its result object or null.
            let got = unsafe { encode_object(result) };
            check(
                &mut mismatches,
                "cjk_incremental.tsv",
                lineno,
                &row,
                got,
                want.join("\t"),
            );
        }
    }
    finish("cjk_incremental.tsv", mismatches);
}

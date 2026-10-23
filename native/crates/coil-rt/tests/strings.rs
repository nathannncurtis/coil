//! Oracle-driven tests for str, bytes and bytearray (spec sections 8.2
//! and 8.5). Every table under `tests/data` whose rows came from CPython
//! 3.12 via `tools/gen_str_oracle.py`; the field encoding is shared with
//! that script. The harness here is the method layer the runtime does not
//! have yet: it parses the row's arguments the way `str`/`bytes` methods
//! do, runs the runtime core or C API behind the method, and encodes the
//! result in the oracle's tags.

use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::sync::Once;

use core::ffi::{c_char, c_void};

use coil_capi_sys::{PyObject, PyTypeObject};
use python312::object::{exceptions, typeobject};
use python312::tstate;
use python312::types::{bytes as rt_bytes, float_, long, unicode};

static INIT: Once = Once::new();

const STR_TYPE: *mut PyTypeObject = &raw mut unicode::PyUnicode_Type;
const BYTES_TYPE: *mut PyTypeObject = &raw mut rt_bytes::PyBytes_Type;
const BYTEARRAY_TYPE: *mut PyTypeObject = &raw mut rt_bytes::PyByteArray_Type;

fn init() {
    INIT.call_once(|| assert_eq!(typeobject::init_static_types(), 0));
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
            'u' => {
                let mut value = 0u32;
                for _ in 0..4 {
                    value =
                        value * 16 + it.next().expect("short \\u").to_digit(16).expect("bad hex");
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

/// Hash tables carry a `# key <k0> <k1>` section per SipHash key; each
/// row is that key plus its fields.
fn hash_rows(name: &str) -> Vec<(usize, (u64, u64), Vec<String>)> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name);
    let text = fs::read_to_string(path).unwrap();
    let mut out = Vec::new();
    let mut key = (0u64, 0u64);
    for (i, line) in text.lines().enumerate() {
        if let Some(rest) = line.strip_prefix("# key ") {
            let mut it = rest.split(' ');
            key = (
                it.next().unwrap().parse().unwrap(),
                it.next().unwrap().parse().unwrap(),
            );
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        out.push((i + 1, key, line.split('\t').map(str::to_string).collect()));
    }
    out
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
    } else if core::ptr::eq(exc, exceptions::lookup_error()) {
        "LookupError"
    } else if core::ptr::eq(exc, exceptions::unicode_decode_error()) {
        "UnicodeDecodeError"
    } else if core::ptr::eq(exc, exceptions::unicode_encode_error()) {
        "UnicodeEncodeError"
    } else if core::ptr::eq(exc, exceptions::index_error()) {
        "IndexError"
    } else if core::ptr::eq(exc, exceptions::memory_error()) {
        "MemoryError"
    } else {
        "BaseException"
    };
    let message = String::from_utf8_lossy(&tstate::message()).into_owned();
    tstate::clear();
    // The oracle escapes error text like every other field.
    let escaped = escape(&message.chars().map(|c| c as u32).collect::<Vec<_>>());
    format!("e:{name}:{escaped}")
}

fn bool_tag(value: bool) -> String {
    if value { "b:1".into() } else { "b:0".into() }
}

fn int_tag(value: isize) -> String {
    format!("i:{value}")
}

/// A result str object in the shared encoding (read by codepoint).
///
/// # Safety
///
/// `obj` must be a str object or null.
unsafe fn encode_str_object(obj: *mut PyObject) -> String {
    // SAFETY: `obj` is a str object or null, per the caller contract.
    unsafe {
        if obj.is_null() {
            return error_text();
        }
        let length = unicode::PyUnicode_GetLength(obj);
        let mut chars = Vec::with_capacity(length as usize);
        for i in 0..length {
            chars.push(unicode::PyUnicode_ReadChar(obj, i));
        }
        format!("s:{}", escape(&chars))
    }
}

/// A result bytes object in the shared encoding.
///
/// # Safety
///
/// `obj` must be a bytes object or null.
unsafe fn encode_bytes_object(obj: *mut PyObject) -> String {
    // SAFETY: `obj` is a bytes object or null, per the caller contract.
    unsafe {
        if obj.is_null() {
            return error_text();
        }
        let size = rt_bytes::PyBytes_Size(obj);
        let data = rt_bytes::PyBytes_AsString(obj);
        let slice = std::slice::from_raw_parts(data as *const u8, size.max(0) as usize);
        format!("B:{}", escape_bytes(slice))
    }
}

/// A method argument: empty means None, `b:` a bool, `i:` an int, plain
/// text a str (or bytes on the bytes side).
fn arg_opt(field: &str) -> Option<&str> {
    if field.is_empty() { None } else { Some(field) }
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

fn arg_chars(field: &str) -> Vec<u32> {
    unescape(field)
}

/// Build a str object from scalar characters (the inputs never carry
/// lone surrogates).
fn str_object(chars: &[u32]) -> *mut PyObject {
    let text: String = chars
        .iter()
        .map(|&c| char::from_u32(c).expect("input is scalar"))
        .collect();
    let size = text.len() as isize;
    // SAFETY: `text` lives across the call and the size is its byte length.
    unsafe { unicode::PyUnicode_FromStringAndSize(text.as_ptr() as *const c_char, size) }
}

fn bytes_object(bytes: &[u8]) -> *mut PyObject {
    let size = bytes.len() as isize;
    // SAFETY: `bytes` lives across the call; NULL is passed for
    // empty so the shared empty object comes back.
    unsafe {
        rt_bytes::PyBytes_FromStringAndSize(
            if bytes.is_empty() {
                std::ptr::null()
            } else {
                bytes.as_ptr() as *const c_char
            },
            size,
        )
    }
}

fn c_string(text: &str) -> CString {
    CString::new(text.as_bytes().to_vec()).expect("argument has interior NUL")
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

#[test]
fn type_layouts_match_the_abi() {
    use coil_rt_abi::cp312;
    assert_eq!(
        std::mem::offset_of!(rt_bytes::PyBytesObject, ob_shash),
        cp312::bytes::OB_SHASH
    );
    assert_eq!(
        std::mem::offset_of!(rt_bytes::PyBytesObject, ob_sval),
        cp312::bytes::OB_SVAL
    );
    assert_eq!(
        std::mem::size_of::<rt_bytes::PyBytesObject>(),
        cp312::bytes::SIZEOF
    );

    assert_eq!(
        std::mem::offset_of!(rt_bytes::PyByteArrayObject, ob_alloc),
        cp312::bytearray::OB_ALLOC
    );
    assert_eq!(
        std::mem::offset_of!(rt_bytes::PyByteArrayObject, ob_bytes),
        cp312::bytearray::OB_BYTES
    );
    assert_eq!(
        std::mem::offset_of!(rt_bytes::PyByteArrayObject, ob_start),
        cp312::bytearray::OB_START
    );
    assert_eq!(
        std::mem::offset_of!(rt_bytes::PyByteArrayObject, ob_exports),
        cp312::bytearray::OB_EXPORTS
    );
    assert_eq!(
        std::mem::size_of::<rt_bytes::PyByteArrayObject>(),
        cp312::bytearray::SIZEOF
    );

    // `PyUnicodeObject` is the union of all string layouts; CPython's type
    // advertises its full size even though every runtime string is compact.
    assert_eq!(
        std::mem::offset_of!(unicode::PyUnicodeObject, data),
        cp312::unicode::DATA
    );
    assert_eq!(
        std::mem::size_of::<unicode::PyUnicodeObject>(),
        cp312::unicode::SIZEOF
    );
    assert_eq!(
        std::mem::offset_of!(unicode::PyCompactUnicodeObject, utf8),
        cp312::compact_unicode::UTF8
    );
    assert_eq!(
        std::mem::size_of::<unicode::PyASCIIObject>(),
        cp312::ascii::SIZEOF
    );

    init();
    assert_eq!(unsafe { (*STR_TYPE).tp_basicsize }, 64);
    assert_eq!(unsafe { (*STR_TYPE).tp_itemsize }, 0);
    assert_eq!(unsafe { (*BYTES_TYPE).tp_basicsize }, 33);
    assert_eq!(unsafe { (*BYTES_TYPE).tp_itemsize }, 1);
    assert_eq!(unsafe { (*BYTEARRAY_TYPE).tp_basicsize }, 56);
    assert_eq!(unsafe { (*BYTEARRAY_TYPE).tp_itemsize }, 0);
}

#[test]
fn compact_string_layouts_use_the_pinned_offsets() {
    init();
    // SAFETY: every pointer below is a fresh object of the named type.
    unsafe {
        let ascii_obj = unicode::PyUnicode_FromStringAndSize(b"hello".as_ptr() as *const c_char, 5);
        assert!(!ascii_obj.is_null());
        // Kind 1, compact, ASCII: the data is inline right after the header.
        let state = (*ascii_obj.cast::<unicode::PyASCIIObject>()).state;
        assert_eq!(state.kind(), unicode::PY_UNICODE_1BYTE_KIND);
        assert!(state.compact());
        assert!(state.ascii());
        assert_eq!((*ascii_obj.cast::<unicode::PyASCIIObject>()).length, 5);
        let data_addr = (ascii_obj as usize + coil_rt_abi::cp312::ascii::SIZEOF) as *const u8;
        assert_eq!(*data_addr, b'h');
        assert_eq!(
            *((ascii_obj as usize + coil_rt_abi::cp312::ascii::SIZEOF + 4) as *const u8),
            b'o'
        );
        let hash_slot = (ascii_obj as usize + coil_rt_abi::cp312::ascii::HASH) as *const isize;
        assert_eq!(*hash_slot, -1, "hash starts uncached");
        // Asking for UTF-8 fills the cache (which for kind-1 objects lives
        // in the same bytes as the string data itself).
        let utf8 = unicode::PyUnicode_AsUTF8AndSize(ascii_obj, std::ptr::null_mut());
        assert!(!utf8.is_null());
        assert_eq!(std::ffi::CStr::from_ptr(utf8).to_bytes(), b"hello");

        // A 2-byte-kind string (U+0152 exceeds the 1-byte kinds).
        let wide = unicode::PyUnicode_FromStringAndSize("\u{152}".as_ptr() as *const c_char, 2);
        assert_eq!(
            (*wide.cast::<unicode::PyASCIIObject>()).state.kind(),
            unicode::PY_UNICODE_2BYTE_KIND
        );
        assert!(!(*wide.cast::<unicode::PyASCIIObject>()).state.ascii());
        let utf8_len_slot =
            (wide as usize + coil_rt_abi::cp312::compact_unicode::UTF8_LENGTH) as *const usize;
        assert_eq!(*utf8_len_slot, 0, "the UTF-8 cache is lazy");
        let raw = (wide as usize + coil_rt_abi::cp312::compact_unicode::SIZEOF) as *const u16;
        assert_eq!(*raw, 0x152, "2-byte data is UTF-16-LE inline");

        // A 4-byte-kind string (U+1F600 exceeds UTF-16).
        let astral = unicode::PyUnicode_FromStringAndSize("\u{1f600}".as_ptr() as *const c_char, 4);
        assert_eq!(
            (*astral.cast::<unicode::PyASCIIObject>()).state.kind(),
            unicode::PY_UNICODE_4BYTE_KIND
        );
        let raw = (astral as usize + coil_rt_abi::cp312::compact_unicode::SIZEOF) as *const u32;
        assert_eq!(*raw, 0x1F600, "4-byte data is UTF-32-LE inline");

        // bytes: ob_shash is uncached (-1), ob_sval[0] is the terminator.
        let b = bytes_object(b"xyz");
        let shash = (b as usize + coil_rt_abi::cp312::bytes::OB_SHASH) as *const isize;
        assert_eq!(*shash, -1);
        let sval = (b as usize + coil_rt_abi::cp312::bytes::OB_SVAL) as *const u8;
        assert_eq!(*sval, b'x');
        let empty = bytes_object(b"");
        let empty_sval = (empty as usize + coil_rt_abi::cp312::bytes::OB_SVAL) as *const u8;
        assert_eq!(*empty_sval, 0, "empty bytes still terminates");
    }
}

/// How many leading columns after the method name are arguments: rows
/// are `[method][args...][input][result...]`, and the result's own
/// arity (list results carry their length) pins it. Exactly one split
/// point fits every row.
fn solve_arg_count(row: &[String], max_args: usize) -> usize {
    let mut found = None;
    for nargs in 0..=max_args {
        let result_start = 2 + nargs;
        if result_start >= row.len() {
            break;
        }
        let result_count = match row[result_start].strip_prefix("L:") {
            Some(n) => 1 + n.parse::<usize>().expect("bad list arity"),
            None => 1,
        };
        if result_start + result_count == row.len() {
            assert!(found.is_none(), "ambiguous argument count in {row:?}");
            found = Some(nargs);
        }
    }
    found.unwrap_or_else(|| panic!("no argument count fits {row:?}"))
}

/// The largest argument tuple the CPython method wrapper for `method`
/// accepts on the str side; it bounds the row's argument-count solve.
fn str_method_max_args(method: &str) -> usize {
    match method {
        "count" | "find" | "rfind" | "replace" | "startswith" | "endswith" => 3,
        "center" | "ljust" | "rjust" | "split" | "rsplit" => 2,
        "strip" | "lstrip" | "rstrip" | "removeprefix" | "removesuffix" | "partition"
        | "rpartition" | "expandtabs" | "zfill" | "splitlines" => 1,
        _ => 0,
    }
}

fn list_tag(pieces: Vec<String>) -> String {
    if pieces.is_empty() {
        return "L:0".into();
    }
    format!("L:{}\t{}", pieces.len(), pieces.join("\t"))
}

#[test]
fn str_methods_match_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("str_methods.tsv") {
        tstate::clear();
        let method = row[0].clone();
        let nargs = solve_arg_count(&row, str_method_max_args(&method));
        let args = &row[1..1 + nargs];
        let input = &row[1 + nargs].clone();
        let got = str_method_case(&method, args, input);
        let want = row[2 + nargs..].join("\t");
        check(&mut mismatches, "str_methods.tsv", lineno, &row, got, want);
    }
    finish("str_methods.tsv", mismatches);
}

/// The method layer: parse the row's arguments like the `str` method
/// wrappers would, run the runtime entry point behind the method, and
/// encode the result or error.
fn str_method_case(method: &str, args: &[String], input: &str) -> String {
    let chars = unescape(input);
    let obj = str_object(&chars);
    assert!(!obj.is_null(), "input str build failed for {method}");
    // SAFETY: every call below receives the fresh input object and
    // fresh argument objects built in this function.
    unsafe {
        match method {
            "upper" | "lower" => {
                let result = if method == "upper" {
                    unicode::PyUnicode_Upper(obj)
                } else {
                    unicode::PyUnicode_Lower(obj)
                };
                encode_str_object(result)
            }
            "casefold" | "title" | "capitalize" | "swapcase" => {
                let result = match method {
                    "casefold" => unicode::casefold_core(&chars),
                    "title" => unicode::title_core(&chars),
                    "capitalize" => unicode::capitalize_core(&chars),
                    _ => unicode::swapcase_core(&chars),
                };
                format!("s:{}", escape(&result))
            }
            "strip" | "lstrip" | "rstrip" => {
                let kind = match method {
                    "lstrip" => unicode::StripKind::Left,
                    "rstrip" => unicode::StripKind::Right,
                    _ => unicode::StripKind::Both,
                };
                let result = match arg_opt(&args[0]) {
                    None => unicode::strip_core(&chars, kind),
                    Some(sep_text) => {
                        let sep = str_object(&unescape(sep_text));
                        let out = unicode::_PyUnicode_XStrip(
                            obj,
                            match kind {
                                unicode::StripKind::Left => -1,
                                unicode::StripKind::Right => 1,
                                unicode::StripKind::Both => 0,
                            },
                            sep,
                        );
                        if out.is_null() {
                            return error_text();
                        }
                        return encode_str_object(out);
                    }
                };
                format!("s:{}", escape(&result))
            }
            "center" | "ljust" | "rjust" | "zfill" => {
                let width = arg_int(&args[0]);
                let fill = if args.len() > 1 {
                    let fill = arg_chars(&args[1]);
                    if fill.len() != 1 {
                        tstate::set_string(
                            exceptions::type_error(),
                            b"The fill character must be exactly one character long",
                        );
                        return error_text();
                    }
                    fill[0]
                } else {
                    ' ' as u32
                };
                let result = match method {
                    "center" => unicode::center_core(&chars, width, fill),
                    "ljust" => unicode::ljust_core(&chars, width, fill),
                    "rjust" => unicode::rjust_core(&chars, width, fill),
                    _ => unicode::zfill_core(&chars, width),
                };
                format!("s:{}", escape(&result))
            }
            "expandtabs" => {
                let tabsize = if !args.is_empty() {
                    arg_int(&args[0])
                } else {
                    8
                };
                format!("s:{}", escape(&unicode::expandtabs_core(&chars, tabsize)))
            }
            "count" | "find" | "rfind" => {
                let sub = str_object(&arg_chars(&args[0]));
                let start = arg_int(&args[1]);
                let end = arg_int(&args[2]);
                if method == "count" {
                    int_tag(unicode::PyUnicode_Count(obj, sub, start, end))
                } else {
                    int_tag(unicode::PyUnicode_Find(
                        obj,
                        sub,
                        start,
                        end,
                        if method == "rfind" { -1 } else { 1 },
                    ))
                }
            }
            "startswith" | "endswith" => {
                let prefix = str_object(&arg_chars(&args[0]));
                let start = arg_int(&args[1]);
                let end = arg_int(&args[2]);
                let matched = unicode::PyUnicode_Tailmatch(
                    obj,
                    prefix,
                    start,
                    end,
                    if method == "endswith" { -1 } else { 1 },
                );
                if matched < 0 {
                    return error_text();
                }
                bool_tag(matched != 0)
            }
            "replace" => {
                let sub = str_object(&arg_chars(&args[0]));
                let repl = str_object(&arg_chars(&args[1]));
                let count = arg_int(&args[2]);
                encode_str_object(unicode::PyUnicode_Replace(obj, sub, repl, count))
            }
            "removeprefix" | "removesuffix" => {
                let affix = str_object(&arg_chars(&args[0]));
                let hay = unicode::as_chars(obj).expect("input is a str");
                let needle = unicode::as_chars(affix).expect("affix is a str");
                let at_start =
                    hay.len() >= needle.len() && hay.slice(0, needle.len()) == needle.as_vec();
                let at_end = hay.len() >= needle.len()
                    && hay.slice(hay.len() - needle.len(), hay.len()) == needle.as_vec();
                let stripped = if method == "removeprefix" {
                    at_start
                } else {
                    at_end
                };
                if stripped {
                    let start = if method == "removeprefix" {
                        needle.len()
                    } else {
                        0
                    };
                    let end = if method == "removeprefix" {
                        hay.len()
                    } else {
                        hay.len() - needle.len()
                    };
                    encode_str_object(unicode::PyUnicode_Substring(obj, start, end))
                } else {
                    format!("s:{}", escape(&chars))
                }
            }
            "isidentifier" => bool_tag(unicode::PyUnicode_IsIdentifier(obj) != 0),
            "isalnum" | "isalpha" | "isascii" | "isdecimal" | "isdigit" | "islower"
            | "isnumeric" | "isprintable" | "isspace" | "istitle" | "isupper" => {
                let hay = unicode::as_chars(obj).expect("input is a str");
                // The string-level rules are the method wrappers':
                // "at least one character" where CPython demands it, and
                // istitle's alternating cased/uncased walk.
                let mut cased = false;
                let mut all = true;
                let mut previous_cased = false;
                for i in 0..hay.len() {
                    let ch = hay.get(i);
                    match method {
                        "isalnum" => all &= unicode::is_alpha(ch) || unicode::is_numeric_char(ch),
                        "isalpha" => all &= unicode::is_alpha(ch),
                        "isascii" => all &= ch < 0x80,
                        "isdecimal" => all &= unicode::is_decimal_digit(ch),
                        "isdigit" => all &= unicode::is_digit(ch),
                        "isnumeric" => all &= unicode::is_numeric_char(ch),
                        "isprintable" => all &= unicode::is_printable(ch),
                        "isspace" => all &= unicode::_PyUnicode_IsWhitespace(ch) != 0,
                        "islower" | "isupper" => {
                            let (yes, no) = if method == "islower" {
                                (unicode::is_lowercase(ch), unicode::is_uppercase(ch))
                            } else {
                                (unicode::is_uppercase(ch), unicode::is_lowercase(ch))
                            };
                            if yes {
                                cased = true;
                            } else if no || unicode::is_titlecase(ch) {
                                all = false;
                            }
                        }
                        _ => {
                            // istitle: an uppercase/title character must
                            // not follow a cased character, a lowercase
                            // one must; uncased characters close the run
                            // (unicodeobject.c's istitle_unicode walk).
                            if unicode::is_uppercase(ch) || unicode::is_titlecase(ch) {
                                if previous_cased {
                                    all = false;
                                }
                                previous_cased = true;
                                cased = true;
                            } else if unicode::is_lowercase(ch) {
                                if !previous_cased {
                                    all = false;
                                }
                                cased = true;
                            } else {
                                previous_cased = false;
                            }
                        }
                    }
                }
                if method == "istitle" {
                    bool_tag(!hay.is_empty() && all && cased)
                } else if method == "islower" || method == "isupper" {
                    bool_tag(all && cased)
                } else if method == "isprintable" || method == "isascii" {
                    bool_tag(all)
                } else {
                    bool_tag(!hay.is_empty() && all)
                }
            }
            "split" | "rsplit" => {
                let sep = arg_opt(&args[0]).map(unescape);
                let maxsplit = arg_int(&args[1]);
                let hay = unicode::chars_from_owned(&chars);
                let sep_chars = sep.as_deref().map(unicode::chars_from_owned);
                let pieces = unicode::split_chars(hay, sep_chars, maxsplit, method == "rsplit");
                match pieces {
                    None => {
                        tstate::set_string(exceptions::value_error(), b"empty separator");
                        error_text()
                    }
                    Some(pieces) => list_tag(pieces.iter().map(|p| escape(p)).collect()),
                }
            }
            "splitlines" => {
                let keepends = arg_bool(&args[0]);
                let hay = unicode::chars_from_owned(&chars);
                list_tag(
                    unicode::splitlines(hay, keepends)
                        .iter()
                        .map(|p| escape(p))
                        .collect(),
                )
            }
            "partition" | "rpartition" => {
                let sep = arg_chars(&args[0]);
                let (before, mid, after) =
                    unicode::partition_pieces(&chars, &sep, method == "rpartition");
                list_tag(vec![escape(&before), escape(&mid), escape(&after)])
            }
            "join_pair" => {
                let pieces = [obj, obj];
                encode_str_object(unicode::_PyUnicode_JoinArray(
                    obj,
                    pieces.as_ptr(),
                    pieces.len() as isize,
                ))
            }
            other => panic!("no dispatch for str method {other}"),
        }
    }
}

/// The largest argument tuple the bytes-side wrapper accepts.
fn bytes_method_max_args(method: &str) -> usize {
    match method {
        "count"
        | "find"
        | "rfind"
        | "replace"
        | "startswith"
        | "endswith"
        | "translate_maketrans" => 3,
        "center" | "ljust" | "rjust" | "split" | "rsplit" | "translate" | "decode" | "hex" => 2,
        "strip" | "lstrip" | "rstrip" | "removeprefix" | "removesuffix" | "partition"
        | "rpartition" | "expandtabs" | "zfill" | "splitlines" | "fromhex" => 1,
        _ => 0,
    }
}

#[test]
fn bytes_methods_match_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("bytes_methods.tsv") {
        tstate::clear();
        let method = row[0].clone();
        let nargs = solve_arg_count(&row, bytes_method_max_args(&method));
        let args = &row[1..1 + nargs];
        let input = &row[1 + nargs].clone();
        let got = bytes_method_case(&method, args, input);
        let want = row[2 + nargs..].join("\t");
        check(
            &mut mismatches,
            "bytes_methods.tsv",
            lineno,
            &row,
            got,
            want,
        );
    }
    finish("bytes_methods.tsv", mismatches);
}

/// The bytes method layer over the runtime cores and C API.
fn bytes_method_case(method: &str, args: &[String], input: &str) -> String {
    let data = unescape_bytes(input);
    // SAFETY: fresh object for the row's input; every call below only
    // touches it and fresh argument objects.
    unsafe {
        if !matches!(
            method,
            "upper"
                | "lower"
                | "capitalize"
                | "title"
                | "swapcase"
                | "strip"
                | "lstrip"
                | "rstrip"
                | "center"
                | "ljust"
                | "rjust"
                | "zfill"
                | "expandtabs"
                | "count"
                | "find"
                | "rfind"
                | "startswith"
                | "endswith"
                | "replace"
                | "removeprefix"
                | "removesuffix"
                | "isalnum"
                | "isalpha"
                | "isascii"
                | "isdigit"
                | "islower"
                | "isspace"
                | "istitle"
                | "isupper"
                | "split"
                | "rsplit"
                | "splitlines"
                | "partition"
                | "rpartition"
                | "fromhex"
                | "hex"
                | "translate"
                | "translate_maketrans"
                | "decode"
        ) {
            panic!("no dispatch for bytes method {method}");
        }
        match method {
            "upper" | "lower" => {
                format!(
                    "B:{}",
                    escape_bytes(&rt_bytes::to_upper_or_lower(&data, method == "upper"))
                )
            }
            "capitalize" => format!("B:{}", escape_bytes(&rt_bytes::capitalize(&data))),
            "title" => format!("B:{}", escape_bytes(&rt_bytes::title(&data))),
            "swapcase" => format!("B:{}", escape_bytes(&rt_bytes::swapcase(&data))),
            "strip" | "lstrip" | "rstrip" => {
                let kind = match method {
                    "lstrip" => rt_bytes::StripKind::Left,
                    "rstrip" => rt_bytes::StripKind::Right,
                    _ => rt_bytes::StripKind::Both,
                };
                let sep = unescape_bytes(&args[0]);
                format!("B:{}", escape_bytes(&rt_bytes::strip(&data, &sep, kind)))
            }
            "center" | "ljust" | "rjust" => {
                let width = arg_int(&args[0]);
                let kind = match method {
                    "ljust" => rt_bytes::PadKind::Right,
                    "rjust" => rt_bytes::PadKind::Left,
                    _ => rt_bytes::PadKind::Center,
                };
                let fill = if args.len() > 1 {
                    let fill = unescape_bytes(&args[1]);
                    if fill.len() != 1 {
                        tstate::set_string(
                            exceptions::type_error(),
                            format!(
                                "{method}() argument 2 must be a byte string of length 1, not bytes"
                            )
                            .as_bytes(),
                        );
                        return error_text();
                    }
                    fill[0]
                } else {
                    b' '
                };
                format!(
                    "B:{}",
                    escape_bytes(&rt_bytes::pad(&data, width, fill, kind))
                )
            }
            "zfill" => format!(
                "B:{}",
                escape_bytes(&rt_bytes::zfill(&data, arg_int(&args[0])))
            ),
            "expandtabs" => {
                let tabsize = if !args.is_empty() {
                    arg_int(&args[0])
                } else {
                    8
                };
                format!("B:{}", escape_bytes(&rt_bytes::expandtabs(&data, tabsize)))
            }
            "count" => int_tag(rt_bytes::count(
                &data,
                &unescape_bytes(&args[0]),
                arg_int(&args[1]),
                arg_int(&args[2]),
            )),
            "find" | "rfind" => {
                let needle = unescape_bytes(&args[0]);
                let start = arg_int(&args[1]);
                let end = arg_int(&args[2]);
                int_tag(if method == "rfind" {
                    rt_bytes::rfind(&data, &needle, start, end)
                } else {
                    rt_bytes::find(&data, &needle, start, end)
                })
            }
            "startswith" | "endswith" => bool_tag(rt_bytes::tailmatch(
                &data,
                &unescape_bytes(&args[0]),
                arg_int(&args[1]),
                arg_int(&args[2]),
                method == "startswith",
            )),
            "replace" => format!(
                "B:{}",
                escape_bytes(&rt_bytes::replace(
                    &data,
                    &unescape_bytes(&args[0]),
                    &unescape_bytes(&args[1]),
                    arg_int(&args[2]),
                ))
            ),
            "removeprefix" | "removesuffix" => format!(
                "B:{}",
                escape_bytes(&rt_bytes::remove_affix(
                    &data,
                    &unescape_bytes(&args[0]),
                    method == "removeprefix",
                ))
            ),
            "isalnum" => bool_tag(rt_bytes::is_alnum(&data)),
            "isalpha" => bool_tag(rt_bytes::is_alpha(&data)),
            "isascii" => bool_tag(rt_bytes::is_ascii(&data)),
            "isdigit" => bool_tag(rt_bytes::is_digit(&data)),
            "islower" => bool_tag(rt_bytes::is_lower(&data)),
            "isspace" => bool_tag(rt_bytes::is_space(&data)),
            "istitle" => bool_tag(rt_bytes::is_title(&data)),
            "isupper" => bool_tag(rt_bytes::is_upper(&data)),
            "split" | "rsplit" => {
                let sep = arg_opt(&args[0]).map(unescape_bytes);
                let maxsplit = arg_int(&args[1]);
                let pieces = rt_bytes::split(&data, sep.as_deref(), maxsplit, method == "rsplit");
                list_tag(pieces.iter().map(|p| escape_bytes(p)).collect())
            }
            "splitlines" => list_tag(
                rt_bytes::splitlines(&data, arg_bool(&args[0]))
                    .iter()
                    .map(|p| escape_bytes(p))
                    .collect(),
            ),
            "partition" | "rpartition" => {
                match rt_bytes::partition(&data, &unescape_bytes(&args[0]), method == "rpartition")
                {
                    None => {
                        tstate::set_string(exceptions::value_error(), b"empty separator");
                        error_text()
                    }
                    Some((before, mid, after)) => list_tag(vec![
                        escape_bytes(&before),
                        escape_bytes(&mid),
                        escape_bytes(&after),
                    ]),
                }
            }
            "fromhex" => {
                let text = unescape(&args[0]);
                match rt_bytes::from_hex(&text) {
                    None => error_text(),
                    Some(bytes) => format!("B:{}", escape_bytes(&bytes)),
                }
            }
            "hex" => {
                // The bare hex() rows leave both argument columns empty.
                let hex_text = if args[1].is_empty() {
                    rt_bytes::hex(&data, b"", 0)
                } else {
                    let sep = unescape_bytes(&args[0]);
                    if sep.len() != 1 {
                        tstate::set_string(exceptions::value_error(), b"sep must be length 1.");
                        return error_text();
                    }
                    rt_bytes::hex(&data, &sep, arg_int(&args[1]))
                };
                format!(
                    "s:{}",
                    escape(&hex_text.iter().map(|&b| b as u32).collect::<Vec<_>>())
                )
            }
            "translate" => {
                let table = unescape_bytes(&args[0]);
                let delete = unescape_bytes(&args[1]);
                if table.len() != 256 {
                    tstate::set_string(
                        exceptions::value_error(),
                        b"translation table must be 256 characters long",
                    );
                    return error_text();
                }
                format!(
                    "B:{}",
                    escape_bytes(&rt_bytes::translate(&data, Some(&table), &delete))
                )
            }
            "translate_maketrans" => {
                let frm = unescape_bytes(&args[0]);
                let to = unescape_bytes(&args[1]);
                let delete = unescape_bytes(&args[2]);
                match rt_bytes::maketrans(&frm, &to, b"") {
                    None => {
                        tstate::set_string(
                            exceptions::value_error(),
                            b"maketrans arguments must have same length",
                        );
                        error_text()
                    }
                    Some(table) => format!(
                        "B:{}",
                        escape_bytes(&rt_bytes::translate(&data, Some(&table), &delete))
                    ),
                }
            }
            "decode" => {
                let obj = bytes_object(&data);
                let encoding = c_string(&args[0]);
                let errors = match arg_opt(&args[1]) {
                    None => std::ptr::null(),
                    Some(text) => c_string(text).into_raw(),
                };
                let result = unicode::PyUnicode_FromEncodedObject(obj, encoding.as_ptr(), errors);
                if !errors.is_null() {
                    drop(CString::from_raw(errors.cast_mut()));
                }
                encode_str_object(result)
            }
            other => panic!("no dispatch for bytes method {other}"),
        }
    }
}

#[test]
fn str_decode_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("str_decode.tsv") {
        tstate::clear();
        let data = unescape_bytes(&row[0]);
        let obj = bytes_object(&data);
        let encoding = c_string(&row[1]);
        let errors = match arg_opt(&row[2]) {
            None => std::ptr::null(),
            Some(text) => c_string(text).into_raw(),
        };
        // SAFETY: fresh objects and C strings for the call.
        let got = unsafe {
            let result = unicode::PyUnicode_FromEncodedObject(obj, encoding.as_ptr(), errors);
            if !errors.is_null() {
                drop(CString::from_raw(errors.cast_mut()));
            }
            encode_str_object(result)
        };
        let want = row[3].clone();
        check(&mut mismatches, "str_decode.tsv", lineno, &row, got, want);
    }
    finish("str_decode.tsv", mismatches);
}

#[test]
fn str_encode_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("str_encode.tsv") {
        tstate::clear();
        let obj = str_object(&unescape(&row[0]));
        let encoding = c_string(&row[1]);
        let errors = match arg_opt(&row[2]) {
            None => std::ptr::null(),
            Some(text) => c_string(text).into_raw(),
        };
        // SAFETY: fresh objects and C strings for the call.
        let got = unsafe {
            let result = unicode::PyUnicode_AsEncodedString(obj, encoding.as_ptr(), errors);
            if !errors.is_null() {
                drop(CString::from_raw(errors.cast_mut()));
            }
            encode_bytes_object(result)
        };
        let want = row[3].clone();
        check(&mut mismatches, "str_encode.tsv", lineno, &row, got, want);
    }
    finish("str_encode.tsv", mismatches);
}

/// One `%`-formatting argument, in the generator's `fmt_arg` encoding.
fn format_arg_object(field: &str) -> *mut PyObject {
    if field.is_empty() {
        return python312::object::singletons::none();
    }
    if let Some(rest) = field.strip_prefix("i:") {
        return long::PyLong_FromLongLong(rest.parse().unwrap());
    }
    if let Some(rest) = field.strip_prefix("f:") {
        let bits = u64::from_str_radix(rest, 16).unwrap();
        return float_::PyFloat_FromDouble(f64::from_bits(bits));
    }
    if let Some(rest) = field.strip_prefix("B:") {
        return bytes_object(&unescape_bytes(rest));
    }
    if let Some(rest) = field.strip_prefix("s:") {
        return str_object(&unescape(rest));
    }
    panic!("unformatted argument field {field:?}");
}

#[test]
fn str_percent_format_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("str_format.tsv") {
        tstate::clear();
        let fmt = str_object(&unescape(&row[0]));
        // SAFETY: fresh objects for the call.
        let got = unsafe {
            let args = format_arg_object(&row[1]);
            encode_str_object(unicode::PyUnicode_Format(fmt, args))
        };
        let want = row[2].clone();
        check(&mut mismatches, "str_format.tsv", lineno, &row, got, want);
    }
    finish("str_format.tsv", mismatches);
}

#[test]
fn bytes_percent_format_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("bytes_format.tsv") {
        tstate::clear();
        let fmt = unescape_bytes(&row[0]);
        // SAFETY: fresh objects for the call.
        let got = unsafe {
            let args = format_arg_object(&row[1]);
            let result = rt_bytes::_PyBytes_FormatEx(
                fmt.as_ptr() as *const c_char,
                fmt.len() as isize,
                args,
                0,
            );
            encode_bytes_object(result)
        };
        let want = row[2].clone();
        check(&mut mismatches, "bytes_format.tsv", lineno, &row, got, want);
    }
    finish("bytes_format.tsv", mismatches);
}

#[test]
fn format_spec_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("str_format_spec.tsv") {
        tstate::clear();
        let value = unescape(&row[0]);
        let got = match unicode::format_advanced(&value, &row[1]) {
            // `format_advanced` always sets the pending error on None.
            None => error_text(),
            Some(result) => format!("s:{}", escape(&result)),
        };
        let want = row[2].clone();
        check(
            &mut mismatches,
            "str_format_spec.tsv",
            lineno,
            &row,
            got,
            want,
        );
    }
    finish("str_format_spec.tsv", mismatches);
}

#[test]
fn repr_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    for (lineno, row) in rows("str_repr.tsv") {
        tstate::clear();
        // SAFETY: fresh objects for the call.
        let got =
            unsafe { encode_str_object(unicode::PyObject_Repr(str_object(&unescape(&row[0])))) };
        check(
            &mut mismatches,
            "str_repr.tsv",
            lineno,
            &row,
            got,
            row[1].clone(),
        );
    }
    for (lineno, row) in rows("bytes_repr.tsv") {
        tstate::clear();
        let (obj, want) = if let Some(rest) = row[0].strip_prefix("B:") {
            // The one bytearray row.
            let data = unescape_bytes(rest);
            (
                // SAFETY: fresh bytearray for the row.
                unsafe {
                    rt_bytes::PyByteArray_FromStringAndSize(
                        data.as_ptr() as *const c_char,
                        data.len() as isize,
                    )
                },
                row[1].clone(),
            )
        } else {
            (bytes_object(&unescape_bytes(&row[0])), row[1].clone())
        };
        assert!(!obj.is_null());
        // SAFETY: the object was just built.
        let got = unsafe { encode_str_object(unicode::PyObject_Repr(obj)) };
        check(&mut mismatches, "bytes_repr.tsv", lineno, &row, got, want);
    }
    finish("repr tables", mismatches);
}

/// The hash tables: one `# key <k0> <k1>` section per SipHash key, each
/// row hashing through the type's `tp_hash` with that key installed.
fn check_hash_table(
    table: &str,
    mismatches: &mut Vec<String>,
    build: unsafe fn(&[u32]) -> *mut PyObject,
    hash_type: *mut PyTypeObject,
) {
    for (lineno, key, row) in hash_rows(table) {
        tstate::clear();
        unicode::set_hash_key_for_tests(key.0, key.1);
        // SAFETY: the fresh object is hashed through the static type.
        let got = unsafe {
            let obj = build(&unescape(&row[0]));
            let hash = ((*hash_type).tp_hash.unwrap())(obj);
            format!("i:{hash}")
        };
        let want = row[1]
            .parse::<i64>()
            .map(|value| int_tag(value as isize))
            .unwrap_or_else(|_| row[1].clone());
        check(mismatches, table, lineno, &row, got, want);
    }
    unicode::set_hash_key_for_tests(0, 0);
}

#[test]
fn str_hash_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    check_hash_table("str_hash.tsv", &mut mismatches, str_object, STR_TYPE);
    finish("str_hash.tsv", mismatches);
}

#[test]
fn bytes_hash_matches_cpython() {
    init();
    let mut mismatches = Vec::new();
    check_hash_table(
        "bytes_hash.tsv",
        &mut mismatches,
        // SAFETY: builds a fresh bytes object from the row's bytes; the
        // shared single-character cache would pin a hash computed under an
        // earlier section's key.
        |chars| unsafe {
            let bytes: Vec<u8> = chars.iter().map(|&c| c as u8).collect();
            let space = rt_bytes::PyBytes_FromStringAndSize(std::ptr::null(), bytes.len() as isize);
            assert!(!space.is_null());
            if !bytes.is_empty() {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    rt_bytes::PyBytes_AsString(space) as *mut u8,
                    bytes.len(),
                );
            }
            space
        },
        BYTES_TYPE,
    );
    finish("bytes_hash.tsv", mismatches);
}

#[test]
fn interning_deduplicates_strings() {
    init();
    // SAFETY: the interning C API surface, exercised on fresh strings.
    unsafe {
        let first = unicode::PyUnicode_InternFromString(c"coil.intern.me".as_ptr());
        let second = unicode::PyUnicode_InternFromString(c"coil.intern.me".as_ptr());
        assert!(!first.is_null() && !second.is_null());
        assert!(
            core::ptr::eq(first, second),
            "same content shares one object"
        );
        let state = (*first.cast::<unicode::PyASCIIObject>()).state;
        assert_eq!(state.interned(), 1, "mortal interning marks the state");

        // InternInPlace replaces a heap duplicate with the table entry;
        // the entry must exist first, so seed it.
        let seed = unicode::PyUnicode_InternFromString(c"another one".as_ptr());
        let fresh = str_object(&unescape("another one"));
        let mut p = fresh;
        unicode::PyUnicode_InternInPlace(&mut p);
        assert!(!core::ptr::eq(p, fresh), "the duplicate is replaced");
        assert!(core::ptr::eq(p, seed), "the replacement is the table entry");
        assert_eq!((*p.cast::<unicode::PyASCIIObject>()).state.interned(), 1);

        // InternImmortal marks the state's immortal bit (2).
        let mut q = str_object(&unescape("identifier"));
        unicode::PyUnicode_InternImmortal(&mut q);
        assert_eq!((*q.cast::<unicode::PyASCIIObject>()).state.interned(), 2);
    }
}

#[test]
fn bytes_object_api_roundtrips() {
    use coil_capi_sys::{Py_buffer, PyBUF_SIMPLE, PyBUF_WRITABLE};
    init();
    // SAFETY: every object below is created, checked and read in place.
    unsafe {
        // FromStringAndSize(NULL, n) hands back writable space.
        let space = rt_bytes::PyBytes_FromStringAndSize(std::ptr::null(), 5);
        assert!(!space.is_null());
        assert_eq!(rt_bytes::PyBytes_Size(space), 5);
        let raw = rt_bytes::PyBytes_AsString(space);
        core::ptr::copy_nonoverlapping(b"12345".as_ptr(), raw.cast::<u8>(), 5);
        let mut buffer: *mut c_char = std::ptr::null_mut();
        let mut size = 0isize;
        assert_eq!(
            rt_bytes::PyBytes_AsStringAndSize(space, &mut buffer, &mut size),
            0
        );
        let data = buffer;
        assert_eq!(size, 5);
        assert_eq!(std::slice::from_raw_parts(data as *const u8, 5), b"12345");

        // Concat appends in place through the pointer slot.
        let mut target = bytes_object(b"ab");
        rt_bytes::PyBytes_Concat(&mut target, bytes_object(b"cd"));
        assert_eq!(rt_bytes::PyBytes_Size(target), 4);
        assert_eq!(
            std::slice::from_raw_parts(rt_bytes::PyBytes_AsString(target) as *const u8, 4),
            b"abcd"
        );
        let mut grown = bytes_object(b"x");
        rt_bytes::PyBytes_ConcatAndDel(&mut grown, bytes_object(b"yz"));
        assert_eq!(
            std::slice::from_raw_parts(rt_bytes::PyBytes_AsString(grown) as *const u8, 3),
            b"xyz"
        );

        // _PyBytes_Resize grows and shrinks the allocation.
        let mut resized = bytes_object(b"abcd");
        assert_eq!(rt_bytes::_PyBytes_Resize(&mut resized, 2), 0);
        assert_eq!(
            std::slice::from_raw_parts(rt_bytes::PyBytes_AsString(resized) as *const u8, 2),
            b"ab"
        );
        assert_eq!(rt_bytes::PyBytes_Size(resized), 2);

        // FromObject copies any bytes-like; PyObject_Bytes too.
        let copied = rt_bytes::PyBytes_FromObject(bytes_object(b"copy"));
        assert_eq!(
            std::slice::from_raw_parts(rt_bytes::PyBytes_AsString(copied) as *const u8, 4),
            b"copy"
        );
        let via_bytes = rt_bytes::PyObject_Bytes(bytes_object(b"obj"));
        assert_eq!(rt_bytes::PyBytes_Size(via_bytes), 3);

        // bytearray: build, read, resize both ways.
        let array = rt_bytes::PyByteArray_FromStringAndSize(b"seed".as_ptr() as *const c_char, 4);
        assert!(!array.is_null());
        assert_eq!(rt_bytes::PyByteArray_Size(array), 4);
        assert_eq!(rt_bytes::PyByteArray_Resize(array, 8), 0);
        assert_eq!(rt_bytes::PyByteArray_Size(array), 8);
        assert_eq!(rt_bytes::PyByteArray_Resize(array, 2), 0);
        assert_eq!(rt_bytes::PyByteArray_Size(array), 2);
        let from_obj = rt_bytes::PyByteArray_FromObject(bytes_object(b"zz"));
        assert_eq!(rt_bytes::PyByteArray_Size(from_obj), 2);

        // The buffer protocol over both types.
        let mut view = Py_buffer {
            buf: std::ptr::null_mut(),
            obj: std::ptr::null_mut(),
            len: 0,
            itemsize: 0,
            readonly: 0,
            ndim: 0,
            format: std::ptr::null_mut(),
            shape: std::ptr::null_mut(),
            strides: std::ptr::null_mut(),
            suboffsets: std::ptr::null_mut(),
            internal: std::ptr::null_mut(),
        };
        assert_eq!(
            rt_bytes::PyObject_GetBuffer(bytes_object(b"buf"), &mut view, PyBUF_SIMPLE),
            0
        );
        assert_eq!(view.len, 3);
        assert_eq!(std::slice::from_raw_parts(view.buf as *const u8, 3), b"buf");
        rt_bytes::PyBuffer_Release(&mut view);
        assert_eq!(
            rt_bytes::PyObject_GetBuffer(array, &mut view, PyBUF_WRITABLE),
            0
        );
        assert_eq!(view.len, 2);
        assert_eq!(view.readonly, 0, "bytearray exports writable views");
        rt_bytes::PyBuffer_Release(&mut view);
        // bytes refuses writable exports.
        let immutable = rt_bytes::PyObject_GetBuffer(bytes_object(b"b"), &mut view, PyBUF_WRITABLE);
        assert_eq!(immutable, -1);
        assert!(!tstate::occurred().is_null());
        tstate::clear();

        // A filled C view via PyBuffer_FillInfo reads back.
        let storage: [u8; 4] = *b"hi!!";
        assert_eq!(
            rt_bytes::PyBuffer_FillInfo(
                &mut view,
                std::ptr::null_mut(),
                storage.as_ptr() as *mut c_void,
                4,
                1,
                PyBUF_SIMPLE
            ),
            0
        );
        assert_eq!(
            std::slice::from_raw_parts(view.buf as *const u8, 4),
            b"hi!!"
        );
    }
}

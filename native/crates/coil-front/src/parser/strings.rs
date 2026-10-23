//! The grammar's literal rules: numbers, strings, f-strings and their
//! decoders (spec section 4). The escape decoders mirror
//! `_PyUnicode_DecodeUnicodeEscapeInternal` and `_PyBytes_DecodeEscape` of
//! the pinned CPython, and `\N{...}` uses its name table.

use super::expressions;
use super::helpers::utf32_to_string;
use super::invalid;
use super::unicode_names;
use super::{ExcKind, Parser, Pos};
use crate::ast::Constant;
use crate::ast_generated::Expr;
use crate::tokenizer::TokenKind as T;

// -- numbers --------------------------------------------------------------------------

/// `_PyPegen_number_token`: NUMBER to a Constant, with `parsenumber`'s
/// handling of underscores, bases and the 4300-digit decimal limit.
pub(crate) fn number_token(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let index = p.mark;
        p.expect_opt(T::Number)?;
        let t = p.tok_at(index).clone();
        let value = match parse_number(&t.bytes) {
            Ok(value) => value,
            Err(NumberError::TooManyDigits) => {
                p.macro_raise(
                    ExcKind::Syntax,
                    t.lineno as i64,
                    -1,
                    t.end_lineno as i64,
                    -1,
                    "Exceeds the limit (4300 digits) for integer string conversion; use "
                        .to_string()
                        + "hexadecimal for huge integer literals to avoid decimal conversion "
                        + "limits",
                );
                return None;
            }
            Err(NumberError::Malformed) => {
                p.raise_syntax_error("invalid decimal literal".to_string());
                return None;
            }
        };
        let pos = Pos::from_token(&t);
        Some(Expr::Constant {
            value,
            kind: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    p.exit_rule();
    res
}

enum NumberError {
    TooManyDigits,
    Malformed,
}

fn parse_number(raw: &[u8]) -> Result<Constant, NumberError> {
    let digits: Vec<u8> = raw.iter().copied().filter(|&b| b != b'_').collect();
    let s = digits.as_slice();
    if s.is_empty() {
        return Err(NumberError::Malformed);
    }
    let last = s[s.len() - 1];
    if last == b'j' || last == b'J' {
        let imag = parse_f64(&s[..s.len() - 1])?;
        return Ok(Constant::Complex { real: 0.0, imag });
    }
    if let Some((base, body)) = int_base(s) {
        if base == 10 && body.len() > 4300 {
            return Err(NumberError::TooManyDigits);
        }
        return Ok(Constant::Int(parse_int_decimal(body, base)));
    }
    Ok(Constant::Float(parse_f64(s)?))
}

/// The base and digit body of an integer literal, `None` for floats.
fn int_base(s: &[u8]) -> Option<(u32, &[u8])> {
    if s.len() >= 2 && s[0] == b'0' {
        let body = &s[2..];
        match s[1] {
            b'x' | b'X' if body.iter().all(|b| b.is_ascii_hexdigit()) => return Some((16, body)),
            b'o' | b'O' if body.iter().all(|b| (b'0'..=b'7').contains(b)) => {
                return Some((8, body));
            }
            b'b' | b'B' if body.iter().all(|b| *b == b'0' || *b == b'1') => return Some((2, body)),
            _ => {}
        }
    }
    if s.iter().all(|b| b.is_ascii_digit()) {
        Some((10, s))
    } else {
        None
    }
}

/// Arbitrary-precision base conversion on a decimal digit string: no bignum
/// crate, just multiply-accumulate.
fn parse_int_decimal(digits: &[u8], base: u32) -> String {
    let mut out: Vec<u8> = vec![0];
    for &d in digits {
        let add = match d {
            b'0'..=b'9' => (d - b'0') as u32,
            b'a'..=b'f' => (d - b'a' + 10) as u32,
            b'A'..=b'F' => (d - b'A' + 10) as u32,
            _ => 0,
        };
        let mut carry = add;
        for cell in out.iter_mut().rev() {
            let value = (*cell as u32) * base + carry;
            *cell = (value % 10) as u8;
            carry = value / 10;
        }
        while carry > 0 {
            out.insert(0, (carry % 10) as u8);
            carry /= 10;
        }
    }
    let first_nonzero = out.iter().position(|&d| d != 0).unwrap_or(out.len() - 1);
    out[first_nonzero..]
        .iter()
        .map(|d| (b'0' + d) as char)
        .collect()
}

fn parse_f64(s: &[u8]) -> Result<f64, NumberError> {
    let text = std::str::from_utf8(s).map_err(|_| NumberError::Malformed)?;
    text.parse::<f64>().map_err(|_| NumberError::Malformed)
}

// -- string decoding ------------------------------------------------------------------

enum DecodeError {
    Unicode(String),
    Value(String),
}

/// The decoded body and its prefix flags.
struct RawLiteral<'a> {
    body: &'a [u8],
    bytes: bool,
    raw: bool,
}

/// `_PyPegen_parse_string`'s prefix and quote scan. The tokenizer only
/// produces well-formed literals, so the checks are structural.
fn split_literal(bytes: &[u8]) -> RawLiteral<'_> {
    let mut i = 0;
    let mut bytes_mode = false;
    let mut raw_mode = false;
    while i < bytes.len() {
        match bytes[i] {
            b'b' | b'B' if !bytes_mode => bytes_mode = true,
            b'u' | b'U' => {}
            b'r' | b'R' if !raw_mode => raw_mode = true,
            _ => break,
        }
        i += 1;
    }
    let rest = &bytes[i..];
    let quote = rest[0];
    let triple = rest.len() >= 6 && rest[1] == quote && rest[2] == quote;
    let body = if triple {
        &rest[3..rest.len() - 3]
    } else {
        &rest[1..rest.len() - 1]
    };
    let raw = raw_mode || !body.contains(&b'\\');
    RawLiteral {
        body,
        bytes: bytes_mode,
        raw,
    }
}

/// `str(UnicodeDecodeError)` for a strict UTF-8 decode of `bytes`: the
/// message CPython quotes in `(unicode error)` SyntaxErrors. The position is
/// an index into `bytes`, so raw literals report it over the whole body and
/// the escape pass over the failing non-ASCII run.
fn utf8_decode_error(bytes: &[u8], err: &std::str::Utf8Error) -> String {
    let start = err.valid_up_to();
    match err.error_len() {
        None => {
            let end = bytes.len() - 1;
            if end == start {
                format!(
                    "'utf-8' codec can't decode byte 0x{:02x} in position {start}: unexpected end of data",
                    bytes[start]
                )
            } else {
                format!(
                    "'utf-8' codec can't decode bytes in position {start}-{end}: unexpected end of data"
                )
            }
        }
        // A valid lead byte with a broken continuation, or a byte that cannot
        // start a sequence at all — CPython's two continuation failures.
        Some(_) => {
            let lead = bytes[start];
            let reason = if (0xC2..=0xF4).contains(&lead) {
                "invalid continuation byte"
            } else {
                "invalid start byte"
            };
            format!("'utf-8' codec can't decode byte 0x{lead:02x} in position {start}: {reason}")
        }
    }
}

/// `str(UnicodeDecodeError)` for an escape failure: `exceptions.c` shows the
/// offending byte when the span covers exactly one, else the half-open span
/// with its last index.
fn unicodeescape_error(s: &[u8], start: usize, end: usize, reason: &str) -> String {
    if end == start + 1 && start < s.len() {
        format!(
            "'unicodeescape' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            s[start]
        )
    } else {
        format!(
            "'unicodeescape' codec can't decode bytes in position {start}-{}: {reason}",
            end - 1
        )
    }
}

/// `_PyPegen_decode_string` for a string constant: raw text is UTF-8,
/// otherwise the escape preprocessor plus the unicode-escape decoder.
fn decode_string(p: &mut Parser, raw: bool, s: &[u8]) -> Option<Vec<u32>> {
    if raw {
        return match std::str::from_utf8(s) {
            Ok(text) => Some(text.chars().map(|c| c as u32).collect()),
            Err(e) => {
                p.raise_string_decode_error("unicode error", utf8_decode_error(s, &e));
                None
            }
        };
    }
    let preprocessed = match escape_preprocess(s) {
        Ok(preprocessed) => preprocessed,
        Err(message) => {
            p.raise_string_decode_error("unicode error", message);
            return None;
        }
    };
    match decode_unicode_escapes(&preprocessed) {
        Ok(value) => Some(value),
        Err(DecodeError::Unicode(message)) => {
            p.raise_decode_error("unicode error", message);
            None
        }
        Err(DecodeError::Value(message)) => {
            p.raise_decode_error("value error", message);
            None
        }
    }
}

/// `decode_unicode_with_escapes`' first pass: everything non-ASCII becomes a
/// `\U00xxxxxx` escape and a backslash before a non-ASCII byte becomes
/// `\`, so the escape decoder sees ASCII only. A non-ASCII run decodes
/// strictly, and its failure is the `UnicodeDecodeError` CPython raises
/// (positions relative to the run).
fn escape_preprocess(s: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' {
            out.push(b'\\');
            i += 1;
            if i >= s.len() || s[i] >= 0x80 {
                out.extend_from_slice(b"u005c");
                if i >= s.len() {
                    break;
                }
            }
        }
        if s[i] >= 0x80 {
            let start = i;
            while i < s.len() && s[i] >= 0x80 {
                i += 1;
            }
            match std::str::from_utf8(&s[start..i]) {
                Ok(run) => {
                    for c in run.chars() {
                        out.extend_from_slice(format!("\\U{:08x}", c as u32).as_bytes());
                    }
                }
                Err(e) => return Err(utf8_decode_error(&s[start..i], &e)),
            }
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn hex_value(b: u8) -> Option<u32> {
    match b {
        b'0'..=b'9' => Some((b - b'0') as u32),
        b'a'..=b'f' => Some((b - b'a' + 10) as u32),
        b'A'..=b'F' => Some((b - b'A' + 10) as u32),
        _ => None,
    }
}

/// `_PyUnicode_DecodeUnicodeEscapeInternal` on an ASCII buffer. Invalid
/// escape characters are kept verbatim (CPython warns and continues); the
/// hard errors match its decode failures.
fn decode_unicode_escapes(s: &[u8]) -> Result<Vec<u32>, DecodeError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        i += 1;
        if c != b'\\' {
            out.push(c as u32);
            continue;
        }
        // The error handler runs with the backslash's index and the scan's
        // stop as the failure span.
        let startinpos = i - 1;
        if i >= s.len() {
            return Err(DecodeError::Unicode(unicodeescape_error(
                s,
                startinpos,
                s.len(),
                "\\ at end of string",
            )));
        }
        let c = s[i];
        i += 1;
        match c {
            b'\n' => continue,
            b'\\' => out.push('\\' as u32),
            b'\'' => out.push('\'' as u32),
            b'"' => out.push('"' as u32),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b't' => out.push('\t' as u32),
            b'n' => out.push('\n' as u32),
            b'r' => out.push('\r' as u32),
            b'v' => out.push(0x0b),
            b'a' => out.push(0x07),
            b'0'..=b'7' => {
                let mut ch = (c - b'0') as u32;
                if i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                    ch = (ch << 3) + (s[i] - b'0') as u32;
                    i += 1;
                    if i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                        ch = (ch << 3) + (s[i] - b'0') as u32;
                        i += 1;
                    }
                }
                out.push(ch);
            }
            b'x' | b'u' | b'U' => {
                let count = match c {
                    b'x' => 2,
                    b'u' => 4,
                    _ => 8,
                };
                let kind = match c {
                    b'x' => "xXX",
                    b'u' => "uXXXX",
                    _ => "UXXXXXXXX",
                };
                let message = format!("truncated \\{kind} escape");
                let mut ch: u32 = 0;
                for _ in 0..count {
                    if i >= s.len() {
                        return Err(DecodeError::Unicode(unicodeescape_error(
                            s,
                            startinpos,
                            s.len(),
                            &message,
                        )));
                    }
                    match hex_value(s[i]) {
                        Some(d) => ch = (ch << 4) + d,
                        None => {
                            return Err(DecodeError::Unicode(unicodeescape_error(
                                s, startinpos, i, &message,
                            )));
                        }
                    }
                    i += 1;
                }
                if ch > 0x10FFFF {
                    return Err(DecodeError::Unicode(unicodeescape_error(
                        s,
                        startinpos,
                        i,
                        "illegal Unicode character",
                    )));
                }
                out.push(ch);
            }
            b'N' => {
                let mut message = "malformed \\N character escape";
                if i >= s.len() {
                    return Err(DecodeError::Unicode(unicodeescape_error(
                        s,
                        startinpos,
                        s.len(),
                        message,
                    )));
                }
                if s[i] == b'{' {
                    i += 1;
                    let start = i;
                    while i < s.len() && s[i] != b'}' {
                        i += 1;
                    }
                    if i >= s.len() {
                        return Err(DecodeError::Unicode(unicodeescape_error(
                            s,
                            startinpos,
                            s.len(),
                            message,
                        )));
                    }
                    let name = &s[start..i];
                    if !name.is_empty() {
                        // A real name consumes the closing brace; an empty
                        // or unknown one reports at or before it.
                        i += 1;
                        if let Some(ch) = lookup_unicode_name(name) {
                            out.push(ch);
                            continue;
                        }
                        message = "unknown Unicode character name";
                    }
                }
                return Err(DecodeError::Unicode(unicodeescape_error(
                    s, startinpos, i, message,
                )));
            }
            _ => {
                out.push('\\' as u32);
                out.push(c as u32);
            }
        }
    }
    Ok(out)
}

/// `_PyBytes_DecodeEscape`.
fn decode_bytes_escapes(s: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if s[i] != b'\\' {
            out.push(s[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i >= s.len() {
            return Err(DecodeError::Value("Trailing \\ in string".to_string()));
        }
        let c = s[i];
        i += 1;
        match c {
            b'\n' => {}
            b'\\' => out.push(b'\\'),
            b'\'' => out.push(b'\''),
            b'"' => out.push(b'"'),
            b'b' => out.push(0x08),
            b'f' => out.push(0x0c),
            b't' => out.push(b'\t'),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b'v' => out.push(0x0b),
            b'a' => out.push(0x07),
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                if i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                    v = (v << 3) + (s[i] - b'0') as u32;
                    i += 1;
                    if i < s.len() && (b'0'..=b'7').contains(&s[i]) {
                        v = (v << 3) + (s[i] - b'0') as u32;
                        i += 1;
                    }
                }
                out.push((v & 0xff) as u8);
            }
            b'x' => {
                if i + 1 < s.len()
                    && let (Some(d1), Some(d2)) = (hex_value(s[i]), hex_value(s[i + 1]))
                {
                    out.push(((d1 << 4) + d2) as u8);
                    i += 2;
                    continue;
                }
                return Err(DecodeError::Value(format!(
                    "invalid \\x escape at position {}",
                    i.saturating_sub(2)
                )));
            }
            _ => {
                out.push(b'\\');
                out.push(c);
            }
        }
    }
    Ok(out)
}

// -- \N{...} name lookup ---------------------------------------------------------------

const HANGUL_SYLLABLES: [[&str; 3]; 28] = [
    ["G", "A", ""],
    ["GG", "AE", "G"],
    ["N", "YA", "GG"],
    ["D", "YAE", "GS"],
    ["DD", "EO", "N"],
    ["R", "E", "NJ"],
    ["M", "YEO", "NH"],
    ["B", "YE", "D"],
    ["BB", "O", "L"],
    ["S", "WA", "LG"],
    ["SS", "WAE", "LM"],
    ["", "OE", "LB"],
    ["J", "YO", "LS"],
    ["JJ", "U", "LT"],
    ["C", "WEO", "LP"],
    ["K", "WE", "LH"],
    ["T", "WI", "M"],
    ["P", "YU", "B"],
    ["H", "EU", "BS"],
    ["", "YI", "S"],
    ["", "I", "SS"],
    ["", "", "NG"],
    ["", "", "J"],
    ["", "", "C"],
    ["", "", "K"],
    ["", "", "T"],
    ["", "", "P"],
    ["", "", "H"],
];

fn is_unified_ideograph(code: u32) -> bool {
    (0x3400..=0x4DBF).contains(&code)
        || (0x4E00..=0x9FFF).contains(&code)
        || (0x20000..=0x2A6DF).contains(&code)
        || (0x2A700..=0x2B739).contains(&code)
        || (0x2B740..=0x2B81D).contains(&code)
        || (0x2B820..=0x2CEA1).contains(&code)
        || (0x2CEB0..=0x2EBE0).contains(&code)
        || (0x30000..=0x3134A).contains(&code)
        || (0x31350..=0x323AF).contains(&code)
}

fn find_syllable(name: &[u8], column: usize, count: usize) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for (i, entry) in HANGUL_SYLLABLES.iter().enumerate().take(count) {
        let syllable = entry[column].as_bytes();
        if syllable.len() > best.map_or(0, |b| b.1) && name.starts_with(syllable) {
            best = Some((i, syllable.len()));
        }
    }
    best
}

/// The parser's `ucnhash_capi->getcode` with `with_named_seq` 0: the stored
/// names (case-insensitively) plus the algorithmic Hangul and CJK names,
/// whose prefixes must match exactly.
fn lookup_unicode_name(name: &[u8]) -> Option<u32> {
    let upper: Vec<u8> = name.iter().map(|b| b.to_ascii_uppercase()).collect();
    if upper.starts_with(b"HANGUL SYLLABLE ") {
        let mut pos = 16;
        let (l, len) = find_syllable(&upper[pos..], 0, 19)?;
        pos += len;
        let (v, len) = find_syllable(&upper[pos..], 1, 21)?;
        pos += len;
        let (t, len) = find_syllable(&upper[pos..], 2, 28)?;
        pos += len;
        if pos == upper.len() {
            return Some(0xAC00 + ((l * 21 + v) * 28 + t) as u32);
        }
        return None;
    }
    if upper.starts_with(b"CJK UNIFIED IDEOGRAPH-") {
        let hex = &upper[22..];
        if hex.len() != 4 && hex.len() != 5 {
            return None;
        }
        let mut v: u32 = 0;
        for &b in hex {
            let d = match b {
                b'0'..=b'9' => b - b'0',
                b'A'..=b'F' => b - b'A' + 10,
                _ => return None,
            };
            v = v * 16 + d as u32;
        }
        return is_unified_ideograph(v).then_some(v);
    }
    let text = std::str::from_utf8(&upper).ok()?;
    let mut low = 0usize;
    let mut high = unicode_names::OFFSETS.len();
    while low < high {
        let mid = (low + high) / 2;
        let (name_start, name_end) = (unicode_names::OFFSETS[mid] as usize, {
            let end = if mid + 1 < unicode_names::OFFSETS.len() {
                unicode_names::OFFSETS[mid + 1] as usize
            } else {
                unicode_names::NAMES.len()
            };
            end - 1
        });
        let stored = &unicode_names::NAMES.as_bytes()[name_start..name_end];
        match stored.cmp(text.as_bytes()) {
            std::cmp::Ordering::Equal => return Some(unicode_names::CODEPOINTS[mid]),
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
        }
    }
    None
}

// -- grammar rules --------------------------------------------------------------------

/// `string`: STRING to a Constant via `_PyPegen_constant_from_string`.
pub(crate) fn string(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let index = p.mark;
        p.expect_opt(T::String)?;
        let t = p.tok_at(index).clone();
        constant_from_string(p, &t)
    });
    p.exit_rule();
    res
}

fn constant_from_string(p: &mut Parser, t: &crate::tokenizer::PegToken) -> Option<Expr> {
    let literal = split_literal(&t.bytes);
    let value = if literal.bytes {
        if literal.body.iter().any(|&b| b >= 0x80) {
            p.raise_known_location(
                Pos::from_token(t),
                "bytes can only contain ASCII literal characters".to_string(),
            );
            return None;
        }
        if literal.raw {
            Constant::Bytes(literal.body.to_vec())
        } else {
            match decode_bytes_escapes(literal.body) {
                Ok(value) => Constant::Bytes(value),
                Err(DecodeError::Value(message)) | Err(DecodeError::Unicode(message)) => {
                    p.raise_decode_error("value error", message);
                    return None;
                }
            }
        }
    } else {
        Constant::Str(decode_string(p, literal.raw, literal.body)?)
    };
    let kind = if t.bytes.first() == Some(&b'u') {
        Some(p.interner.intern("u"))
    } else {
        None
    };
    let pos = Pos::from_token(t);
    Some(Expr::Constant {
        value,
        kind,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

struct ResultWithMetadata {
    result: Expr,
    metadata: Option<Box<str>>,
}

pub(crate) fn fstring_middle(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[fstring_middle_field, fstring_middle_text]);
    p.exit_rule();
    res
}

fn fstring_middle_field(p: &mut Parser) -> Option<Expr> {
    fstring_replacement_field(p)
}

fn fstring_middle_text(p: &mut Parser) -> Option<Expr> {
    let index = p.mark;
    p.expect_opt(T::FStringMiddle)?;
    let t = p.tok_at(index).clone();
    let pos = Pos::from_token(&t);
    let value: Vec<u32> = match std::str::from_utf8(&t.bytes) {
        Ok(text) => text.chars().map(|c| c as u32).collect(),
        Err(e) => {
            p.raise_decode_error("unicode error", utf8_decode_error(&t.bytes, &e));
            return None;
        }
    };
    Some(Expr::Constant {
        value: Constant::Str(value),
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn fstring_replacement_field(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[fstring_replacement_field_main, |p| {
        if p.call_invalid_rules {
            invalid::replacement_field(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn fstring_replacement_field_main(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let value = p.alternatives(&[
        |p| expressions::yield_expr(p),
        |p| expressions::star_expressions(p),
    ])?;
    let debug = p.expect(T::Equal);
    let conversion = p.attempt(fstring_conversion);
    let format = p.attempt(fstring_full_format_spec);
    let rbrace = p.expect(T::Rbrace);
    if !rbrace {
        return None;
    }
    let rbrace_index = p.mark - 1;
    let pos = p.extra(p.pos_at(m))?;
    formatted_value(p, value, debug, conversion, format, rbrace_index, pos)
}

fn fstring_conversion(p: &mut Parser) -> Option<ResultWithMetadata> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let conv_index = p.mark;
        p.expect_opt(T::Exclamation)?;
        let conv = p.name_token()?;
        let conv_token = p.tok_at(conv_index).clone();
        let conv_pos = Pos::from_expr(&conv);
        if conv_token.lineno as i32 != conv_pos.lineno
            || conv_token.end_col_offset != conv_pos.col_offset as i64
        {
            p.raise_known_range(
                Pos::from_token(&conv_token),
                conv_pos,
                "f-string: conversion type must come right after the exclamanation mark"
                    .to_string(),
            );
            return None;
        }
        Some(ResultWithMetadata {
            result: conv,
            metadata: conv_token.metadata,
        })
    });
    p.exit_rule();
    res
}

fn fstring_full_format_spec(p: &mut Parser) -> Option<ResultWithMetadata> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let colon_index = p.mark;
        p.expect_opt(T::Colon)?;
        let spec = p.repeat0(fstring_format_spec);
        let pos = p.extra(p.pos_at(colon_index))?;
        let result = setup_full_format_spec(p, spec, pos)?;
        let colon = p.tok_at(colon_index).clone();
        Some(ResultWithMetadata {
            result,
            metadata: colon.metadata,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn fstring_format_spec(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[fstring_format_spec_text, fstring_format_spec_field]);
    p.exit_rule();
    res
}

fn fstring_format_spec_text(p: &mut Parser) -> Option<Expr> {
    let index = p.mark;
    p.expect_opt(T::FStringMiddle)?;
    let t = p.tok_at(index).clone();
    let pos = Pos::from_token(&t);
    let value = decode_string(p, false, &t.bytes)?;
    Some(Expr::Constant {
        value: Constant::Str(value),
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn fstring_format_spec_field(p: &mut Parser) -> Option<Expr> {
    fstring_replacement_field(p)
}

pub(crate) fn fstring(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let start_index = p.mark;
        p.expect_opt(T::FStringStart)?;
        let values = p.repeat0(fstring_middle);
        let end_index = p.mark;
        p.expect_opt(T::FStringEnd)?;
        let start = p.tok_at(start_index).clone();
        let end = p.tok_at(end_index).clone();
        joined_str(p, &start, values, &end)
    });
    p.exit_rule();
    res
}

/// `_PyPegen_setup_full_format_spec`.
fn setup_full_format_spec(p: &mut Parser, mut spec: Vec<Expr>, pos: Pos) -> Option<Expr> {
    if spec.len() == 1
        && let Expr::Constant {
            value: Constant::Str(value),
            ..
        } = &spec[0]
        && value.is_empty()
    {
        spec = Vec::new();
    }
    if spec.is_empty() || (spec.len() == 1 && matches!(spec[0], Expr::Constant { .. })) {
        return Some(Expr::JoinedStr {
            values: spec,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        });
    }
    concatenate_strings(p, spec, pos)
}

/// `_PyPegen_joined_str`.
fn joined_str(
    p: &mut Parser,
    start: &crate::tokenizer::PegToken,
    exprs: Vec<Expr>,
    end: &crate::tokenizer::PegToken,
) -> Option<Expr> {
    let is_raw = start.bytes.iter().any(|&b| b == b'r' || b == b'R');
    let mut values = Vec::with_capacity(exprs.len());
    for item in exprs {
        match item {
            // The two-element debug JoinedStr from formatted_value.
            Expr::JoinedStr { values: nested, .. } => {
                if nested.len() != 2 {
                    p.raise_known_location(
                        Pos::from_token(start),
                        "unexpected JoinedStr node without debug data in f-string".to_string(),
                    );
                    return None;
                }
                values.push(nested[0].clone());
                values.push(nested[1].clone());
            }
            Expr::Constant {
                value: Constant::Str(text),
                kind,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                let decoded = decode_fstring_part(p, is_raw, &text)?;
                // Tokenizer string parts may decode to an empty value.
                if decoded.is_empty() {
                    continue;
                }
                values.push(Expr::Constant {
                    value: Constant::Str(decoded),
                    kind,
                    lineno,
                    col_offset,
                    end_lineno,
                    end_col_offset,
                });
            }
            other => values.push(other),
        }
    }
    Some(Expr::JoinedStr {
        values,
        lineno: start.lineno as i32,
        col_offset: start.col_offset as i32,
        end_lineno: Some(end.end_lineno as i32),
        end_col_offset: Some(end.end_col_offset as i32),
    })
}

/// `_PyPegen_decode_fstring_part`: a `{{`/`}}` token collapses to one
/// character, and the escape decoding respects the f-string's `raw` prefix.
fn decode_fstring_part(p: &mut Parser, is_raw: bool, text: &[u32]) -> Option<Vec<u32>> {
    let len = if text.len() == 2
        && ((text[0] == '{' as u32 && text[1] == '{' as u32)
            || (text[0] == '}' as u32 && text[1] == '}' as u32))
    {
        1
    } else {
        text.len()
    };
    let raw = is_raw || !text.contains(&('\\' as u32));
    let mut bytes = Vec::with_capacity(text.len());
    for &c in &text[..len] {
        match char::from_u32(c) {
            Some(ch) => {
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
            // The text came from UTF-8, so every code point encodes.
            None => bytes.push(b'?'),
        }
    }
    decode_string(p, raw, &bytes)
}

/// `_PyPegen_formatted_value`.
fn formatted_value(
    p: &mut Parser,
    expression: Expr,
    debug: bool,
    conversion: Option<ResultWithMetadata>,
    format: Option<ResultWithMetadata>,
    closing_brace: usize,
    pos: Pos,
) -> Option<Expr> {
    let mut conversion_val: i32 = -1;
    if let Some(conv) = &conversion {
        let text = p.interner.resolve(name_id(&conv.result)?);
        let first = text.first().copied().unwrap_or(0);
        if text.len() > 1 || !(first == 's' as u32 || first == 'r' as u32 || first == 'a' as u32) {
            p.raise_known_location(
                Pos::from_expr(&conv.result),
                format!(
                    "f-string: invalid conversion character '{}': expected 's', 'r', or 'a'",
                    utf32_to_string(text)
                ),
            );
            return None;
        }
        conversion_val = first as i32;
    } else if debug && format.is_none() {
        // If no conversion is specified, use !r for debug expressions.
        conversion_val = 'r' as i32;
    }

    let spec = format.as_ref().map(|f| Box::new(f.result.clone()));
    let formatted = Expr::FormattedValue {
        value: Box::new(expression),
        conversion: conversion_val,
        format_spec: spec,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    };

    if !debug {
        return Some(formatted);
    }

    // Find the non whitespace token after the "=".
    let (debug_end_line, debug_end_offset, debug_metadata) = if let Some(conv) = &conversion {
        (
            Pos::from_expr(&conv.result).lineno,
            Pos::from_expr(&conv.result).col_offset,
            conv.metadata.clone(),
        )
    } else if let Some(f) = &format {
        let spec_pos = Pos::from_expr(&f.result);
        (spec_pos.lineno, spec_pos.col_offset + 1, f.metadata.clone())
    } else {
        let brace = p.tok_at(closing_brace);
        (
            pos.end_lineno.unwrap_or(pos.lineno),
            pos.end_col_offset.unwrap_or(pos.col_offset),
            brace.metadata.clone(),
        )
    };
    let debug_text = Expr::Constant {
        value: Constant::Str(
            debug_metadata
                .as_deref()
                .map(|s| s.chars().map(|c| c as u32).collect())
                .unwrap_or_default(),
        ),
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset + 1,
        end_lineno: Some(debug_end_line),
        end_col_offset: Some(debug_end_offset - 1),
    };

    Some(Expr::JoinedStr {
        values: vec![debug_text, formatted],
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: Some(debug_end_line),
        end_col_offset: Some(debug_end_offset),
    })
}

fn name_id(expr: &Expr) -> Option<crate::intern::Sym> {
    let Expr::Name { id, .. } = expr else {
        return None;
    };
    Some(*id)
}

/// `strings`: one or more adjacent literals, concatenated.
pub(crate) fn strings(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.strings) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        let seq = p.repeat1(fstring_or_string)?;
        let pos = p.extra(p.pos_at(m))?;
        concatenate_strings(p, seq, pos)
    });
    p.memo_insert(|t| &mut t.strings, m, res.clone());
    p.exit_rule();
    res
}

fn fstring_or_string(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[fstring, string]);
    p.exit_rule();
    res
}

/// `_PyPegen_concatenate_strings`.
fn concatenate_strings(p: &mut Parser, strings: Vec<Expr>, pos: Pos) -> Option<Expr> {
    let mut f_string_found = false;
    let mut unicode_string_found = false;
    let mut bytes_found = false;
    for elem in &strings {
        match elem {
            Expr::Constant {
                value: Constant::Bytes(_),
                ..
            } => bytes_found = true,
            Expr::Constant { .. } => unicode_string_found = true,
            _ => f_string_found = true,
        }
    }

    if (unicode_string_found || f_string_found) && bytes_found {
        p.raise_syntax_error("cannot mix bytes and nonbytes literals".to_string());
        return None;
    }

    if bytes_found {
        let kind = match &strings[0] {
            Expr::Constant { kind, .. } => *kind,
            _ => None,
        };
        let mut value = Vec::new();
        for elem in &strings {
            if let Expr::Constant {
                value: Constant::Bytes(b),
                ..
            } = elem
            {
                value.extend_from_slice(b);
            }
        }
        return Some(Expr::Constant {
            value: Constant::Bytes(value),
            kind,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        });
    }

    if !f_string_found && strings.len() == 1 {
        return strings.into_iter().next();
    }

    let mut flattened: Vec<Expr> = Vec::new();
    for elem in strings {
        match elem {
            Expr::JoinedStr { values, .. } => flattened.extend(values),
            other => flattened.push(other),
        }
    }

    let mut values: Vec<Expr> = Vec::new();
    let mut i = 0;
    while i < flattened.len() {
        let elem = flattened[i].clone();
        match elem {
            Expr::Constant { .. } => {
                if i + 1 < flattened.len() && matches!(flattened[i + 1], Expr::Constant { .. }) {
                    // Fold the run: the kind comes from the first constant.
                    let first = &flattened[i];
                    let kind = constant_kind(first);
                    let (first_lineno, first_col) = constant_start(first);
                    let mut text = Vec::new();
                    let mut last_end = (None, None);
                    let mut j = i;
                    while j < flattened.len() {
                        if let Expr::Constant {
                            value: Constant::Str(part),
                            end_lineno,
                            end_col_offset,
                            ..
                        } = &flattened[j]
                        {
                            text.extend_from_slice(part);
                            last_end = (*end_lineno, *end_col_offset);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    let folded = Expr::Constant {
                        value: Constant::Str(text),
                        kind,
                        lineno: first_lineno,
                        col_offset: first_col,
                        end_lineno: last_end.0,
                        end_col_offset: last_end.1,
                    };
                    i = j;
                    if f_string_found && constant_is_empty_str(&folded) {
                        continue;
                    }
                    values.push(folded);
                } else {
                    i += 1;
                    if f_string_found && constant_is_empty_str(&elem) {
                        continue;
                    }
                    values.push(elem);
                }
            }
            other => {
                i += 1;
                values.push(other);
            }
        }
    }

    if !f_string_found {
        return values.into_iter().next();
    }

    Some(Expr::JoinedStr {
        values,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn constant_kind(elem: &Expr) -> Option<crate::intern::Sym> {
    match elem {
        Expr::Constant { kind, .. } => *kind,
        _ => None,
    }
}

fn constant_start(elem: &Expr) -> (i32, i32) {
    match elem {
        Expr::Constant {
            lineno, col_offset, ..
        } => (*lineno, *col_offset),
        _ => (0, 0),
    }
}

fn constant_is_empty_str(elem: &Expr) -> bool {
    match elem {
        Expr::Constant {
            value: Constant::Str(value),
            ..
        } => value.is_empty(),
        _ => false,
    }
}

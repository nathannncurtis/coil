//! The tokenizer: a port of CPython 3.12's C tokenizer (`Parser/tokenizer.c`,
//! `Parser/token.c`, `Grammar/Tokens`) together with the encoding detection
//! of `tokenize.detect_encoding`; see the native compiler spec, section 4.
//!
//! The core (`Tokenizer` below) mirrors the C state machine one for one: it
//! consumes one decoded source line at a time, works over UTF-8 bytes in a
//! buffer with byte column arithmetic, and reports token extents as buffer
//! offsets. Two views sit on top of it:
//!
//! * the parser view (`tokenize_parser_view`): what the parser consumes. No
//!   `ENCODING`, `NL` or `COMMENT` tokens, identifiers are validated, `async`
//!   and `await` are keywords. Positions are (line, column) in code points.
//! * the tokenize-module view (`tokenize_module_view`): reproduces
//!   `tokenize.tokenize`'s stream, including `ENCODING`, `NL`, `COMMENT`, the
//!   `NEWLINE` string conventions, `async`/`await` reported as `NAME`, all
//!   operators reported as `OP`, and the trailing-`DEDENT`/`ENDMARKER`
//!   positions, with columns in code points as `tokenize` reports them.
//!
//! Supported codecs, under every alias `codecs.lookup` accepts for them:
//! UTF-8 (and UTF-8-sig), ISO-8859-1, ASCII, cp1252, ISO-8859-15 and KOI8-R.
//! Any other cookie is a `TokenizeErrorKind::UnsupportedEncoding` naming the
//! codec.

use crate::unicode_tables::{KOI8_R, XID_CONTINUE, XID_START};

const TABSIZE: i64 = 8;
const ALTTABSIZE: i64 = 1;
const MAXINDENT: usize = 100;
const MAXLEVEL: i32 = 200;
const MAXFSTRINGLEVEL: usize = 150;
const MAX_EXPR_NESTING: i32 = 3;
const EOF: i32 = -1;

/// Spaces here mean "zero or more spaces or tabs" (`type_comment_prefix`).
const TYPE_COMMENT_PREFIX: &[u8] = b"# type: ";

// --- token kinds (numbered like Grammar/Tokens) -----------------------------

/// Token kinds with CPython's numbering; `TokenKind::name` is `tok_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Endmarker,
    Name,
    Number,
    String,
    Newline,
    Indent,
    Dedent,
    Lpar,
    Rpar,
    Lsqb,
    Rsqb,
    Colon,
    Comma,
    Semi,
    Plus,
    Minus,
    Star,
    Slash,
    Vbar,
    Amber,
    Less,
    Greater,
    Equal,
    Dot,
    Percent,
    Lbrace,
    Rbrace,
    Eqequal,
    Notequal,
    Lessequal,
    Greaterequal,
    Tilde,
    Circumflex,
    Leftshift,
    Rightshift,
    Doublestar,
    Plusequal,
    Minequal,
    Starequal,
    Slashequal,
    Percentequal,
    Amperequal,
    Vbarequal,
    Circumflexequal,
    Leftshiftequal,
    Rightshiftequal,
    Doublestarequal,
    Doubleslash,
    Doubleslashequal,
    At,
    Atequal,
    Rarrow,
    Ellipsis,
    Colonequal,
    Exclamation,
    Op,
    Await,
    Async,
    TypeIgnore,
    TypeComment,
    SoftKeyword,
    FStringStart,
    FStringMiddle,
    FStringEnd,
    Comment,
    Nl,
    ErrorToken,
    Encoding,
}

impl TokenKind {
    pub fn name(self) -> &'static str {
        match self {
            TokenKind::Endmarker => "ENDMARKER",
            TokenKind::Name => "NAME",
            TokenKind::Number => "NUMBER",
            TokenKind::String => "STRING",
            TokenKind::Newline => "NEWLINE",
            TokenKind::Indent => "INDENT",
            TokenKind::Dedent => "DEDENT",
            TokenKind::Lpar => "LPAR",
            TokenKind::Rpar => "RPAR",
            TokenKind::Lsqb => "LSQB",
            TokenKind::Rsqb => "RSQB",
            TokenKind::Colon => "COLON",
            TokenKind::Comma => "COMMA",
            TokenKind::Semi => "SEMI",
            TokenKind::Plus => "PLUS",
            TokenKind::Minus => "MINUS",
            TokenKind::Star => "STAR",
            TokenKind::Slash => "SLASH",
            TokenKind::Vbar => "VBAR",
            TokenKind::Amber => "AMPER",
            TokenKind::Less => "LESS",
            TokenKind::Greater => "GREATER",
            TokenKind::Equal => "EQUAL",
            TokenKind::Dot => "DOT",
            TokenKind::Percent => "PERCENT",
            TokenKind::Lbrace => "LBRACE",
            TokenKind::Rbrace => "RBRACE",
            TokenKind::Eqequal => "EQEQUAL",
            TokenKind::Notequal => "NOTEQUAL",
            TokenKind::Lessequal => "LESSEQUAL",
            TokenKind::Greaterequal => "GREATEREQUAL",
            TokenKind::Tilde => "TILDE",
            TokenKind::Circumflex => "CIRCUMFLEX",
            TokenKind::Leftshift => "LEFTSHIFT",
            TokenKind::Rightshift => "RIGHTSHIFT",
            TokenKind::Doublestar => "DOUBLESTAR",
            TokenKind::Plusequal => "PLUSEQUAL",
            TokenKind::Minequal => "MINEQUAL",
            TokenKind::Starequal => "STAREQUAL",
            TokenKind::Slashequal => "SLASHEQUAL",
            TokenKind::Percentequal => "PERCENTEQUAL",
            TokenKind::Amperequal => "AMPEREQUAL",
            TokenKind::Vbarequal => "VBAREQUAL",
            TokenKind::Circumflexequal => "CIRCUMFLEXEQUAL",
            TokenKind::Leftshiftequal => "LEFTSHIFTEQUAL",
            TokenKind::Rightshiftequal => "RIGHTSHIFTEQUAL",
            TokenKind::Doublestarequal => "DOUBLESTAREQUAL",
            TokenKind::Doubleslash => "DOUBLESLASH",
            TokenKind::Doubleslashequal => "DOUBLESLASHEQUAL",
            TokenKind::At => "AT",
            TokenKind::Atequal => "ATEQUAL",
            TokenKind::Rarrow => "RARROW",
            TokenKind::Ellipsis => "ELLIPSIS",
            TokenKind::Colonequal => "COLONEQUAL",
            TokenKind::Exclamation => "EXCLAMATION",
            TokenKind::Op => "OP",
            TokenKind::Await => "AWAIT",
            TokenKind::Async => "ASYNC",
            TokenKind::TypeIgnore => "TYPE_IGNORE",
            TokenKind::TypeComment => "TYPE_COMMENT",
            TokenKind::SoftKeyword => "SOFT_KEYWORD",
            TokenKind::FStringStart => "FSTRING_START",
            TokenKind::FStringMiddle => "FSTRING_MIDDLE",
            TokenKind::FStringEnd => "FSTRING_END",
            TokenKind::Comment => "COMMENT",
            TokenKind::Nl => "NL",
            TokenKind::ErrorToken => "ERRORTOKEN",
            TokenKind::Encoding => "ENCODING",
        }
    }

    fn is_string_literal(self) -> bool {
        matches!(self, TokenKind::String | TokenKind::FStringMiddle)
    }

    /// Every kind `tokenize` reports as `OP`: `LPAR` through `EXCLAMATION`.
    fn is_operator(self) -> bool {
        matches!(
            self,
            TokenKind::Lpar
                | TokenKind::Rpar
                | TokenKind::Lsqb
                | TokenKind::Rsqb
                | TokenKind::Colon
                | TokenKind::Comma
                | TokenKind::Semi
                | TokenKind::Plus
                | TokenKind::Minus
                | TokenKind::Star
                | TokenKind::Slash
                | TokenKind::Vbar
                | TokenKind::Amber
                | TokenKind::Less
                | TokenKind::Greater
                | TokenKind::Equal
                | TokenKind::Dot
                | TokenKind::Percent
                | TokenKind::Lbrace
                | TokenKind::Rbrace
                | TokenKind::Eqequal
                | TokenKind::Notequal
                | TokenKind::Lessequal
                | TokenKind::Greaterequal
                | TokenKind::Tilde
                | TokenKind::Circumflex
                | TokenKind::Leftshift
                | TokenKind::Rightshift
                | TokenKind::Doublestar
                | TokenKind::Plusequal
                | TokenKind::Minequal
                | TokenKind::Starequal
                | TokenKind::Slashequal
                | TokenKind::Percentequal
                | TokenKind::Amperequal
                | TokenKind::Vbarequal
                | TokenKind::Circumflexequal
                | TokenKind::Leftshiftequal
                | TokenKind::Rightshiftequal
                | TokenKind::Doublestarequal
                | TokenKind::Doubleslash
                | TokenKind::Doubleslashequal
                | TokenKind::At
                | TokenKind::Atequal
                | TokenKind::Rarrow
                | TokenKind::Ellipsis
                | TokenKind::Colonequal
                | TokenKind::Exclamation
        )
    }
}

/// `token.EXACT_TOKEN_TYPES`: the operator kind whose exact text is `text`.
pub fn exact_token_type(text: &str) -> Option<TokenKind> {
    Some(match text {
        "(" => TokenKind::Lpar,
        ")" => TokenKind::Rpar,
        "[" => TokenKind::Lsqb,
        "]" => TokenKind::Rsqb,
        ":" => TokenKind::Colon,
        "," => TokenKind::Comma,
        ";" => TokenKind::Semi,
        "+" => TokenKind::Plus,
        "-" => TokenKind::Minus,
        "*" => TokenKind::Star,
        "/" => TokenKind::Slash,
        "|" => TokenKind::Vbar,
        "&" => TokenKind::Amber,
        "<" => TokenKind::Less,
        ">" => TokenKind::Greater,
        "=" => TokenKind::Equal,
        "." => TokenKind::Dot,
        "%" => TokenKind::Percent,
        "{" => TokenKind::Lbrace,
        "}" => TokenKind::Rbrace,
        "==" => TokenKind::Eqequal,
        "!=" => TokenKind::Notequal,
        "<=" => TokenKind::Lessequal,
        ">=" => TokenKind::Greaterequal,
        "~" => TokenKind::Tilde,
        "^" => TokenKind::Circumflex,
        "<<" => TokenKind::Leftshift,
        ">>" => TokenKind::Rightshift,
        "**" => TokenKind::Doublestar,
        "+=" => TokenKind::Plusequal,
        "-=" => TokenKind::Minequal,
        "*=" => TokenKind::Starequal,
        "/=" => TokenKind::Slashequal,
        "%=" => TokenKind::Percentequal,
        "&=" => TokenKind::Amperequal,
        "|=" => TokenKind::Vbarequal,
        "^=" => TokenKind::Circumflexequal,
        "<<=" => TokenKind::Leftshiftequal,
        ">>=" => TokenKind::Rightshiftequal,
        "**=" => TokenKind::Doublestarequal,
        "//" => TokenKind::Doubleslash,
        "//=" => TokenKind::Doubleslashequal,
        "@" => TokenKind::At,
        "@=" => TokenKind::Atequal,
        "->" => TokenKind::Rarrow,
        "..." => TokenKind::Ellipsis,
        ":=" => TokenKind::Colonequal,
        "!" => TokenKind::Exclamation,
        _ => return None,
    })
}

fn one_char(c: i32) -> TokenKind {
    match u8::try_from(c) {
        Ok(b'!') => TokenKind::Exclamation,
        Ok(b'%') => TokenKind::Percent,
        Ok(b'&') => TokenKind::Amber,
        Ok(b'(') => TokenKind::Lpar,
        Ok(b')') => TokenKind::Rpar,
        Ok(b'*') => TokenKind::Star,
        Ok(b'+') => TokenKind::Plus,
        Ok(b',') => TokenKind::Comma,
        Ok(b'-') => TokenKind::Minus,
        Ok(b'.') => TokenKind::Dot,
        Ok(b'/') => TokenKind::Slash,
        Ok(b':') => TokenKind::Colon,
        Ok(b';') => TokenKind::Semi,
        Ok(b'<') => TokenKind::Less,
        Ok(b'=') => TokenKind::Equal,
        Ok(b'>') => TokenKind::Greater,
        Ok(b'@') => TokenKind::At,
        Ok(b'[') => TokenKind::Lsqb,
        Ok(b']') => TokenKind::Rsqb,
        Ok(b'^') => TokenKind::Circumflex,
        Ok(b'{') => TokenKind::Lbrace,
        Ok(b'|') => TokenKind::Vbar,
        Ok(b'}') => TokenKind::Rbrace,
        Ok(b'~') => TokenKind::Tilde,
        _ => TokenKind::Op,
    }
}

fn two_chars(c1: i32, c2: i32) -> TokenKind {
    let (Ok(a), Ok(b)) = (u8::try_from(c1), u8::try_from(c2)) else {
        return TokenKind::Op;
    };
    match (a, b) {
        (b'!', b'=') => TokenKind::Notequal,
        (b'%', b'=') => TokenKind::Percentequal,
        (b'&', b'=') => TokenKind::Amperequal,
        (b'*', b'*') => TokenKind::Doublestar,
        (b'*', b'=') => TokenKind::Starequal,
        (b'+', b'=') => TokenKind::Plusequal,
        (b'-', b'=') => TokenKind::Minequal,
        (b'-', b'>') => TokenKind::Rarrow,
        (b'/', b'/') => TokenKind::Doubleslash,
        (b'/', b'=') => TokenKind::Slashequal,
        (b':', b'=') => TokenKind::Colonequal,
        (b'<', b'<') => TokenKind::Leftshift,
        (b'<', b'=') => TokenKind::Lessequal,
        (b'<', b'>') => TokenKind::Notequal,
        (b'=', b'=') => TokenKind::Eqequal,
        (b'>', b'=') => TokenKind::Greaterequal,
        (b'>', b'>') => TokenKind::Rightshift,
        (b'@', b'=') => TokenKind::Atequal,
        (b'^', b'=') => TokenKind::Circumflexequal,
        (b'|', b'=') => TokenKind::Vbarequal,
        _ => TokenKind::Op,
    }
}

fn three_chars(c1: i32, c2: i32, c3: i32) -> TokenKind {
    let (Ok(a), Ok(b), Ok(c)) = (u8::try_from(c1), u8::try_from(c2), u8::try_from(c3)) else {
        return TokenKind::Op;
    };
    match (a, b, c) {
        (b'*', b'*', b'=') => TokenKind::Doublestarequal,
        (b'.', b'.', b'.') => TokenKind::Ellipsis,
        (b'/', b'/', b'=') => TokenKind::Doubleslashequal,
        (b'<', b'<', b'=') => TokenKind::Leftshiftequal,
        (b'>', b'>', b'=') => TokenKind::Rightshiftequal,
        _ => TokenKind::Op,
    }
}

// --- public results ----------------------------------------------------------

/// A token with its text and (line, column) extent; columns are in code
/// points, line numbers 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub start: (u32, i64),
    pub end: (u32, i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenizeErrorKind {
    /// An error the C tokenizer raises as `SyntaxError`; the tokenize-module
    /// view reports these as `TokenError`, like `tokenize` does.
    SyntaxError,
    TokenError,
    IndentationError,
    TabError,
    /// A coding cookie naming a codec this tokenizer does not carry.
    UnsupportedEncoding,
}

impl TokenizeErrorKind {
    pub fn name(self) -> &'static str {
        match self {
            TokenizeErrorKind::SyntaxError => "SyntaxError",
            TokenizeErrorKind::TokenError => "TokenError",
            TokenizeErrorKind::IndentationError => "IndentationError",
            TokenizeErrorKind::TabError => "TabError",
            TokenizeErrorKind::UnsupportedEncoding => "UnsupportedEncoding",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenizeError {
    pub kind: TokenizeErrorKind,
    pub message: String,
    /// 1-based line; `None` when the error carries no location (as with the
    /// `SyntaxError`s from encoding detection).
    pub line: Option<u32>,
    pub column: Option<i64>,
}

impl std::fmt::Display for TokenizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.name(), self.message)?;
        if let Some(line) = self.line {
            write!(f, " (line {}", line)?;
            if let Some(column) = self.column {
                write!(f, ", column {}", column)?;
            }
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl std::error::Error for TokenizeError {}

// --- encoding detection (tokenize.detect_encoding) ---------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Codec {
    Utf8,
    Iso8859_1,
    Koi8R,
    Ascii,
    Cp1252,
    Iso8859_15,
}

/// `codecs.lookup` for the codecs this tokenizer carries: the name is
/// lowercased and normalised as `encodings.normalize_encoding` does (runs of
/// characters other than ASCII alphanumerics and `.` become one `_`, trimmed
/// at both ends), then resolved through the aliases in `encodings/aliases.py`.
fn lookup_codec(name: &str) -> Option<Codec> {
    let mut norm = String::new();
    let mut pending = false;
    for c in name.chars().map(|c| c.to_ascii_lowercase()) {
        if c.is_ascii_alphanumeric() || c == '.' {
            if pending && !norm.is_empty() {
                norm.push('_');
            }
            pending = false;
            norm.push(c);
        } else {
            pending = true;
        }
    }
    let resolve = |n: &str| -> Option<Codec> {
        Some(match n {
            "utf_8" | "utf8" | "u8" | "utf" | "utf8_ucs2" | "utf8_ucs4" | "cp65001"
            | "utf_8_sig" => Codec::Utf8,
            "latin_1" | "iso8859_1" | "iso_8859_1" | "8859" | "cp819" | "csisolatin1"
            | "ibm819" | "iso8859" | "iso_8859_1_1987" | "iso_ir_100" | "l1" | "latin"
            | "latin1" => Codec::Iso8859_1,
            "ascii" | "646" | "ansi_x3.4_1968" | "ansi_x3_4_1968" | "ansi_x3.4_1986" | "cp367"
            | "csascii" | "ibm367" | "iso646_us" | "iso_646.irv_1991" | "iso_ir_6" | "us"
            | "us_ascii" => Codec::Ascii,
            "cp1252" | "windows_1252" => Codec::Cp1252,
            "iso8859_15" | "iso_8859_15" | "l9" | "latin9" => Codec::Iso8859_15,
            "koi8_r" | "cskoi8r" => Codec::Koi8R,
            _ => return None,
        })
    };
    resolve(&norm).or_else(|| resolve(&norm.replace('.', "_")))
}

/// cp1252's 0x80..=0x9F; `None` is undefined (decoded as U+FFFD).
const CP1252_HIGH: [Option<char>; 32] = [
    Some('\u{20AC}'),
    None,
    Some('\u{201A}'),
    Some('\u{0192}'),
    Some('\u{201E}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02C6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017D}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201C}'),
    Some('\u{201D}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02DC}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203A}'),
    Some('\u{0153}'),
    None,
    Some('\u{017E}'),
    Some('\u{0178}'),
];

struct Detection {
    /// The string carried by the tokenize view's `ENCODING` token.
    encoding_token: String,
    /// The source lines the core consumes: UTF-8 bytes each, BOM stripped,
    /// decoded through the detected codec with replacement.
    lines: Vec<Vec<u8>>,
}

const BOM_UTF8: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// One `readline` step: the bytes up to and including the next `\n`, or the
/// rest of the input; `None` at end of input.
fn read_line<'a>(bytes: &'a [u8], pos: &mut usize) -> Option<&'a [u8]> {
    if *pos >= bytes.len() {
        return None;
    }
    let rest = &bytes[*pos..];
    let end = rest
        .iter()
        .position(|&b| b == b'\n')
        .map_or(rest.len(), |i| i + 1);
    *pos += end;
    Some(&rest[..end])
}

/// `tokenize._get_normal_name`.
fn get_normal_name(orig: &str) -> String {
    let normalized: String = orig
        .chars()
        .take(12)
        .map(|c| {
            if c == '_' {
                '-'
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    if normalized == "utf-8" || normalized.starts_with("utf-8-") {
        "utf-8".to_string()
    } else if matches!(
        normalized.as_str(),
        "latin-1" | "iso-8859-1" | "iso-latin-1"
    ) || normalized.starts_with("latin-1-")
        || normalized.starts_with("iso-8859-1-")
        || normalized.starts_with("iso-latin-1-")
    {
        "iso-8859-1".to_string()
    } else {
        orig.to_string()
    }
}

fn detect_encoding(bytes: &[u8]) -> Result<Detection, TokenizeError> {
    let mut pos = 0;
    let first_raw = match read_line(bytes, &mut pos) {
        None => {
            // An empty file: no lines ever reach the core, and the ENCODING
            // token still names the default.
            return Ok(Detection {
                encoding_token: "utf-8".to_string(),
                lines: Vec::new(),
            });
        }
        Some(line) => line,
    };
    let had_bom = first_raw.starts_with(&BOM_UTF8);
    let first = if had_bom { &first_raw[3..] } else { first_raw };
    let body = if had_bom { &bytes[3..] } else { bytes };

    let plain = || Detection {
        encoding_token: "utf-8".to_string(),
        lines: decode_lines(body, Codec::Utf8),
    };

    let cookie_encoding = |encoding: String| -> Result<Detection, TokenizeError> {
        if had_bom && encoding != "utf-8" {
            return Err(TokenizeError {
                kind: TokenizeErrorKind::SyntaxError,
                message: "encoding problem: utf-8".to_string(),
                line: None,
                column: None,
            });
        }
        // The BOM wins over an explicit utf-8 cookie, exactly as in
        // detect_encoding.
        if had_bom && encoding == "utf-8" {
            return Ok(plain());
        }
        let codec = match lookup_codec(&encoding) {
            Some(codec) => codec,
            None => {
                let other = encoding.as_str();
                // CPython raises this for any codec codecs.lookup rejects; a
                // codec CPython knows but this tokenizer does not carry
                // reports the same typed error naming the codec.
                return Err(TokenizeError {
                    kind: TokenizeErrorKind::UnsupportedEncoding,
                    message: format!("unknown encoding: {other}"),
                    line: None,
                    column: None,
                });
            }
        };
        let lines = decode_lines(body, codec);
        // tokenize() renames "utf-8-sig" to "utf-8" for the ENCODING token.
        let encoding_token = if encoding == "utf-8-sig" {
            "utf-8".to_string()
        } else {
            encoding
        };
        Ok(Detection {
            encoding_token,
            lines,
        })
    };

    if let Some(encoding) = find_cookie(first)? {
        return cookie_encoding(encoding);
    }

    // `blank_re`: `^[ \t\f]*(?:[#\r\n]|$)`
    let mut i = 0;
    while i < first.len() && matches!(first[i], b' ' | b'\t' | 0x0C) {
        i += 1;
    }
    let blank = i >= first.len() || matches!(first[i], b'#' | b'\r' | b'\n');
    if !blank {
        return Ok(plain());
    }

    let second = match read_line(bytes, &mut pos) {
        None => return Ok(plain()),
        Some(line) => line,
    };
    if let Some(encoding) = find_cookie(second)? {
        return cookie_encoding(encoding);
    }
    Ok(plain())
}

/// The `^[ \t\f]*#.*?coding[:=][ \t]*([-\w.]+)` cookie match (ASCII classes);
/// `.` in the regex excludes newlines, so the name never crosses the line's
/// terminator. The line is decoded strictly first: a line that is not valid
/// UTF-8 cannot carry a cookie and is the "invalid or missing encoding
/// declaration" error, like find_cookie.
fn find_cookie(line: &[u8]) -> Result<Option<String>, TokenizeError> {
    let invalid = || TokenizeError {
        kind: TokenizeErrorKind::SyntaxError,
        message: "invalid or missing encoding declaration".to_string(),
        line: None,
        column: None,
    };
    let text = match std::str::from_utf8(line) {
        Ok(text) => text,
        Err(_) => return Err(invalid()),
    };
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | 0x0C) {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'#' {
        return Ok(None);
    }
    let mut p = i + 1;
    while p + 6 <= bytes.len() {
        if &bytes[p..p + 6] == b"coding" {
            let mut q = p + 6;
            if q < bytes.len() && (bytes[q] == b':' || bytes[q] == b'=') {
                q += 1;
                while q < bytes.len() && (bytes[q] == b' ' || bytes[q] == b'\t') {
                    q += 1;
                }
                let begin = q;
                while q < bytes.len()
                    && (bytes[q].is_ascii_alphanumeric() || matches!(bytes[q], b'-' | b'_' | b'.'))
                {
                    q += 1;
                }
                if q > begin {
                    return Ok(Some(get_normal_name(&text[begin..q])));
                }
            }
        }
        p += 1;
    }
    Ok(None)
}

fn decode_lines(body: &[u8], codec: Codec) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    let mut pos = 0;
    while let Some(line) = read_line(body, &mut pos) {
        let decoded: String = match codec {
            Codec::Utf8 => String::from_utf8_lossy(line).into_owned(),
            Codec::Iso8859_1 => line.iter().map(|&b| b as char).collect(),
            Codec::Koi8R => line
                .iter()
                .map(|&b| {
                    if b < 128 {
                        b as char
                    } else {
                        KOI8_R[b as usize]
                    }
                })
                .collect(),
            // tokenize decodes with errors="replace": undecodable bytes become U+FFFD.
            Codec::Ascii => line
                .iter()
                .map(|&b| if b < 128 { b as char } else { '\u{FFFD}' })
                .collect(),
            Codec::Cp1252 => line
                .iter()
                .map(|&b| match b {
                    0x80..=0x9F => CP1252_HIGH[(b - 0x80) as usize].unwrap_or('\u{FFFD}'),
                    _ => b as char,
                })
                .collect(),
            Codec::Iso8859_15 => line
                .iter()
                .map(|&b| match b {
                    0xA4 => '\u{20AC}',
                    0xA6 => '\u{0160}',
                    0xA8 => '\u{0161}',
                    0xB4 => '\u{017D}',
                    0xB8 => '\u{017E}',
                    0xBC => '\u{0152}',
                    0xBD => '\u{0153}',
                    0xBE => '\u{0178}',
                    _ => b as char,
                })
                .collect(),
        };
        // The C tokenizer re-encodes each decoded line to UTF-8.
        lines.push(decoded.into_bytes());
    }
    lines
}

// --- the core tokenizer (Parser/tokenizer.c, readline input mode) ------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Done {
    Ok,
    Eof,
    Error,
    TabSpace,
    Toodeep,
    Dedent,
    LineCont,
    ColumnOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModeKind {
    Regular,
    FString,
}

#[derive(Debug, Clone, Copy)]
struct Mode {
    kind: ModeKind,
    curly_bracket_depth: i32,
    curly_bracket_expr_start_depth: i32,
    quote: u8,
    quote_size: i32,
    raw: bool,
    f_string_start: usize,
    f_string_multi_line_start: usize,
    f_string_line_start: u32,
    f_string_debug: bool,
    in_format_spec: bool,
}

impl Mode {
    const fn new() -> Self {
        Mode {
            kind: ModeKind::Regular,
            curly_bracket_depth: 0,
            curly_bracket_expr_start_depth: -1,
            quote: 0,
            quote_size: 0,
            raw: false,
            f_string_start: 0,
            f_string_multi_line_start: 0,
            f_string_line_start: 0,
            f_string_debug: false,
            in_format_spec: false,
        }
    }
}

/// A token as produced by the core: kind plus byte offsets into the buffer;
/// `None` for tokens the C tokenizer emits without a buffer extent.
#[derive(Debug, Clone, Copy)]
struct RawToken {
    kind: TokenKind,
    start: Option<usize>,
    end: Option<usize>,
}

impl RawToken {
    const ERROR: RawToken = RawToken {
        kind: TokenKind::ErrorToken,
        start: None,
        end: None,
    };
}

/// The Python-level error the C tokenizer raises through PyErr.
#[derive(Debug, Clone)]
struct SyntaxErr {
    message: String,
    line: u32,
    column: i64,
}

pub struct Tokenizer {
    // Input buffer: `buf[..inp]` holds the current line(s); `cur` is the next
    // byte. The buffer only resets to empty when no token is in flight and
    // no f-string is open, like `tok_underflow_readline`.
    buf: Vec<u8>,
    cur: usize,
    inp: usize,
    line_start: usize,
    multi_line_start: usize,
    start: Option<usize>,
    done: Done,
    err: Option<SyntaxErr>,

    lineno: u32,
    first_lineno: u32,
    col_offset: i64,
    starting_col_offset: i64,

    atbol: bool,
    pendin: i32,
    indent: usize,
    indstack: [i64; MAXINDENT],
    altindstack: [i64; MAXINDENT],
    level: i32,
    parenstack: Vec<u8>,
    parenlineno: Vec<u32>,

    tok_extra_tokens: bool,
    type_comments: bool,
    comment_newline: bool,
    implicit_newline: bool,

    modes: [Mode; MAXFSTRINGLEVEL],
    mode_index: usize,

    lines: std::vec::IntoIter<Vec<u8>>,
}

fn is_potential_identifier_start(c: i32) -> bool {
    is_lower(c) || is_upper(c) || c == b'_' as i32 || c >= 128
}

fn is_potential_identifier_char(c: i32) -> bool {
    is_potential_identifier_start(c) || is_digit(c)
}

fn is_lower(c: i32) -> bool {
    (b'a' as i32..=b'z' as i32).contains(&c)
}

fn is_upper(c: i32) -> bool {
    (b'A' as i32..=b'Z' as i32).contains(&c)
}

fn is_digit(c: i32) -> bool {
    (b'0' as i32..=b'9' as i32).contains(&c)
}

fn is_xdigit(c: i32) -> bool {
    is_digit(c)
        || (b'a' as i32..=b'f' as i32).contains(&c)
        || (b'A' as i32..=b'F' as i32).contains(&c)
}

/// `Py_UNICODE_ISPRINTABLE` restricted to what can reach the tokenizer's
/// checks: single bytes. Every byte >= 0xA0 (and every UTF-8 lead byte) maps
/// to a printable code point; the non-printables are the C0/C1 controls,
/// U+00A0 and U+00AD.
fn is_printable(c: i32) -> bool {
    let Ok(b) = u8::try_from(c) else { return false };
    !matches!(b, 0x00..=0x1F | 0x7F..=0x9F | 0xA0 | 0xAD)
}

fn is_printable_char(c: char) -> bool {
    let cp = c as u32;
    cp >= 0x100 || is_printable(cp as i32)
}

fn xid_start(c: char) -> bool {
    c == '_' || in_ranges(c as u32, XID_START)
}

fn xid_continue(c: char) -> bool {
    in_ranges(c as u32, XID_CONTINUE)
}

fn in_ranges(cp: u32, ranges: &[(u32, u32)]) -> bool {
    ranges
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

impl Tokenizer {
    fn new(lines: Vec<Vec<u8>>, tok_extra_tokens: bool, type_comments: bool) -> Self {
        Tokenizer {
            buf: Vec::new(),
            cur: 0,
            inp: 0,
            line_start: 0,
            multi_line_start: 0,
            start: None,
            done: Done::Ok,
            err: None,
            lineno: 0,
            first_lineno: 0,
            col_offset: -1,
            starting_col_offset: -1,
            atbol: true,
            pendin: 0,
            indent: 0,
            indstack: [0; MAXINDENT],
            altindstack: [0; MAXINDENT],
            level: 0,
            parenstack: Vec::new(),
            parenlineno: Vec::new(),
            tok_extra_tokens,
            type_comments,
            comment_newline: false,
            implicit_newline: false,
            modes: [Mode::new(); MAXFSTRINGLEVEL],
            mode_index: 0,
            lines: lines.into_iter(),
        }
    }

    fn mode(&self) -> &Mode {
        &self.modes[self.mode_index]
    }

    fn mode_mut(&mut self) -> &mut Mode {
        &mut self.modes[self.mode_index]
    }

    /// The next token; `TokenKind::ErrorToken` means failure, after which the
    /// caller reads `finish_error()`.
    fn next_token(&mut self) -> RawToken {
        if self.mode().kind == ModeKind::Regular {
            self.normal_mode()
        } else {
            self.fstring_mode()
        }
    }

    // -- input ---------------------------------------------------------------

    /// `tok_underflow_readline` + `tok_readline_string`.
    fn underflow(&mut self) -> bool {
        if self.start.is_none() && self.mode_index == 0 {
            self.cur = 0;
            self.inp = 0;
            self.buf.clear();
        }
        let line = match self.lines.next() {
            Some(line) => line,
            None => {
                self.done = Done::Eof;
                return false;
            }
        };
        self.buf.extend_from_slice(&line);
        self.inp = self.buf.len();
        self.line_start = self.cur;
        if self.inp == self.cur {
            self.done = Done::Eof;
            return false;
        }
        self.implicit_newline = false;
        if self.buf[self.inp - 1] != b'\n' {
            // Last line does not end in \n; fake one.
            self.buf.push(b'\n');
            self.inp = self.buf.len();
            self.implicit_newline = true;
        }
        self.lineno += 1;
        self.col_offset = 0;
        true
    }

    fn nextc(&mut self) -> i32 {
        loop {
            if self.cur != self.inp {
                if self.col_offset >= i32::MAX as i64 {
                    self.done = Done::ColumnOverflow;
                    return EOF;
                }
                self.col_offset += 1;
                let c = self.buf[self.cur];
                self.cur += 1;
                return c as i32;
            }
            if self.done != Done::Ok {
                return EOF;
            }
            if !self.underflow() {
                self.cur = self.inp;
                return EOF;
            }
            if self.buf[self.line_start..self.inp].contains(&0) {
                self.syntaxerror("source code cannot contain null bytes".to_string());
                self.cur = self.inp;
                return EOF;
            }
        }
    }

    fn backup(&mut self, c: i32) {
        if c != EOF {
            self.cur -= 1;
            self.col_offset -= 1;
        }
    }

    fn buf_char(&self, offset: usize) -> i32 {
        if offset < self.buf.len() {
            self.buf[offset] as i32
        } else {
            EOF
        }
    }

    // -- errors ---------------------------------------------------------------

    /// `_syntaxerror_range`: record the error and its location, the current
    /// line decoded up to `cur`, in code points.
    fn syntaxerror(&mut self, message: String) -> RawToken {
        if self.done == Done::Error {
            return RawToken::ERROR;
        }
        let upto = self.cur.saturating_sub(self.line_start);
        let prefix = String::from_utf8_lossy(&self.buf[self.line_start..self.line_start + upto]);
        self.err = Some(SyntaxErr {
            message,
            line: self.lineno,
            column: prefix.chars().count() as i64,
        });
        self.done = Done::Error;
        RawToken::ERROR
    }

    /// Maps the failure state to the exception `tokenize` would raise.
    fn finish_error(&self) -> TokenizeError {
        if let Some(err) = &self.err {
            // A SyntaxError raised by the tokenizer itself becomes a
            // TokenError, per _generate_tokens_from_c_tokenizer.
            return TokenizeError {
                kind: TokenizeErrorKind::TokenError,
                message: transform_msg(&err.message),
                line: Some(err.line),
                column: Some(err.column),
            };
        }
        if self.done == Done::Eof {
            return TokenizeError {
                kind: TokenizeErrorKind::TokenError,
                message: "unexpected EOF in multi-line statement".to_string(),
                line: Some(self.lineno),
                // E_EOF goes through PyErr_SyntaxLocationObject, which
                // carries inp - buf as-is.
                column: Some(self.inp as i64),
            };
        }
        let (kind, message) = match self.done {
            Done::TabSpace => (
                TokenizeErrorKind::TabError,
                "inconsistent use of tabs and spaces in indentation",
            ),
            Done::Toodeep => (
                TokenizeErrorKind::IndentationError,
                "too many levels of indentation",
            ),
            Done::Dedent => (
                TokenizeErrorKind::IndentationError,
                "unindent does not match any outer indentation level",
            ),
            Done::LineCont => (
                TokenizeErrorKind::TokenError,
                "unexpected character after line continuation character",
            ),
            _ => (TokenizeErrorKind::TokenError, "unknown tokenization error"),
        };
        TokenizeError {
            kind,
            message: message.to_string(),
            line: Some(self.lineno),
            // The built SyntaxError carries the character offset of inp
            // within the current line, its newline excluded.
            column: Some(self.error_offset()),
        }
    }

    /// `_PyPegen_byte_offset_to_character_offset` over `buf[..inp - 1]` at
    /// byte offset `inp`: the error position of the non-EOF failures.
    fn error_offset(&self) -> i64 {
        let line_len = self.inp.saturating_sub(1).min(self.buf.len());
        count_chars_between(&self.buf[..line_len], 0, self.inp)
    }

    // -- helpers over the input ------------------------------------------------

    /// `lookahead`: does the input at the cursor continue with `test` and
    /// then stop being an identifier character?
    fn lookahead(&mut self, test: &[u8]) -> bool {
        let mut matched = 0;
        loop {
            let c = self.nextc();
            if matched == test.len() {
                let res = !is_potential_identifier_char(c);
                self.backup(c);
                while matched > 0 {
                    matched -= 1;
                    self.backup(test[matched] as i32);
                }
                return res;
            }
            if c != test[matched] as i32 {
                self.backup(c);
                while matched > 0 {
                    matched -= 1;
                    self.backup(test[matched] as i32);
                }
                return false;
            }
            matched += 1;
        }
    }

    /// `verify_end_of_number`. The keyword branch only emits a warning in
    /// CPython; under the default warning filters that leaves the token
    /// stream alone, so this port keeps the stream and drops the warning.
    fn verify_end_of_number(&mut self, c: i32, kind: &str) -> bool {
        if self.tok_extra_tokens {
            return true;
        }
        let keyword = match u8::try_from(c) {
            Ok(b'a') => self.lookahead(b"nd"),
            Ok(b'e') => self.lookahead(b"lse"),
            Ok(b'f') => self.lookahead(b"or"),
            Ok(b'i') => {
                let c2 = self.nextc();
                let hit = c2 == b'f' as i32 || c2 == b'n' as i32 || c2 == b's' as i32;
                self.backup(c2);
                hit
            }
            Ok(b'o') => self.lookahead(b"r"),
            Ok(b'n') => self.lookahead(b"ot"),
            _ => false,
        };
        if keyword {
            self.backup(c);
            self.nextc();
        } else if c < 128 && is_potential_identifier_char(c) {
            self.backup(c);
            self.syntaxerror(format!("invalid {kind} literal"));
            return false;
        }
        true
    }

    /// `verify_identifier` (PEP 3131); only runs with `tok_extra_tokens` off,
    /// i.e. in the parser view.
    fn verify_identifier(&mut self) -> bool {
        if self.tok_extra_tokens {
            return true;
        }
        let (Some(start), Some(end)) = (self.start, Some(self.cur)) else {
            return true;
        };
        let text = match std::str::from_utf8(&self.buf[start..end]) {
            Ok(text) => text,
            Err(_) => {
                // The buffer is re-encoded UTF-8, so this cannot happen; the
                // C tokenizer would set E_DECODE here.
                self.done = Done::Error;
                return false;
            }
        };
        let mut chars = text.char_indices();
        let invalid = match chars.next() {
            None => 0,
            Some((_, first)) => {
                if !xid_start(first) {
                    0
                } else {
                    let mut bad = text.chars().count();
                    for (idx, ch) in chars {
                        if !xid_continue(ch) {
                            bad = text[..idx].chars().count();
                            break;
                        }
                    }
                    bad
                }
            }
        };
        let total = text.chars().count();
        if invalid < total {
            let bad_char = text.chars().nth(invalid).unwrap_or('\u{FFFD}');
            if invalid + 1 < total {
                // Point `cur` just past the invalid character.
                let consumed = text
                    .char_indices()
                    .nth(invalid + 1)
                    .map_or(text.len(), |(idx, _)| idx);
                self.cur = start + consumed;
            }
            let message = if is_printable_char(bad_char) {
                format!("invalid character {bad_char} (U+{:04X})", bad_char as u32)
            } else {
                format!("invalid non-printable character U+{:04X}", bad_char as u32)
            };
            self.syntaxerror(message);
            return false;
        }
        true
    }

    /// `tok_decimal_tail`; `None` after an error.
    fn decimal_tail(&mut self) -> Option<i32> {
        loop {
            let mut c = self.nextc();
            while is_digit(c) {
                c = self.nextc();
            }
            if c != b'_' as i32 {
                return Some(c);
            }
            c = self.nextc();
            if !is_digit(c) {
                self.backup(c);
                self.syntaxerror("invalid decimal literal".to_string());
                return None;
            }
        }
    }

    /// `tok_continuation_line`; `Err` reports through `done`.
    fn continuation_line(&mut self) -> Result<i32, ()> {
        let mut c = self.nextc();
        if c == b'\r' as i32 {
            c = self.nextc();
        }
        if c != b'\n' as i32 {
            self.done = Done::LineCont;
            return Err(());
        }
        let c = self.nextc();
        if c == EOF {
            self.done = Done::Eof;
            self.cur = self.inp;
            return Err(());
        }
        self.backup(c);
        Ok(c)
    }

    // -- normal mode ------------------------------------------------------------

    fn indenterror(&mut self) -> RawToken {
        self.done = Done::TabSpace;
        self.cur = self.inp;
        RawToken::ERROR
    }

    fn normal_mode(&mut self) -> RawToken {
        let mut p_start: Option<usize> = None;
        let mut p_end: Option<usize> = None;
        'nextline: loop {
            self.start = None;
            self.starting_col_offset = -1;
            let mut blankline = false;

            if self.atbol {
                let mut col: i64 = 0;
                let mut altcol: i64 = 0;
                self.atbol = false;
                let mut cont_line_col: i64 = 0;
                let c;
                loop {
                    let ch = self.nextc();
                    if ch == b' ' as i32 {
                        col += 1;
                        altcol += 1;
                    } else if ch == b'\t' as i32 {
                        col = (col / TABSIZE + 1) * TABSIZE;
                        altcol = (altcol / ALTTABSIZE + 1) * ALTTABSIZE;
                    } else if ch == 0x0C {
                        col = 0;
                        altcol = 0;
                    } else if ch == b'\\' as i32 {
                        // The first backslash in the indentation fixes the
                        // column for whatever follows the continuation.
                        if cont_line_col == 0 {
                            cont_line_col = col;
                        }
                        match self.continuation_line() {
                            Ok(_) => continue,
                            Err(()) => return RawToken::ERROR,
                        }
                    } else {
                        c = ch;
                        break;
                    }
                }
                self.backup(c);
                if c == b'#' as i32 || c == b'\n' as i32 || c == b'\r' as i32 {
                    blankline = true;
                }
                if !blankline && self.level == 0 {
                    let col = if cont_line_col != 0 {
                        cont_line_col
                    } else {
                        col
                    };
                    let altcol = if cont_line_col != 0 {
                        cont_line_col
                    } else {
                        altcol
                    };
                    if col == self.indstack[self.indent] {
                        if altcol != self.altindstack[self.indent] {
                            return self.indenterror();
                        }
                    } else if col > self.indstack[self.indent] {
                        if self.indent + 1 >= MAXINDENT {
                            self.done = Done::Toodeep;
                            self.cur = self.inp;
                            return RawToken::ERROR;
                        }
                        if altcol <= self.altindstack[self.indent] {
                            return self.indenterror();
                        }
                        self.pendin += 1;
                        self.indent += 1;
                        self.indstack[self.indent] = col;
                        self.altindstack[self.indent] = altcol;
                    } else {
                        while self.indent > 0 && col < self.indstack[self.indent] {
                            self.pendin -= 1;
                            self.indent -= 1;
                        }
                        if col != self.indstack[self.indent] {
                            self.done = Done::Dedent;
                            self.cur = self.inp;
                            return RawToken::ERROR;
                        }
                        if altcol != self.altindstack[self.indent] {
                            return self.indenterror();
                        }
                    }
                }
            }

            self.start = Some(self.cur);
            self.starting_col_offset = self.col_offset;

            if self.pendin != 0 {
                if self.pendin < 0 {
                    if self.tok_extra_tokens {
                        p_start = Some(self.cur);
                        p_end = Some(self.cur);
                    }
                    self.pendin += 1;
                    return RawToken {
                        kind: TokenKind::Dedent,
                        start: p_start,
                        end: p_end,
                    };
                }
                if self.tok_extra_tokens {
                    p_start = Some(0);
                    p_end = Some(self.cur);
                }
                self.pendin -= 1;
                return RawToken {
                    kind: TokenKind::Indent,
                    start: p_start,
                    end: p_end,
                };
            }

            'again: loop {
                self.start = None;
                let mut c = self.nextc();
                while c == b' ' as i32 || c == b'\t' as i32 || c == 0x0C {
                    c = self.nextc();
                }

                // At a reset EOF the C tokenizer computes buf-1 here, a
                // pointer it never dereferences; the token it produces
                // carries a NULL start, which `None` mirrors.
                self.start = self.cur.checked_sub(1);
                self.starting_col_offset = self.col_offset - 1;

                if c == b'#' as i32 {
                    while c != EOF && c != b'\n' as i32 && c != b'\r' as i32 {
                        c = self.nextc();
                    }
                    let mut p = self.tok_extra_tokens.then_some(self.start.unwrap());
                    if self.type_comments {
                        match self.type_comment(c, blankline) {
                            TypeComment::Ignore(start, end) => {
                                return RawToken {
                                    kind: TokenKind::TypeIgnore,
                                    start: Some(start),
                                    end: Some(end),
                                };
                            }
                            TypeComment::Comment(start, end) => {
                                return RawToken {
                                    kind: TokenKind::TypeComment,
                                    start: Some(start),
                                    end: Some(end),
                                };
                            }
                            TypeComment::NotType(next_p) => p = Some(next_p),
                        }
                    }
                    if self.tok_extra_tokens {
                        self.backup(c);
                        self.comment_newline = blankline;
                        return RawToken {
                            kind: TokenKind::Comment,
                            start: p,
                            end: Some(self.cur),
                        };
                    }
                }

                if c == EOF {
                    if self.level != 0 {
                        return RawToken::ERROR;
                    }
                    return RawToken {
                        kind: if self.done == Done::Eof {
                            TokenKind::Endmarker
                        } else {
                            TokenKind::ErrorToken
                        },
                        start: None,
                        end: None,
                    };
                }

                // Identifier or string prefix.
                let mut nonascii = false;
                if is_potential_identifier_start(c) {
                    let (mut saw_b, mut saw_r, mut saw_u, mut saw_f) = (false, false, false, false);
                    loop {
                        if !saw_b && !saw_u && !saw_f && (c == b'b' as i32 || c == b'B' as i32) {
                            saw_b = true;
                        } else if !saw_b
                            && !saw_u
                            && !saw_r
                            && !saw_f
                            && (c == b'u' as i32 || c == b'U' as i32)
                        {
                            saw_u = true;
                        } else if !saw_r && !saw_u && (c == b'r' as i32 || c == b'R' as i32) {
                            saw_r = true;
                        } else if !saw_f
                            && !saw_b
                            && !saw_u
                            && (c == b'f' as i32 || c == b'F' as i32)
                        {
                            saw_f = true;
                        } else {
                            break;
                        }
                        c = self.nextc();
                        if c == b'"' as i32 || c == b'\'' as i32 {
                            if saw_f {
                                return self.f_string_quote(c);
                            }
                            return self.letter_quote(c);
                        }
                    }
                    while is_potential_identifier_char(c) {
                        if c >= 128 {
                            nonascii = true;
                        }
                        c = self.nextc();
                    }
                    self.backup(c);
                    if nonascii && !self.verify_identifier() {
                        return RawToken::ERROR;
                    }
                    p_start = self.start;
                    p_end = Some(self.cur);
                    // async/await are unconditional keywords (3.7+).
                    if p_end.unwrap() - p_start.unwrap() == 5 && self.buf[p_start.unwrap()] == b'a'
                    {
                        if &self.buf[p_start.unwrap()..p_end.unwrap()] == b"async" {
                            return RawToken {
                                kind: TokenKind::Async,
                                start: p_start,
                                end: p_end,
                            };
                        }
                        if &self.buf[p_start.unwrap()..p_end.unwrap()] == b"await" {
                            return RawToken {
                                kind: TokenKind::Await,
                                start: p_start,
                                end: p_end,
                            };
                        }
                    }
                    return RawToken {
                        kind: TokenKind::Name,
                        start: p_start,
                        end: p_end,
                    };
                }

                if c == b'\r' as i32 {
                    c = self.nextc();
                }
                if c == b'\n' as i32 {
                    self.atbol = true;
                    if blankline || self.level > 0 {
                        if self.tok_extra_tokens {
                            if self.comment_newline {
                                self.comment_newline = false;
                            }
                            return RawToken {
                                kind: TokenKind::Nl,
                                start: self.start,
                                end: Some(self.cur),
                            };
                        }
                        continue 'nextline;
                    }
                    if self.comment_newline && self.tok_extra_tokens {
                        self.comment_newline = false;
                        return RawToken {
                            kind: TokenKind::Nl,
                            start: self.start,
                            end: Some(self.cur),
                        };
                    }
                    return RawToken {
                        kind: TokenKind::Newline,
                        start: self.start,
                        end: Some(self.cur - 1),
                    };
                }

                if c == b'.' as i32 {
                    c = self.nextc();
                    if is_digit(c) {
                        return match self.number_chain(ChainStart::Fraction, c) {
                            Ok(None) => RawToken {
                                kind: TokenKind::Number,
                                start: self.start,
                                end: Some(self.cur),
                            },
                            Ok(Some(term)) => {
                                self.backup(term);
                                RawToken {
                                    kind: TokenKind::Number,
                                    start: self.start,
                                    end: Some(self.cur),
                                }
                            }
                            Err(token) => token,
                        };
                    }
                    if c == b'.' as i32 {
                        let c2 = self.nextc();
                        if c2 == b'.' as i32 {
                            return RawToken {
                                kind: TokenKind::Ellipsis,
                                start: self.start,
                                end: Some(self.cur),
                            };
                        }
                        self.backup(c2);
                    }
                    self.backup(c);
                    return RawToken {
                        kind: TokenKind::Dot,
                        start: self.start,
                        end: Some(self.cur),
                    };
                }

                if is_digit(c) {
                    return match self.number(c) {
                        Ok(()) => RawToken {
                            kind: TokenKind::Number,
                            start: self.start,
                            end: Some(self.cur),
                        },
                        Err(token) => token,
                    };
                }

                // A bare quote reaches letter_quote by falling through the
                // number block; prefixed quotes jumped there directly.
                if c == b'\'' as i32 || c == b'"' as i32 {
                    return self.letter_quote(c);
                }

                if c == b'\\' as i32 {
                    match self.continuation_line() {
                        Ok(_) => continue 'again,
                        Err(()) => return RawToken::ERROR,
                    }
                }

                // Punctuation special to f-string expressions.
                let is_punctuation =
                    c == b':' as i32 || c == b'}' as i32 || c == b'!' as i32 || c == b'{' as i32;
                if is_punctuation
                    && self.mode_index > 0
                    && self.mode().curly_bracket_expr_start_depth >= 0
                {
                    // The depth is decremented manually for non-'{' because
                    // the '{' case below has not run yet.
                    let mode = *self.mode();
                    let cursor = mode.curly_bracket_depth - i32::from(c != b'{' as i32);
                    if c == b':' as i32 && cursor == mode.curly_bracket_expr_start_depth {
                        self.mode_mut().kind = ModeKind::FString;
                        self.mode_mut().in_format_spec = true;
                        return RawToken {
                            kind: one_char(c),
                            start: self.start,
                            end: Some(self.cur),
                        };
                    }
                }

                let c2 = self.nextc();
                let mut current = two_chars(c, c2);
                if current != TokenKind::Op {
                    let c3 = self.nextc();
                    let current3 = three_chars(c, c2, c3);
                    if current3 != TokenKind::Op {
                        current = current3;
                    } else {
                        self.backup(c3);
                    }
                    return RawToken {
                        kind: current,
                        start: self.start,
                        end: Some(self.cur),
                    };
                }
                self.backup(c2);

                if let Ok(b) = u8::try_from(c) {
                    match b {
                        b'(' | b'[' | b'{' => {
                            if self.level >= MAXLEVEL {
                                return self.syntaxerror("too many nested parentheses".to_string());
                            }
                            self.parenstack.push(b);
                            self.parenlineno.push(self.lineno);
                            self.level += 1;
                            if self.mode_index > 0 {
                                self.mode_mut().curly_bracket_depth += 1;
                            }
                        }
                        b')' | b']' | b'}' => {
                            if self.mode_index > 0
                                && self.mode().curly_bracket_depth == 0
                                && b == b'}'
                            {
                                return self.syntaxerror(
                                    "f-string: single '}' is not allowed".to_string(),
                                );
                            }
                            if !self.tok_extra_tokens && self.level == 0 {
                                return self.syntaxerror(format!("unmatched '{}'", b as char));
                            }
                            if self.level > 0 {
                                self.level -= 1;
                                let opening = self.parenstack.pop().unwrap_or(b'(');
                                let opening_line = self.parenlineno.pop().unwrap_or(self.lineno);
                                if !self.tok_extra_tokens
                                    && !matches!(
                                        (opening, b),
                                        (b'(', b')') | (b'[', b']') | (b'{', b'}')
                                    )
                                {
                                    // If the opening bracket belongs to an
                                    // f-string's expression part, prefer the
                                    // f-string error.
                                    if self.mode_index > 0 && opening == b'{' {
                                        let previous_bracket = self.mode().curly_bracket_depth - 1;
                                        if previous_bracket
                                            == self.mode().curly_bracket_expr_start_depth
                                        {
                                            return self.syntaxerror(format!(
                                                "f-string: unmatched '{}'",
                                                b as char
                                            ));
                                        }
                                    }
                                    if opening_line != self.lineno {
                                        return self.syntaxerror(format!(
                                            "closing parenthesis '{}' does not match opening parenthesis '{}' on line {}",
                                            b as char, opening as char, opening_line
                                        ));
                                    }
                                    return self.syntaxerror(format!(
                                        "closing parenthesis '{}' does not match opening parenthesis '{}'",
                                        b as char, opening as char
                                    ));
                                }
                            }
                            if self.mode_index > 0 {
                                let mode = self.mode_mut();
                                mode.curly_bracket_depth -= 1;
                                if mode.curly_bracket_depth < 0 {
                                    return self.syntaxerror(format!(
                                        "f-string: unmatched '{}'",
                                        b as char
                                    ));
                                }
                                if b == b'}'
                                    && mode.curly_bracket_depth
                                        == mode.curly_bracket_expr_start_depth
                                {
                                    mode.curly_bracket_expr_start_depth -= 1;
                                    mode.kind = ModeKind::FString;
                                    mode.in_format_spec = false;
                                    mode.f_string_debug = false;
                                }
                            }
                        }
                        _ => {}
                    }
                }

                if !is_printable(c) {
                    return self
                        .syntaxerror(format!("invalid non-printable character U+{:04X}", c));
                }

                if c == b'=' as i32
                    && self.mode_index > 0
                    && self.mode().curly_bracket_expr_start_depth >= 0
                {
                    self.mode_mut().f_string_debug = true;
                }

                return RawToken {
                    kind: one_char(c),
                    start: self.start,
                    end: Some(self.cur),
                };
            }
        }
    }

    /// The comment-or-type-comment handling of `tok_get_normal_mode`.
    fn type_comment(&mut self, c: i32, blankline: bool) -> TypeComment {
        let mut p = self.start.unwrap();
        let mut current_starting_col_offset = self.starting_col_offset;
        let mut matched = 0;
        while matched < TYPE_COMMENT_PREFIX.len() && p < self.cur {
            let want = TYPE_COMMENT_PREFIX[matched];
            if want == b' ' {
                while p < self.cur
                    && (self.buf_char(p) == b' ' as i32 || self.buf_char(p) == b'\t' as i32)
                {
                    p += 1;
                    current_starting_col_offset += 1;
                }
            } else if self.buf_char(p) == want as i32 {
                p += 1;
                current_starting_col_offset += 1;
            } else {
                break;
            }
            matched += 1;
        }
        if matched != TYPE_COMMENT_PREFIX.len() {
            return TypeComment::NotType(p);
        }

        // This is a type comment; don't eat the newline.
        self.backup(c);
        let type_start = p;
        let ignore_end = p + 6;
        let _ignore_end_col_offset = current_starting_col_offset + 6;
        let cur_off = self.cur;
        let is_type_ignore = cur_off >= ignore_end
            && self.buf[type_start..type_start + 6] == *b"ignore"
            && !(cur_off > ignore_end
                && (self.buf_char(ignore_end) >= 128
                    || (self.buf_char(ignore_end) as u8).is_ascii_alphanumeric()));

        if is_type_ignore {
            let end = self.cur;
            // If this type ignore is the only thing on the line, consume the
            // newline also.
            if blankline {
                self.nextc();
                self.atbol = true;
            }
            TypeComment::Ignore(ignore_end, end)
        } else {
            TypeComment::Comment(type_start, self.cur)
        }
    }

    // -- numbers ------------------------------------------------------------------

    /// The fraction/exponent/imaginary tail shared by the decimal forms (the
    /// `goto fraction` / `goto exponent` / `goto imaginary` labels).
    /// `Ok(None)` means the number already ended with everything backed up
    /// (the early `return MAKE_TOKEN(NUMBER)` in the exponent path);
    /// `Ok(Some(term))` leaves `term` consumed for the caller to back up.
    fn number_chain(&mut self, start: ChainStart, mut c: i32) -> Result<Option<i32>, RawToken> {
        if matches!(start, ChainStart::Fraction) && is_digit(c) {
            c = self.decimal_tail().ok_or(RawToken::ERROR)?;
        }
        if !matches!(start, ChainStart::Imaginary) && (c == b'e' as i32 || c == b'E' as i32) {
            let e = c;
            c = self.nextc();
            if c == b'+' as i32 || c == b'-' as i32 {
                c = self.nextc();
                if !is_digit(c) {
                    self.backup(c);
                    return Err(self.syntaxerror("invalid decimal literal".to_string()));
                }
            } else if !is_digit(c) {
                self.backup(c);
                if !self.verify_end_of_number(e, "decimal") {
                    return Err(RawToken::ERROR);
                }
                self.backup(e);
                return Ok(None);
            }
            c = self.decimal_tail().ok_or(RawToken::ERROR)?;
        }
        if c == b'j' as i32 || c == b'J' as i32 {
            c = self.nextc();
            if !self.verify_end_of_number(c, "imaginary") {
                return Err(RawToken::ERROR);
            }
        } else if !self.verify_end_of_number(c, "decimal") {
            return Err(RawToken::ERROR);
        }
        Ok(Some(c))
    }

    /// The `isdigit` entry point; leaves the terminator unconsumed.
    fn number(&mut self, first: i32) -> Result<(), RawToken> {
        let mut c = first;
        if c != b'0' as i32 {
            // Decimal.
            c = self.decimal_tail().ok_or(RawToken::ERROR)?;
            let term = if c == b'.' as i32 {
                let after_dot = self.nextc();
                self.number_chain(ChainStart::Fraction, after_dot)?
            } else if c == b'e' as i32 || c == b'E' as i32 {
                self.number_chain(ChainStart::Exponent, c)?
            } else if c == b'j' as i32 || c == b'J' as i32 {
                self.number_chain(ChainStart::Imaginary, c)?
            } else {
                if !self.verify_end_of_number(c, "decimal") {
                    return Err(RawToken::ERROR);
                }
                Some(c)
            };
            if let Some(term) = term {
                self.backup(term);
            }
            return Ok(());
        }

        // Hex, octal or binary -- maybe.
        c = self.nextc();
        if c == b'x' as i32 || c == b'X' as i32 {
            c = self.nextc();
            loop {
                if c == b'_' as i32 {
                    c = self.nextc();
                }
                if !is_xdigit(c) {
                    self.backup(c);
                    return Err(self.syntaxerror("invalid hexadecimal literal".to_string()));
                }
                loop {
                    c = self.nextc();
                    if !is_xdigit(c) {
                        break;
                    }
                }
                if c != b'_' as i32 {
                    break;
                }
            }
            if !self.verify_end_of_number(c, "hexadecimal") {
                return Err(RawToken::ERROR);
            }
            self.backup(c);
            return Ok(());
        } else if c == b'o' as i32 || c == b'O' as i32 {
            c = self.nextc();
            loop {
                if c == b'_' as i32 {
                    c = self.nextc();
                }
                if !('0' as i32..'8' as i32).contains(&c) {
                    if is_digit(c) {
                        return Err(self.syntaxerror(format!(
                            "invalid digit '{}' in octal literal",
                            c as u8 as char
                        )));
                    }
                    self.backup(c);
                    return Err(self.syntaxerror("invalid octal literal".to_string()));
                }
                loop {
                    c = self.nextc();
                    if !('0' as i32..'8' as i32).contains(&c) {
                        break;
                    }
                }
                if c != b'_' as i32 {
                    break;
                }
            }
            if is_digit(c) {
                return Err(self.syntaxerror(format!(
                    "invalid digit '{}' in octal literal",
                    c as u8 as char
                )));
            }
            if !self.verify_end_of_number(c, "octal") {
                return Err(RawToken::ERROR);
            }
            self.backup(c);
            return Ok(());
        } else if c == b'b' as i32 || c == b'B' as i32 {
            c = self.nextc();
            loop {
                if c == b'_' as i32 {
                    c = self.nextc();
                }
                if c != b'0' as i32 && c != b'1' as i32 {
                    if is_digit(c) {
                        return Err(self.syntaxerror(format!(
                            "invalid digit '{}' in binary literal",
                            c as u8 as char
                        )));
                    }
                    self.backup(c);
                    return Err(self.syntaxerror("invalid binary literal".to_string()));
                }
                loop {
                    c = self.nextc();
                    if c != b'0' as i32 && c != b'1' as i32 {
                        break;
                    }
                }
                if c != b'_' as i32 {
                    break;
                }
            }
            if is_digit(c) {
                return Err(self.syntaxerror(format!(
                    "invalid digit '{}' in binary literal",
                    c as u8 as char
                )));
            }
            if !self.verify_end_of_number(c, "binary") {
                return Err(RawToken::ERROR);
            }
            self.backup(c);
            return Ok(());
        } else {
            // Maybe old-style octal; in any case, allow '0' as a literal.
            let mut nonzero = false;
            loop {
                if c == b'_' as i32 {
                    c = self.nextc();
                    if !is_digit(c) {
                        self.backup(c);
                        return Err(self.syntaxerror("invalid decimal literal".to_string()));
                    }
                }
                if c != b'0' as i32 {
                    break;
                }
                c = self.nextc();
            }
            let zeros_end = self.cur;
            if is_digit(c) {
                nonzero = true;
                c = self.decimal_tail().ok_or(RawToken::ERROR)?;
            }
            let term = if c == b'.' as i32 {
                let after_dot = self.nextc();
                self.number_chain(ChainStart::Fraction, after_dot)?
            } else if c == b'e' as i32 || c == b'E' as i32 {
                self.number_chain(ChainStart::Exponent, c)?
            } else if c == b'j' as i32 || c == b'J' as i32 {
                self.number_chain(ChainStart::Imaginary, c)?
            } else {
                if nonzero && !self.tok_extra_tokens {
                    // Old-style octal: now disallowed.
                    self.backup(c);
                    return Err(self.syntaxerror_known_range(
                        (self.start.unwrap() + 1 - self.line_start) as i64,
                        (zeros_end - self.line_start) as i64,
                        "leading zeros in decimal integer literals are not permitted; \
                         use an 0o prefix for octal integers"
                            .to_string(),
                    ));
                }
                if !self.verify_end_of_number(c, "decimal") {
                    return Err(RawToken::ERROR);
                }
                Some(c)
            };
            if let Some(term) = term {
                self.backup(term);
            }
        }
        Ok(())
    }

    fn syntaxerror_known_range(
        &mut self,
        _col_offset: i64,
        _end_col_offset: i64,
        message: String,
    ) -> RawToken {
        // The range only refines the exception's text, which this port does
        // not carry; the location is the same as syntaxerror's default.
        self.syntaxerror(message)
    }

    // -- strings --------------------------------------------------------------------

    fn f_string_quote(&mut self, c: i32) -> RawToken {
        let quote = c as u8;
        let mut quote_size = 1;
        self.first_lineno = self.lineno;
        self.multi_line_start = self.line_start;

        let after_quote = self.nextc();
        if after_quote == quote as i32 {
            let after_after_quote = self.nextc();
            if after_after_quote == quote as i32 {
                quote_size = 3;
            } else {
                self.backup(after_after_quote);
                self.backup(after_quote);
            }
        }
        if after_quote != quote as i32 {
            self.backup(after_quote);
        }

        let p_start = self.start;
        let p_end = Some(self.cur);
        if self.mode_index + 1 >= MAXFSTRINGLEVEL {
            return self.syntaxerror("too many nested f-strings".to_string());
        }
        self.mode_index += 1;
        let first = self.buf[p_start.unwrap()];
        let raw = match first {
            b'F' | b'f' => (self.buf_char(p_start.unwrap() + 1) as u8).eq_ignore_ascii_case(&b'r'),
            b'R' | b'r' => true,
            _ => return self.syntaxerror("invalid f-string prefix".to_string()),
        };
        let mode = &mut self.modes[self.mode_index];
        mode.kind = ModeKind::FString;
        mode.curly_bracket_depth = 0;
        mode.curly_bracket_expr_start_depth = -1;
        mode.quote = quote;
        mode.quote_size = quote_size;
        mode.raw = raw;
        mode.f_string_start = p_start.unwrap();
        mode.f_string_multi_line_start = self.line_start;
        mode.f_string_line_start = self.lineno;
        mode.f_string_debug = false;
        mode.in_format_spec = false;
        RawToken {
            kind: TokenKind::FStringStart,
            start: p_start,
            end: p_end,
        }
    }

    fn letter_quote(&mut self, c: i32) -> RawToken {
        let quote = c as u8;
        let mut quote_size = 1;
        let mut end_quote_size = 0;

        self.first_lineno = self.lineno;
        self.multi_line_start = self.line_start;

        let mut c = self.nextc();
        if c == quote as i32 {
            c = self.nextc();
            if c == quote as i32 {
                quote_size = 3;
            } else {
                end_quote_size = 1;
            }
        }
        if c != quote as i32 {
            self.backup(c);
        }

        while end_quote_size != quote_size {
            c = self.nextc();
            if self.done == Done::Error {
                return RawToken::ERROR;
            }
            if c == EOF || (quote_size == 1 && c == b'\n' as i32) {
                // Shift the location to the initial quote and report from
                // there.
                self.cur = self.start.unwrap() + 1;
                self.line_start = self.multi_line_start;
                let start_line = self.lineno;
                self.lineno = self.first_lineno;

                if self.mode_index > 0 {
                    let mode = *self.mode();
                    if mode.quote == quote && mode.quote_size == quote_size {
                        return self.syntaxerror("f-string: expecting '}'".to_string());
                    }
                }

                if quote_size == 3 {
                    self.syntaxerror(format!(
                        "unterminated triple-quoted string literal (detected at line {start_line})"
                    ));
                } else {
                    self.syntaxerror(format!(
                        "unterminated string literal (detected at line {start_line})"
                    ));
                }
                if c != b'\n' as i32 {
                    // E_EOFS / E_EOLS in C; the recorded error still wins.
                    self.done = Done::Error;
                }
                return RawToken::ERROR;
            }
            if c == quote as i32 {
                end_quote_size += 1;
            } else {
                end_quote_size = 0;
                if c == b'\\' as i32 {
                    let skipped = self.nextc();
                    if skipped == b'\r' as i32 {
                        self.nextc();
                    }
                }
            }
        }

        RawToken {
            kind: TokenKind::String,
            start: self.start,
            end: Some(self.cur),
        }
    }

    // -- f-string mode -----------------------------------------------------------

    fn fstring_mode(&mut self) -> RawToken {
        self.start = Some(self.cur);
        self.first_lineno = self.lineno;
        self.starting_col_offset = self.col_offset;

        // A bracket defers to normal mode: nothing to tokenize before it.
        let start_char = self.nextc();
        if start_char == b'{' as i32 {
            let peek1 = self.nextc();
            self.backup(peek1);
            self.backup(start_char);
            if peek1 != b'{' as i32 {
                self.mode_mut().curly_bracket_expr_start_depth += 1;
                if self.mode().curly_bracket_expr_start_depth >= MAX_EXPR_NESTING {
                    return self.syntaxerror("f-string: expressions nested too deeply".to_string());
                }
                self.mode_mut().kind = ModeKind::Regular;
                return self.normal_mode();
            }
        } else {
            self.backup(start_char);
        }

        // Check if we are at the end of the string.
        let quote = self.mode().quote;
        let quote_size = self.mode().quote_size;
        for _ in 0..quote_size {
            let q = self.nextc();
            if q != quote as i32 {
                self.backup(q);
                return self.fstring_middle();
            }
        }

        self.mode_index -= 1;
        RawToken {
            kind: TokenKind::FStringEnd,
            start: self.start,
            end: Some(self.cur),
        }
    }

    fn fstring_middle(&mut self) -> RawToken {
        let mut end_quote_size = 0;
        let mut unicode_escape = false;
        self.multi_line_start = self.line_start;
        let quote = self.mode().quote;
        let quote_size = self.mode().quote_size;
        loop {
            let c = self.nextc();
            if self.done == Done::Error {
                return RawToken::ERROR;
            }
            let in_format_spec =
                self.mode().in_format_spec && self.mode().curly_bracket_expr_start_depth >= 0;

            if c == EOF || (quote_size == 1 && c == b'\n' as i32) {
                if in_format_spec && c == b'\n' as i32 {
                    // The format spec ends here; return to regular mode.
                    self.backup(c);
                    self.mode_mut().kind = ModeKind::Regular;
                    self.mode_mut().in_format_spec = false;
                    return RawToken {
                        kind: TokenKind::FStringMiddle,
                        start: self.start,
                        end: Some(self.cur),
                    };
                }

                // Shift the location to the initial quote and report from
                // there.
                let mode = *self.mode();
                self.cur = mode.f_string_start + 1;
                self.line_start = mode.f_string_multi_line_start;
                let start_line = self.lineno;
                self.lineno = mode.f_string_line_start;

                if quote_size == 3 {
                    self.syntaxerror(format!(
                        "unterminated triple-quoted f-string literal (detected at line {start_line})"
                    ));
                } else {
                    self.syntaxerror(format!(
                        "unterminated f-string literal (detected at line {start_line})"
                    ));
                }
                if c != b'\n' as i32 {
                    self.done = Done::Error;
                }
                return RawToken::ERROR;
            }

            if c == quote as i32 {
                end_quote_size += 1;
                if end_quote_size == quote_size {
                    break;
                }
                continue;
            }
            end_quote_size = 0;

            if c == b'{' as i32 {
                let peek = self.nextc();
                if peek != b'{' as i32 || in_format_spec {
                    self.backup(peek);
                    self.backup(c);
                    self.mode_mut().curly_bracket_expr_start_depth += 1;
                    if self.mode().curly_bracket_expr_start_depth >= MAX_EXPR_NESTING {
                        return self
                            .syntaxerror("f-string: expressions nested too deeply".to_string());
                    }
                    self.mode_mut().kind = ModeKind::Regular;
                    self.mode_mut().in_format_spec = false;
                    return RawToken {
                        kind: TokenKind::FStringMiddle,
                        start: self.start,
                        end: Some(self.cur),
                    };
                }
                return RawToken {
                    kind: TokenKind::FStringMiddle,
                    start: self.start,
                    end: Some(self.cur - 1),
                };
            } else if c == b'}' as i32 {
                if unicode_escape {
                    return RawToken {
                        kind: TokenKind::FStringMiddle,
                        start: self.start,
                        end: Some(self.cur),
                    };
                }
                let peek = self.nextc();
                // Format specifiers can't legally use double brackets, so
                // only bypass at the top level of the bracket stack.
                let cursor = self.mode().curly_bracket_depth;
                if peek == b'}' as i32 && !in_format_spec && cursor == 0 {
                    return RawToken {
                        kind: TokenKind::FStringMiddle,
                        start: self.start,
                        end: Some(self.cur - 1),
                    };
                }
                self.backup(peek);
                self.backup(c);
                self.mode_mut().kind = ModeKind::Regular;
                return RawToken {
                    kind: TokenKind::FStringMiddle,
                    start: self.start,
                    end: Some(self.cur),
                };
            } else if c == b'\\' as i32 {
                let mut peek = self.nextc();
                if peek == b'\r' as i32 {
                    peek = self.nextc();
                }
                // A backslash right before a curly brace gives the brace
                // back to the loop, so the expression scanner sees it next.
                if peek == b'{' as i32 || peek == b'}' as i32 {
                    self.backup(peek);
                    continue;
                }
                if !self.mode().raw && peek == b'N' as i32 {
                    // Named unicode escapes (\N{BULLET}).
                    peek = self.nextc();
                    if peek == b'{' as i32 {
                        unicode_escape = true;
                    } else {
                        self.backup(peek);
                    }
                }
            }
        }

        // Back the quotes up to emit a final FSTRING_MIDDLE; the quotes go
        // to the FSTRING_END in the next iteration.
        for _ in 0..quote_size {
            self.backup(quote as i32);
        }
        RawToken {
            kind: TokenKind::FStringMiddle,
            start: self.start,
            end: Some(self.cur),
        }
    }
}

enum TypeComment {
    Ignore(usize, usize),
    Comment(usize, usize),
    NotType(usize),
}

enum ChainStart {
    Fraction,
    Exponent,
    Imaginary,
}

/// `tokenize._transform_msg`.
fn transform_msg(message: &str) -> String {
    if message.contains("unterminated triple-quoted string literal") {
        return "EOF in multi-line string".to_string();
    }
    message.to_string()
}

// --- the views -----------------------------------------------------------------

/// Options for the parser view.
#[derive(Debug, Clone, Copy, Default)]
pub struct ParserViewOptions {
    /// Whether `# type:` comments produce TYPE_COMMENT/TYPE_IGNORE tokens.
    /// Off by default, like CPython's tokenizer.
    pub type_comments: bool,
}

/// `_PyPegen_byte_offset_to_character_offset_line`: code points between two
/// byte offsets, stepping by UTF-8 character width; a byte at or past the
/// line's end counts as one character (the C code walks the NUL there).
fn count_chars_between(line: &[u8], a: usize, b: usize) -> i64 {
    let mut count = 0i64;
    let mut i = a;
    while i < b {
        if i >= line.len() {
            count += (b - i) as i64;
            break;
        }
        let byte = line[i];
        let step = if byte < 0x80 {
            1
        } else if byte & 0xE0 == 0xC0 {
            2
        } else if byte & 0xF0 == 0xE0 {
            3
        } else if byte & 0xF8 == 0xF0 {
            4
        } else {
            1
        };
        count += 1;
        i += step;
    }
    count
}

fn count_chars(bytes: &[u8]) -> i64 {
    // The buffer holds valid UTF-8 (re-encoded after the replace decode).
    std::str::from_utf8(bytes).map_or(bytes.len() as i64, |s| s.chars().count() as i64)
}

/// The (line, column) extents after `_tokenize.TokenizerIter`: columns in
/// code points, with the multi-line and trailing-token adjustments.
fn token_position(tokenizer: &Tokenizer, raw: &RawToken) -> (u32, i64, u32, i64) {
    let is_stringlit = raw.kind.is_string_literal();
    let line_base = (if is_stringlit {
        tokenizer.multi_line_start
    } else {
        tokenizer.line_start
    })
    .min(tokenizer.buf.len());
    // The decoded line: through `inp`, minus a faked newline. CPython
    // computes this with pointer differences; degenerate inputs can drive it
    // negative, so the line length is clamped rather than allowed to wrap.
    let mut line_len = tokenizer.inp.min(tokenizer.buf.len()) as i64 - line_base as i64;
    if line_len >= 1 && tokenizer.implicit_newline {
        line_len -= 1;
    }
    let line_len = line_len.clamp(0, (tokenizer.buf.len() - line_base) as i64) as usize;
    let line = &tokenizer.buf[line_base..line_base + line_len];

    let lineno = if is_stringlit {
        tokenizer.first_lineno
    } else {
        tokenizer.lineno
    };
    let end_lineno = tokenizer.lineno;

    let mut start_col: i64 = -1;
    let mut byte_offset: usize = 0;
    if let Some(s) = raw.start
        && s >= line_base
    {
        byte_offset = s - line_base;
        start_col = count_chars_between(line, 0, byte_offset);
    }
    let mut end_col: i64 = -1;
    if let Some(e) = raw.end
        && e >= tokenizer.line_start
    {
        let end_byte = e - tokenizer.line_start;
        if lineno == end_lineno {
            // Same line: count from the token start within the line. The
            // frames coincide whenever the token spans a single line.
            let token_col = count_chars_between(line, byte_offset, end_byte);
            end_col = start_col + token_col;
        } else {
            // `_raw` walks the current line; a token end never reaches
            // past `inp`, but clamp for the degenerate cases anyway.
            let end_byte = end_byte.min(tokenizer.buf.len() - tokenizer.line_start);
            end_col =
                count_chars(&tokenizer.buf[tokenizer.line_start..tokenizer.line_start + end_byte]);
        }
    }
    (lineno, start_col, end_lineno, end_col)
}

fn raw_text(tokenizer: &Tokenizer, raw: &RawToken) -> String {
    match (raw.start, raw.end) {
        (Some(s), Some(e)) if s <= e && e <= tokenizer.buf.len() => {
            String::from_utf8_lossy(&tokenizer.buf[s..e]).into_owned()
        }
        _ => String::new(),
    }
}

/// The tokenize-module view: `tokenize.tokenize`'s stream over the file's
/// bytes, including the ENCODING token.
pub fn tokenize_module_view(bytes: &[u8]) -> Result<Vec<Token>, TokenizeError> {
    let detection = detect_encoding(bytes).map_err(module_view_error)?;
    let mut out = vec![Token {
        kind: TokenKind::Encoding,
        text: detection.encoding_token.clone(),
        start: (0, 0),
        end: (0, 0),
    }];
    let mut tokenizer = Tokenizer::new(detection.lines, true, false);
    loop {
        let raw = tokenizer.next_token();
        if raw.kind == TokenKind::ErrorToken {
            return Err(tokenizer.finish_error());
        }
        let (lineno, start_col, end_lineno, end_col) = token_position(&tokenizer, &raw);
        let is_trailing = raw.kind == TokenKind::Endmarker
            || (raw.kind == TokenKind::Dedent && tokenizer.done == Done::Eof);
        let mut text = raw_text(&tokenizer, &raw);
        let mut end_col = end_col;

        // The adjustments tokenizeriter_next makes to match the original
        // Python tokenize implementation.
        let kind = if raw.kind.is_operator() {
            TokenKind::Op
        } else if matches!(raw.kind, TokenKind::Await | TokenKind::Async) {
            TokenKind::Name
        } else {
            raw.kind
        };
        if is_trailing {
            out.push(Token {
                kind,
                text,
                start: (lineno + 1, 0),
                end: (end_lineno + 1, 0),
            });
            if raw.kind == TokenKind::Endmarker {
                break;
            }
            continue;
        }
        match raw.kind {
            TokenKind::Newline => {
                if !tokenizer.implicit_newline {
                    text = if tokenizer.buf[raw.start.unwrap()] == b'\r' {
                        "\r\n".to_string()
                    } else {
                        "\n".to_string()
                    };
                }
                end_col += 1;
            }
            TokenKind::Nl if tokenizer.implicit_newline => text.clear(),
            _ => {}
        }
        out.push(Token {
            kind,
            text,
            start: (lineno, start_col),
            end: (end_lineno, end_col),
        });
    }
    Ok(out)
}

/// In the tokenize-module view an unsupported cookie codec reports exactly as
/// CPython's detect_encoding does: a bare SyntaxError.
fn module_view_error(error: TokenizeError) -> TokenizeError {
    if error.kind == TokenizeErrorKind::UnsupportedEncoding {
        TokenizeError {
            kind: TokenizeErrorKind::SyntaxError,
            message: error.message,
            line: None,
            column: None,
        }
    } else {
        error
    }
}

/// The parser view: what the parser consumes. No ENCODING, NL or COMMENT
/// tokens; identifiers are validated; async/await are keywords.
pub fn tokenize_parser_view(bytes: &[u8]) -> Result<Vec<Token>, TokenizeError> {
    tokenize_parser_view_with(bytes, &ParserViewOptions::default())
}

pub fn tokenize_parser_view_with(
    bytes: &[u8],
    options: &ParserViewOptions,
) -> Result<Vec<Token>, TokenizeError> {
    let detection = detect_encoding(bytes)?;
    let mut tokenizer = Tokenizer::new(detection.lines, false, options.type_comments);
    let mut out = Vec::new();
    loop {
        let raw = tokenizer.next_token();
        if raw.kind == TokenKind::ErrorToken {
            return Err(tokenizer.finish_error());
        }
        let (lineno, start_col, end_lineno, end_col) = token_position(&tokenizer, &raw);
        let text = raw_text(&tokenizer, &raw);
        let start_col = if raw.start.is_none() { 0 } else { start_col };
        let end_col = if raw.end.is_none() {
            start_col
        } else {
            end_col
        };
        out.push(Token {
            kind: raw.kind,
            text,
            start: (lineno, start_col),
            end: (end_lineno, end_col),
        });
        if raw.kind == TokenKind::Endmarker {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    type ModuleToken = (&'static str, String, (u32, i64), (u32, i64));

    fn module(source: &[u8]) -> Result<Vec<ModuleToken>, TokenizeError> {
        Ok(tokenize_module_view(source)?
            .into_iter()
            .map(|t| (t.kind.name(), t.text, t.start, t.end))
            .collect())
    }

    fn kinds(source: &[u8]) -> Result<Vec<&'static str>, TokenizeError> {
        Ok(module(source)?.into_iter().map(|(kind, ..)| kind).collect())
    }

    fn texts(source: &[u8]) -> Result<Vec<String>, TokenizeError> {
        Ok(module(source)?
            .into_iter()
            .map(|(_, text, ..)| text)
            .collect())
    }

    #[test]
    fn simple_stream() {
        let tokens = module(b"x = 1\n").unwrap();
        assert_eq!(
            tokens,
            vec![
                ("ENCODING", "utf-8".into(), (0, 0), (0, 0)),
                ("NAME", "x".into(), (1, 0), (1, 1)),
                ("OP", "=".into(), (1, 2), (1, 3)),
                ("NUMBER", "1".into(), (1, 4), (1, 5)),
                ("NEWLINE", "\n".into(), (1, 5), (1, 6)),
                ("ENDMARKER", "".into(), (2, 0), (2, 0)),
            ]
        );
    }

    #[test]
    fn empty_file() {
        let tokens = kinds(b"").unwrap();
        assert_eq!(tokens, ["ENCODING", "ENDMARKER"]);
        let tokens = module(b"").unwrap();
        assert_eq!(tokens[1], ("ENDMARKER", "".into(), (1, 0), (1, 0)));
    }

    #[test]
    fn missing_trailing_newline() {
        let tokens = module(b"x = 1").unwrap();
        assert_eq!(tokens[4], ("NEWLINE", "".into(), (1, 5), (1, 6)));
    }

    #[test]
    fn crlf_and_lone_cr() {
        let tokens = module(b"x = 1\r\n").unwrap();
        assert_eq!(tokens[4], ("NEWLINE", "\r\n".into(), (1, 5), (1, 7)));
        let tokens = module(b"x = 1\r").unwrap();
        assert_eq!(tokens[4], ("NEWLINE", "\r".into(), (1, 5), (1, 7)));
    }

    #[test]
    fn comment_and_nl() {
        let tokens = module(b"# only\n").unwrap();
        assert_eq!(
            tokens[1..],
            [
                ("COMMENT", "# only".into(), (1, 0), (1, 6)),
                ("NL", "\n".into(), (1, 6), (1, 7)),
                ("ENDMARKER", "".into(), (2, 0), (2, 0)),
            ]
        );
        // A comment at EOF without a newline gets an empty NL text.
        let tokens = module(b"# only").unwrap();
        assert_eq!(tokens[2], ("NL", "".into(), (1, 6), (1, 7)));
    }

    #[test]
    fn indentation_and_dedents() {
        let tokens = module(b"if 1:\n    pass\nx\n").unwrap();
        assert_eq!(tokens[5], ("INDENT", "    ".into(), (2, 0), (2, 4)));
        assert_eq!(tokens[8], ("DEDENT", "".into(), (3, 0), (3, 0)));
        assert_eq!(tokens[9], ("NAME", "x".into(), (3, 0), (3, 1)));
        assert_eq!(tokens[11], ("ENDMARKER", "".into(), (4, 0), (4, 0)));

        // At EOF the pending DEDENTs are trailing tokens on the next line.
        let tokens = module(b"if 1:\n    pass\n").unwrap();
        assert_eq!(tokens[8], ("DEDENT", "".into(), (3, 0), (3, 0)));
        assert_eq!(tokens[9], ("ENDMARKER", "".into(), (3, 0), (3, 0)));

        let tokens = module(b"if 1:\n    pass\n  x\n").unwrap_err();
        assert_eq!(tokens.kind, TokenizeErrorKind::IndentationError);
        assert_eq!(tokens.line, Some(3));
    }

    #[test]
    fn tabs_and_formfeed() {
        let err = tokenize_module_view(b"if 1:\n\tpass\n        pass\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::TabError);
        assert_eq!(err.line, Some(3));

        // A form feed resets the column but the token columns stay byte-based.
        let tokens = module(b"\x0cif 1:\n").unwrap();
        assert_eq!(tokens[1], ("NAME", "if".into(), (1, 1), (1, 3)));
    }

    #[test]
    fn continuation_lines() {
        // Implicit joining inside parentheses; NL tokens between lines.
        let tokens = module(b"x = (\n    1)\n").unwrap();
        assert_eq!(tokens[4], ("NL", "\n".into(), (1, 5), (1, 6)));
        assert_eq!(tokens[5], ("NUMBER", "1".into(), (2, 4), (2, 5)));

        // A backslash continuation.
        assert_eq!(
            kinds(b"x = \\\n    1\n").unwrap(),
            ["ENCODING", "NAME", "OP", "NUMBER", "NEWLINE", "ENDMARKER"]
        );

        // Something after the continuation backslash.
        let err = tokenize_module_view(b"x = \\y\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::TokenError);
        assert_eq!(err.line, Some(1));
    }

    #[test]
    fn multi_line_string_positions() {
        let tokens = module(b"x = '''\nabc\n'''\n").unwrap();
        assert_eq!(
            tokens[3],
            ("STRING", "'''\nabc\n'''".into(), (1, 4), (3, 3))
        );
    }

    #[test]
    fn byte_columns_become_code_points() {
        let tokens = module("x = 'éè'\n".as_bytes()).unwrap();
        assert_eq!(tokens[3], ("STRING", "'éè'".into(), (1, 4), (1, 8)));
        let tokens = module("é = 1\n".as_bytes()).unwrap();
        assert_eq!(tokens[1], ("NAME", "é".into(), (1, 0), (1, 1)));
    }

    #[test]
    fn replacement_characters_flow_through() {
        // Invalid UTF-8 after the checked lines decodes with replacement.
        let tokens = module(b"x = 1\ny = '\xff'\n").unwrap();
        assert_eq!(tokens[5], ("NAME", "y".into(), (2, 0), (2, 1)));
        assert_eq!(tokens[7], ("STRING", "'\u{FFFD}'".into(), (2, 4), (2, 7)));
    }

    #[test]
    fn degenerate_lone_carriage_return() {
        let tokens = module(b"\ry\n").unwrap();
        assert_eq!(tokens[1], ("OP", "\ry".into(), (1, 0), (1, 2)));
    }

    #[test]
    fn async_and_await_are_names_in_module_view() {
        let tokens = module(b"await async\n").unwrap();
        assert_eq!(tokens[1].0, "NAME");
        assert_eq!(tokens[2].0, "NAME");
        let tokens = tokenize_parser_view(b"await async\n").unwrap();
        assert_eq!(tokens[0].kind, TokenKind::Await);
        assert_eq!(tokens[1].kind, TokenKind::Async);
    }

    #[test]
    fn parser_view_shape() {
        let tokens = tokenize_parser_view(b"x = 1  # note\ny = 2\n").unwrap();
        let ks: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            ks,
            [
                TokenKind::Name,
                TokenKind::Equal,
                TokenKind::Number,
                TokenKind::Newline,
                TokenKind::Name,
                TokenKind::Equal,
                TokenKind::Number,
                TokenKind::Newline,
                TokenKind::Endmarker,
            ]
        );
        // Faithful to the C tokenizer: a trailing comment is skipped, and the
        // NEWLINE that follows spans from the comment's start.
        assert_eq!(tokens[3].text, "# note");
    }

    #[test]
    fn parser_view_type_comments() {
        let options = ParserViewOptions {
            type_comments: true,
        };
        let tokens =
            tokenize_parser_view_with(b"def f():\n    # type: () -> int\n    pass\n", &options)
                .unwrap();
        let comment = tokens
            .iter()
            .find(|t| t.kind == TokenKind::TypeComment)
            .unwrap();
        assert_eq!(comment.text, "() -> int");

        let tokens = tokenize_parser_view_with(b"# type: ignore\nx = 1\n", &options).unwrap();
        assert!(tokens.iter().any(|t| t.kind == TokenKind::TypeIgnore));
        let ignore = tokens
            .iter()
            .find(|t| t.kind == TokenKind::TypeIgnore)
            .unwrap();
        // The TYPE_IGNORE text runs from after the word "ignore" to the
        // newline, which a blankline ignore has already consumed.
        assert_eq!(ignore.text, "");

        // Off by default.
        let tokens = tokenize_parser_view(b"# type: ignore\nx = 1\n").unwrap();
        assert!(!tokens.iter().any(|t| t.kind == TokenKind::TypeIgnore));
    }

    #[test]
    fn parser_view_rejects_invalid_identifier() {
        let err = tokenize_parser_view("\u{00A9} = 1\n".as_bytes()).unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::TokenError);
        assert!(err.message.contains("U+00A9"), "{}", err.message);
        // The module view keeps it as an OP-ish token instead.
        assert!(tokenize_module_view("\u{00A9} = 1\n".as_bytes()).is_ok());
    }

    #[test]
    fn number_forms() {
        assert!(tokenize_module_view(b"0x_1 0b0 0o7 .5e3 1_000").is_ok());
        let tokens = module(b"1 .real").unwrap();
        assert_eq!(
            tokens[1..],
            [
                ("NUMBER", "1".into(), (1, 0), (1, 1)),
                ("OP", ".".into(), (1, 2), (1, 3)),
                ("NAME", "real".into(), (1, 3), (1, 7)),
                ("NEWLINE", "".into(), (1, 7), (1, 8)),
                ("ENDMARKER", "".into(), (2, 0), (2, 0)),
            ]
        );
    }

    #[test]
    fn number_errors() {
        let err = tokenize_module_view(b"1_\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::TokenError);
        assert_eq!(err.line, Some(1));
        let err = tokenize_parser_view(b"01\n").unwrap_err();
        assert!(err.message.contains("leading zeros"), "{}", err.message);
        // 01 is a valid token in the tokenize view.
        assert!(tokenize_module_view(b"01\n").is_ok());
    }

    #[test]
    fn fstrings() {
        let tokens = tokenize_module_view(b"f\"a{x}b\"\n").unwrap();
        let ks: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            ks,
            [
                TokenKind::Encoding,
                TokenKind::FStringStart,
                TokenKind::FStringMiddle,
                TokenKind::Op, // LBRACE reported as OP
                TokenKind::Name,
                TokenKind::Op, // RBRACE reported as OP
                TokenKind::FStringMiddle,
                TokenKind::FStringEnd,
                TokenKind::Newline,
                TokenKind::Endmarker,
            ]
        );
        assert_eq!(tokens[2].text, "a");
        assert_eq!(tokens[4].text, "x");
        assert_eq!(tokens[6].text, "b");

        // Nested quotes and a format spec.
        let tokens = tokenize_module_view(b"f\"{d['k']:!{f}}\"").unwrap();
        assert!(
            tokens
                .iter()
                .any(|t| t.kind == TokenKind::FStringMiddle && t.text == "!")
        );

        // Escaped braces stay in the middle.
        let tokens = texts(b"f\"{{x}}\"").unwrap();
        assert_eq!(tokens[1], "f\"");
        assert_eq!(tokens[2], "{");
        assert_eq!(tokens[3], "x}");
        assert_eq!(tokens[4], "\"");
    }

    #[test]
    fn fstring_nested_format_specs() {
        let tokens = texts(b"f\"{v:{w:0}.{p:1}}\"").unwrap();
        assert_eq!(
            tokens,
            [
                "utf-8", "f\"", "{", "v", ":", "{", "w", ":", "0", "}", ".", "{", "p", ":", "1",
                "}", "", "}", "\"", "", ""
            ]
        );
    }

    #[test]
    fn fstring_errors() {
        let err = tokenize_module_view(b"f'{x'").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::TokenError);
        assert_eq!(err.message, "f-string: expecting '}'");
        assert_eq!(err.line, Some(1));

        let err = tokenize_module_view(b"f'''x").unwrap_err();
        assert_eq!(
            err.message,
            "unterminated triple-quoted f-string literal (detected at line 1)"
        );

        let err = tokenize_module_view(b"x = '''abc").unwrap_err();
        assert_eq!(err.message, "EOF in multi-line string");
        assert_eq!(err.line, Some(1));
    }

    #[test]
    fn tokenizer_errors() {
        let cases: &[(&[u8], TokenizeErrorKind, u32, i64)] = &[
            (b"\x001\n", TokenizeErrorKind::TokenError, 1, 0),
            (b"x = (\n1\n", TokenizeErrorKind::TokenError, 2, 0),
            (b"s = 'abc", TokenizeErrorKind::TokenError, 1, 5),
            // _tokenizer_error positions: inp - buf for E_EOF, the character
            // offset of inp in the line without its newline otherwise.
            (b"x = \\ y\n", TokenizeErrorKind::TokenError, 1, 8),
            (b"x = \\y\n", TokenizeErrorKind::TokenError, 1, 7),
            (b"1_\n", TokenizeErrorKind::TokenError, 1, 2),
            (
                b"if 1:\n\tpass\n        pass\n",
                TokenizeErrorKind::TabError,
                3,
                13,
            ),
            (
                b"if 1:\n    pass\n  x\n",
                TokenizeErrorKind::IndentationError,
                3,
                4,
            ),
        ];
        for (source, kind, line, column) in cases {
            let err = tokenize_module_view(source).unwrap_err();
            assert_eq!(err.kind, *kind, "{:?}", source);
            assert_eq!(err.line, Some(*line), "{:?}", source);
            assert_eq!(err.column, Some(*column), "{:?}", source);
        }
    }

    #[test]
    fn encodings() {
        // ISO-8859-1 cookie. The cookie lines reappear as COMMENT/NL.
        let tokens = module(b"# -*- coding: iso-8859-1 -*-\nx = '\xe9'\n").unwrap();
        assert_eq!(tokens[0], ("ENCODING", "iso-8859-1".into(), (0, 0), (0, 0)));
        assert_eq!(tokens[1].0, "COMMENT");
        assert_eq!(tokens[2].0, "NL");
        assert_eq!(tokens[5], ("STRING", "'\u{e9}'".into(), (2, 4), (2, 7)));

        // KOI8-R cookie.
        let tokens = module(b"# -*- coding: koi8-r -*-\nx = '\xef'\n").unwrap();
        assert_eq!(tokens[5].1, "'\u{41E}'");

        // The ENCODING token keeps the declared name.
        let tokens = module(b"# coding: latin1\nx = 1\n").unwrap();
        assert_eq!(tokens[0].1, "latin1");

        // BOM.
        let tokens = module(b"\xef\xbb\xbfx = 1\n").unwrap();
        assert_eq!(tokens[0].1, "utf-8");

        // BOM plus utf-8 cookie.
        let tokens = module(b"\xef\xbb\xbf# coding: utf-8\nx = 1\n").unwrap();
        assert_eq!(tokens[0].1, "utf-8");

        // A cookie on line 2 when line 1 is blank.
        let tokens = module(b"\n# coding: koi8-r\nx = 1\n").unwrap();
        assert_eq!(tokens[0].1, "koi8-r");

        // A cookie on line 2 is ignored when line 1 has code.
        let tokens = module(b"x = 1\n# coding: koi8-r\ny = '\xef'\n").unwrap();
        assert_eq!(tokens[0].1, "utf-8");
        assert_eq!(tokens[9].1, "'\u{FFFD}'");
    }

    // Expected strings from CPython 3.12's tokenize.tokenize on the same bytes.
    #[test]
    fn codec_aliases() {
        let string = |src: &[u8]| module(src).unwrap()[5].1.clone();
        assert_eq!(string(b"# coding: utf8\nx = '\xc3\xa9'\n"), "'\u{e9}'");
        assert_eq!(module(b"# -*- coding: u8 -*-\nx = 1\n").unwrap()[0].1, "u8");
        assert_eq!(
            module(b"# coding: us-ascii\nx = 1\n").unwrap()[0].1,
            "us-ascii"
        );
        assert_eq!(string(b"# coding: l1\nx = '\xe9'\n"), "'\u{e9}'");
        assert_eq!(
            string(b"# vim: set fileencoding=latin :\nx = '\xe9'\n"),
            "'\u{e9}'"
        );
        assert_eq!(
            string(b"# coding: cp1252\nx = '\x80\x93\xe9'\n"),
            "'\u{20AC}\u{201C}\u{e9}'"
        );
        assert_eq!(
            string(b"# coding: windows-1252\nx = '\x80'\n"),
            "'\u{20AC}'"
        );
        assert_eq!(
            string(b"# coding: iso-8859-15\nx = '\xa4\xbe'\n"),
            "'\u{20AC}\u{178}'"
        );
        assert_eq!(string(b"# coding: latin9\nx = '\xa4'\n"), "'\u{20AC}'");
        // tokenize decodes with errors="replace".
        let tokens = module(b"# coding: ascii\nx = 1\ny = '\xe9'\n").unwrap();
        assert_eq!(tokens[9].1, "'\u{FFFD}'");
        let tokens = module(b"# coding: cp1252\nx = 1\ny = '\x81'\n").unwrap();
        assert_eq!(tokens[9].1, "'\u{FFFD}'");
        assert!(module(b"# coding: nosuchcodec\nx = 1\n").is_err());
    }

    #[test]
    fn encoding_errors() {
        // BOM plus a non-utf-8 cookie.
        let err = tokenize_module_view(b"\xef\xbb\xbf# coding: koi8-r\nx = 1\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::SyntaxError);
        assert_eq!(err.message, "encoding problem: utf-8");
        assert_eq!(err.line, None);

        // An unsupported codec names the codec; the module view reports
        // CPython's SyntaxError for it.
        let err = tokenize_module_view(b"# coding: uft-8\nx = 1\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::SyntaxError);
        assert_eq!(err.message, "unknown encoding: uft-8");
        let err = tokenize_parser_view(b"# coding: uft-8\nx = 1\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::UnsupportedEncoding);
        assert!(err.message.contains("uft-8"), "{}", err.message);

        // Invalid UTF-8 on the first line is the declaration error.
        let err = tokenize_module_view(b"x = '\xff'\n").unwrap_err();
        assert_eq!(err.kind, TokenizeErrorKind::SyntaxError);
        assert_eq!(err.message, "invalid or missing encoding declaration");
        assert_eq!(err.line, None);
    }

    #[test]
    fn exact_token_types_round_trip() {
        assert_eq!(exact_token_type("("), Some(TokenKind::Lpar));
        assert_eq!(exact_token_type("//="), Some(TokenKind::Doubleslashequal));
        assert_eq!(exact_token_type("!"), Some(TokenKind::Exclamation));
        assert_eq!(exact_token_type("\ry"), None);
        assert_eq!(exact_token_type("and"), None);
    }

    #[test]
    fn never_panics_on_random_input() {
        // A deterministic LCG over random byte strings: the only invariant is
        // that tokenization terminates without panicking.
        let mut state: u64 = 0x243F6A8885A308D3;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for length in 0..64 {
            let bytes: Vec<u8> = (0..length).map(|_| (next() & 0xFF) as u8).collect();
            let _ = tokenize_module_view(&bytes);
            let _ = tokenize_parser_view(&bytes);
        }
        for _ in 0..512 {
            let length = (next() % 200) as usize;
            let bytes: Vec<u8> = (0..length).map(|_| (next() & 0xFF) as u8).collect();
            let _ = tokenize_module_view(&bytes);
            let _ = tokenize_parser_view(&bytes);
        }
    }
}

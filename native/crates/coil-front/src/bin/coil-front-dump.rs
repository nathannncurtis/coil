//! Dump front-end results as JSONL, one record per input file, in the exact
//! format of `tools/front_oracle.py`.

use std::io::Write;

use coil_front::tokenizer::{Token, TokenKind, exact_token_type, tokenize_module_view};

fn json_string(out: &mut String, text: &str) {
    // json.dumps(..., ensure_ascii=True): short escapes for \b \t \n \f \r,
    // \u00xx (lowercase hex) for other control characters, and \uXXXX
    // (lowercase, surrogate pairs for astral) for non-ASCII.
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{0009}' => out.push_str("\\t"),
            '\u{000A}' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\u{000D}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) < 0x80 => out.push(c),
            c if (c as u32) < 0x10000 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => {
                let cp = c as u32 - 0x10000;
                let high = 0xD800 + (cp >> 10);
                let low = 0xDC00 + (cp & 0x3FF);
                out.push_str(&format!("\\u{:04x}\\u{:04x}", high, low));
            }
        }
    }
    out.push('"');
}

/// `tokenize.tok_name[token.exact_type]`: `OP` tokens resolve through
/// `token.EXACT_TOKEN_TYPES`, everything else reports its own kind.
fn exact_name(token: &Token) -> &'static str {
    if token.kind == TokenKind::Op {
        exact_token_type(&token.text).map_or("OP", |kind| kind.name())
    } else {
        token.kind.name()
    }
}

fn tokens_record(path: &str, bytes: &[u8], out: &mut String) {
    out.push_str("{\"path\":");
    json_string(out, path);
    match tokenize_module_view(bytes) {
        Ok(tokens) => {
            out.push_str(",\"ok\":true,\"tokens\":[");
            for (i, token) in tokens.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str("[\"");
                out.push_str(exact_name(token));
                out.push_str("\",");
                json_string(out, &token.text);
                out.push_str(&format!(
                    ",[{},{}],[{},{}]]",
                    token.start.0, token.start.1, token.end.0, token.end.1
                ));
            }
            out.push_str("]}\n");
        }
        Err(error) => {
            out.push_str(&format!(
                ",\"ok\":false,\"error\":{{\"type\":\"{}\",\"lineno\":{}}}}}\n",
                error.kind.name(),
                match error.line {
                    Some(line) => line.to_string(),
                    None => "null".to_string(),
                }
            ));
        }
    }
}

fn run(args: &[String]) -> i32 {
    let mut mode = None;
    let mut optimize = 0u8;
    let mut files = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--tokens" if mode.is_none() => mode = Some(Mode::Tokens),
            "--ast" if mode.is_none() => mode = Some(Mode::Ast),
            "--symtable" if mode.is_none() => mode = Some(Mode::Symtable),
            "--ir" if mode.is_none() => mode = Some(Mode::Ir),
            "--optimize" if matches!(mode, Some(Mode::Ir)) => {
                match rest.next().map(|v| v.parse::<u8>()) {
                    Some(Ok(level)) if level <= 2 => optimize = level,
                    _ => {
                        eprintln!("error: --optimize needs a level in 0..=2");
                        return 2;
                    }
                }
            }
            _ => files.push(arg),
        }
    }
    let mode = match mode {
        Some(mode) => mode,
        None => {
            eprintln!(
                "usage: coil-front-dump --tokens|--ast|--symtable|--ir [--optimize N] FILE..."
            );
            return 2;
        }
    };
    let mut out = String::new();
    let mut stdout = std::io::stdout().lock();
    for path in &files {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("error: {path}: {err}");
                return 2;
            }
        };
        match mode {
            Mode::Tokens => tokens_record(path, &bytes, &mut out),
            Mode::Ast => ast_record(path, &bytes, &mut out),
            Mode::Symtable => symtable_record(path, &bytes, &mut out),
            Mode::Ir => ir_record(path, &bytes, optimize, &mut out),
        }
        if stdout.write_all(out.as_bytes()).is_err() {
            return 2;
        }
        out.clear();
    }
    0
}

/// The parse error and symtable error records share the SyntaxError shape;
/// a symtable RuntimeError reports through the same fields.
fn error_record(out: &mut String, kind: &str, message: &str, error: &impl FrontError) {
    out.push_str(",\"ok\":false,\"error\":{\"type\":\"");
    out.push_str(kind);
    out.push_str("\",\"msg\":");
    json_string(out, message);
    out.push_str(&format!(
        ",\"lineno\":{},\"offset\":{},\"end_lineno\":{},\"end_offset\":{}}}}}\n",
        opt(error.lineno()),
        opt(error.offset()),
        opt(error.end_lineno()),
        opt(error.end_offset()),
    ));
}

trait FrontError {
    fn lineno(&self) -> Option<i32>;
    fn offset(&self) -> Option<i32>;
    fn end_lineno(&self) -> Option<i32>;
    fn end_offset(&self) -> Option<i32>;
}

impl FrontError for coil_front::parser::ParseError {
    fn lineno(&self) -> Option<i32> {
        self.lineno
    }
    fn offset(&self) -> Option<i32> {
        self.offset
    }
    fn end_lineno(&self) -> Option<i32> {
        self.end_lineno
    }
    fn end_offset(&self) -> Option<i32> {
        self.end_offset
    }
}

impl FrontError for coil_front::symtable::SymtableError {
    fn lineno(&self) -> Option<i32> {
        self.lineno
    }
    fn offset(&self) -> Option<i32> {
        self.offset
    }
    fn end_lineno(&self) -> Option<i32> {
        self.end_lineno
    }
    fn end_offset(&self) -> Option<i32> {
        self.end_offset
    }
}

fn ast_record(path: &str, bytes: &[u8], out: &mut String) {
    out.push_str("{\"path\":");
    json_string(out, path);
    match coil_front::parser::parse_module(bytes, Some(std::path::Path::new(path))) {
        Ok((module, interner)) => {
            out.push_str(",\"ok\":true,\"ast\":");
            out.push_str(&coil_front::dump::dump_module(&module, &interner));
            out.push_str("}\n");
        }
        Err(error) => {
            error_record(out, error.kind.name(), &error.message, &error);
        }
    }
}

fn symtable_record(path: &str, bytes: &[u8], out: &mut String) {
    out.push_str("{\"path\":");
    json_string(out, path);
    match coil_front::parser::parse_module(bytes, Some(std::path::Path::new(path))) {
        Ok((module, mut interner)) => {
            match coil_front::symtable::symtable(&module, &mut interner) {
                Ok(table) => {
                    out.push_str(",\"ok\":true,\"symtable\":");
                    out.push_str(&coil_front::dump::dump_symtable(&table, &interner));
                    out.push_str("}\n");
                }
                Err(error) => {
                    error_record(out, error.kind.name(), &error.message, &error);
                }
            }
        }
        Err(error) => {
            error_record(out, error.kind.name(), &error.message, &error);
        }
    }
}

fn ir_record(path: &str, bytes: &[u8], optimize: u8, out: &mut String) {
    out.push_str("{\"path\":");
    json_string(out, path);
    match coil_front::parser::parse_module(bytes, Some(std::path::Path::new(path))) {
        Ok((module, mut interner)) => {
            match coil_front::symtable::symtable(&module, &mut interner) {
                Ok(table) => {
                    let module_name = std::path::Path::new(path)
                        .file_stem()
                        .map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned());
                    let options = coil_front::lower::LowerOptions { optimize };
                    match coil_front::lower::lower_module(
                        &module_name,
                        std::path::PathBuf::from(path),
                        &module,
                        bytes,
                        &interner,
                        &table,
                        options,
                    ) {
                        Ok(result) => {
                            out.push_str(&format!(
                                ",\"ok\":true,\"functions\":{},\"classes\":{},\"unsupported\":[",
                                result.module.functions.len(),
                                result.module.classes.len()
                            ));
                            for (i, kind) in result.unsupported.iter().enumerate() {
                                if i > 0 {
                                    out.push(',');
                                }
                                json_string(out, kind.name());
                            }
                            out.push_str("],\"ir\":");
                            json_string(out, &coil_ir::print::print_module(&result.module));
                            out.push_str("}\n");
                        }
                        Err(error) => {
                            error_record(out, "LowerError", &error.to_string(), &NoPosition);
                        }
                    }
                }
                Err(error) => {
                    error_record(out, error.kind.name(), &error.message, &error);
                }
            }
        }
        Err(error) => {
            error_record(out, error.kind.name(), &error.message, &error);
        }
    }
}

/// Lowering errors carry no source position.
struct NoPosition;

impl FrontError for NoPosition {
    fn lineno(&self) -> Option<i32> {
        None
    }
    fn offset(&self) -> Option<i32> {
        None
    }
    fn end_lineno(&self) -> Option<i32> {
        None
    }
    fn end_offset(&self) -> Option<i32> {
        None
    }
}

fn opt(value: Option<i32>) -> String {
    match value {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    }
}

enum Mode {
    Tokens,
    Ast,
    Symtable,
    Ir,
}

fn main() {
    std::process::exit(run(&std::env::args().skip(1).collect::<Vec<_>>()));
}

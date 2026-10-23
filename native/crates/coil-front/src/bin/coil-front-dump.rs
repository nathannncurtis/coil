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
    let mode = match args.first().map(String::as_str) {
        Some("--tokens") => Mode::Tokens,
        Some("--ast") => Mode::Ast,
        Some("--symtable") => {
            eprintln!("error: --symtable is not implemented in this build");
            return 2;
        }
        _ => {
            eprintln!("usage: coil-front-dump --tokens|--ast|--symtable FILE...");
            return 2;
        }
    };
    let mut out = String::new();
    let mut stdout = std::io::stdout().lock();
    for path in &args[1..] {
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
        }
        if stdout.write_all(out.as_bytes()).is_err() {
            return 2;
        }
        out.clear();
    }
    0
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
            out.push_str(",\"ok\":false,\"error\":{\"type\":\"");
            out.push_str(error.kind.name());
            out.push_str("\",\"msg\":");
            json_string(out, &error.message);
            out.push_str(&format!(
                ",\"lineno\":{},\"offset\":{},\"end_lineno\":{},\"end_offset\":{}}}}}\n",
                opt(error.lineno),
                opt(error.offset),
                opt(error.end_lineno),
                opt(error.end_offset),
            ));
        }
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
}

fn main() {
    std::process::exit(run(&std::env::args().skip(1).collect::<Vec<_>>()));
}

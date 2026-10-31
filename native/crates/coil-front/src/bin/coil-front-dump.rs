//! Dump front-end results as JSONL, one record per input file, in the exact
//! format of `tools/front_oracle.py`.

use std::io::Write;

use coil_front::tokenizer::{
    Codec, Token, TokenKind, encode_text, exact_token_type, lookup_codec, tokenize_module_view,
};

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
            "--run" if mode.is_none() => mode = Some(Mode::Run),
            "--coverage" if mode.is_none() => mode = Some(Mode::Coverage),
            "--snippets" if mode.is_none() => {
                return snippets_run(rest.next().map(String::as_str));
            }
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
                "usage: coil-front-dump --tokens|--ast|--symtable|--ir|--run|--coverage|--snippets [--optimize N] FILE..."
            );
            return 2;
        }
    };
    if matches!(mode, Mode::Run | Mode::Coverage) {
        if optimize != 0 || files.is_empty() {
            eprintln!("error: --run/--coverage take exactly FILE arguments");
            return 2;
        }
        let names: Vec<String> = files.into_iter().cloned().collect();
        let coverage = matches!(mode, Mode::Coverage);
        // Interpreting nests one Rust frame per Python frame; corpus
        // programs recurse to CPython's 1000-frame limit, which overflows
        // the default thread stack.
        let worker = std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(move || run_programs(&names, coverage));
        match worker {
            Ok(handle) => match handle.join() {
                Ok(code) => return code,
                Err(_) => {
                    eprintln!("error: interpreter thread panicked");
                    return 2;
                }
            },
            Err(err) => {
                eprintln!("error: {err}");
                return 2;
            }
        }
    }
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
            // handled before the per-file loop
            Mode::Run | Mode::Coverage => unreachable!("handled above"),
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
                        Ok(module) => {
                            out.push_str(&format!(
                                ",\"ok\":true,\"functions\":{},\"classes\":{}",
                                module.functions.len(),
                                module.classes.len()
                            ));
                            out.push_str(",\"ir\":");
                            json_string(out, &coil_ir::print::print_module(&module));
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
    Run,
    Coverage,
}

/// Runs each program on the IR interpreter's test object model: the
/// program's `print` output goes to stdout, an uncaught exception report to
/// stderr (exit 1). With `coverage`, prints the union of the interpreter's
/// covered instruction tags, one name per line, instead of running output.
fn run_programs(files: &[String], coverage: bool) -> i32 {
    use coil_ir::interp::Interp;
    use coil_ir::interp::model::ObjectModel;
    use coil_ir::interp::testmodel::TestModel;

    let mut covered: Vec<bool> = vec![false; coil_ir::interp::tag_count()];
    for path in files {
        let path = path.as_str();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) => {
                eprintln!("error: {path}: {err}");
                return 2;
            }
        };
        let module = match parse_lower(path, &bytes) {
            Ok(module) => module,
            Err(message) => {
                eprintln!("error: {path}: {message}");
                return 2;
            }
        };
        let mut model = TestModel::new();
        let globals = model.new_dict();
        let mut vm = Interp::new(&module, globals);
        let failed = vm.run_main(&mut model).err();
        for tag in vm.coverage.covered() {
            covered[tag] = true;
        }
        if let Some(exc) = failed {
            if coverage {
                // coverage scanning tolerates the corpus's intentionally
                // uncaught exceptions; the instructions still ran
                eprintln!("note: {path}: uncaught exception");
            } else {
                let report = model.format_uncaught(&exc);
                eprintln!("{report}");
                return 1;
            }
        }
    }
    if coverage {
        let mut stdout = std::io::stdout().lock();
        for (tag, seen) in covered.iter().enumerate() {
            if *seen {
                let _ = writeln!(stdout, "{}", coil_ir::interp::tag_name(tag));
            }
        }
    }
    0
}

fn parse_lower(path: &str, bytes: &[u8]) -> Result<coil_ir::ir::Module, String> {
    let (module, mut interner) =
        coil_front::parser::parse_module(bytes, Some(std::path::Path::new(path)))
            .map_err(|e| format!("{}: {}", e.kind.name(), e.message))?;
    let table = coil_front::symtable::symtable(&module, &mut interner)
        .map_err(|e| format!("{}: {}", e.kind.name(), e.message))?;
    let module_name = std::path::Path::new(path)
        .file_stem()
        .map_or_else(|| path.to_string(), |s| s.to_string_lossy().into_owned());
    coil_front::lower::lower_module(
        &module_name,
        std::path::PathBuf::from(path),
        &module,
        bytes,
        &interner,
        &table,
        coil_front::lower::LowerOptions { optimize: 0 },
    )
    .map_err(|e| e.to_string())
}

fn main() {
    std::process::exit(run(&std::env::args().skip(1).collect::<Vec<_>>()));
}

// -- snippets mode ------------------------------------------------------------

/// `front_oracle.snippet_record` over a JSON Lines corpus: one source per
/// line with an optional id, compile mode and encoding.
fn snippets_run(path: Option<&str>) -> i32 {
    let path = match path {
        Some(path) => path,
        None => {
            eprintln!("error: --snippets takes exactly one file");
            return 2;
        }
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("error: {path}: {err}");
            return 2;
        }
    };
    let mut stdout = std::io::stdout().lock();
    for (number, line) in text.split('\n').enumerate() {
        let number = number + 1;
        if line.trim().is_empty() {
            continue;
        }
        let fields = match json_string_fields(line) {
            Some(fields) => fields,
            None => {
                eprintln!("error: {path}:{number}: not a JSON object");
                return 2;
            }
        };
        let src = match fields.iter().find(|(key, _)| key == "src") {
            Some((_, src)) => src.clone(),
            None => {
                eprintln!("error: {path}:{number}: not a snippet object with a src field");
                return 2;
            }
        };
        let path_key = fields
            .iter()
            .find(|(key, _)| key == "id")
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| number.to_string());
        let record = snippet_record(&path_key, &src, &fields);
        if stdout.write_all(record.as_bytes()).is_err() {
            return 2;
        }
    }
    0
}

fn snippet_record(path: &str, src: &str, fields: &[(String, String)]) -> String {
    let mode = fields
        .iter()
        .find(|(key, _)| key == "mode")
        .map(|(_, value)| value.as_str())
        .unwrap_or("exec");
    let encoding = fields
        .iter()
        .find(|(key, _)| key == "encoding")
        .map(|(_, value)| value.as_str())
        .unwrap_or("utf-8");
    let codec = match lookup_codec(encoding) {
        Some(codec) => codec,
        None => {
            return named_error_record(
                path,
                "LookupError",
                &format!("unknown encoding: {encoding}"),
            );
        }
    };
    let bytes = match encode_text(codec, src) {
        Ok(bytes) => bytes,
        Err((ch, position)) => {
            let display = codec_display_name(codec);
            return named_error_record(
                path,
                "UnicodeEncodeError",
                &format!(
                    "'{display}' codec can't encode character {} in position {position}: \
                     ordinal not in range(256)",
                    char_repr(ch)
                ),
            );
        }
    };
    let parsed = match mode {
        "exec" => coil_front::parser::parse_module(&bytes, Some(std::path::Path::new("<string>"))),
        "eval" => coil_front::parser::parse_eval(&bytes, Some(std::path::Path::new("<string>"))),
        "single" => {
            coil_front::parser::parse_interactive(&bytes, Some(std::path::Path::new("<string>")))
        }
        _ => {
            return named_error_record(
                path,
                "ValueError",
                &format!("unsupported compile mode {mode:?}"),
            );
        }
    };
    let mut out = String::new();
    out.push_str("{\"path\":");
    json_string(&mut out, path);
    match parsed {
        Ok((module, interner)) => {
            out.push_str(",\"ok\":true,\"ast\":");
            out.push_str(&coil_front::dump::dump_module(&module, &interner));
            out.push_str("}\n");
        }
        Err(error) => {
            out.push_str(",\"ok\":false,\"error\":{\"type\":\"");
            out.push_str(error.kind.name());
            out.push_str("\",\"msg\":");
            json_string(&mut out, &error.message);
            out.push_str(&format!(
                ",\"lineno\":{},\"offset\":{},\"end_lineno\":{},\"end_offset\":{}}}}}\n",
                opt(error.lineno),
                opt(error.offset),
                opt(error.end_lineno),
                opt(error.end_offset),
            ));
        }
    }
    out
}

/// An error record for a non-SyntaxError failure: `str(exc)` with no
/// positions, like `_error_fields` builds them.
fn named_error_record(path: &str, kind: &str, message: &str) -> String {
    let mut out = String::new();
    out.push_str("{\"path\":");
    json_string(&mut out, path);
    out.push_str(",\"ok\":false,\"error\":{\"type\":\"");
    out.push_str(kind);
    out.push_str("\",\"msg\":");
    json_string(&mut out, message);
    out.push_str(",\"lineno\":null,\"offset\":null,\"end_lineno\":null,\"end_offset\":null}}\n");
    out
}

fn codec_display_name(codec: Codec) -> &'static str {
    match codec {
        Codec::Utf8 => "utf-8",
        Codec::Iso8859_1 => "latin-1",
        Codec::Ascii => "ascii",
        Codec::Cp1252 => "cp1252",
        Codec::Iso8859_15 => "iso8859-15",
        Codec::Cp1251 => "cp1251",
        Codec::Iso8859_7 => "iso8859-7",
        Codec::Koi8R => "koi8-r",
    }
}

/// `repr` of a single character: printable characters stand for themselves
/// (`str.isprintable`, keeping the ASCII space), the rest use Python's
/// `\xXX`, `\uXXXX` and `\UXXXXXXXX` escapes.
fn char_repr(ch: char) -> String {
    let printable = !ch.is_control() && (!ch.is_whitespace() || ch == ' ');
    if printable {
        return format!("'{ch}'");
    }
    let code = ch as u32;
    if code < 0x100 {
        format!("'\\x{code:02x}'")
    } else if code < 0x10000 {
        format!("'\\u{code:04x}'")
    } else {
        format!("'\\U{code:08x}'")
    }
}

/// The top-level string-valued fields of a JSON object; other values (the
/// corpus's `expected` objects, numbers, null) are skipped.
fn json_string_fields(line: &str) -> Option<Vec<(String, String)>> {
    let bytes: Vec<char> = line.chars().collect();
    let mut pos = 0;
    let mut fields = Vec::new();
    let skip_space = |pos: &mut usize, bytes: &[char]| {
        while *pos < bytes.len() && bytes[*pos].is_whitespace() {
            *pos += 1;
        }
    };
    skip_space(&mut pos, &bytes);
    if pos >= bytes.len() || bytes[pos] != '{' {
        return None;
    }
    pos += 1;
    loop {
        skip_space(&mut pos, &bytes);
        if pos < bytes.len() && bytes[pos] == '}' {
            return Some(fields);
        }
        let key = parse_json_string(&bytes, &mut pos)?;
        skip_space(&mut pos, &bytes);
        if pos >= bytes.len() || bytes[pos] != ':' {
            return None;
        }
        pos += 1;
        skip_space(&mut pos, &bytes);
        if pos < bytes.len() && bytes[pos] == '"' {
            let value = parse_json_string(&bytes, &mut pos)?;
            fields.push((key, value));
        } else if !skip_json_value(&bytes, &mut pos) {
            return None;
        }
        skip_space(&mut pos, &bytes);
        match bytes.get(pos) {
            Some(',') => pos += 1,
            Some('}') => return Some(fields),
            _ => return None,
        }
    }
}

fn parse_json_string(bytes: &[char], pos: &mut usize) -> Option<String> {
    if *pos >= bytes.len() || bytes[*pos] != '"' {
        return None;
    }
    *pos += 1;
    let mut out = String::new();
    loop {
        let ch = *bytes.get(*pos)?;
        *pos += 1;
        match ch {
            '"' => return Some(out),
            '\\' => {
                let escape = *bytes.get(*pos)?;
                *pos += 1;
                match escape {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000C}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let code = parse_hex4(bytes, *pos)?;
                        *pos += 4;
                        let code = if (0xD800..0xDC00).contains(&code) {
                            // A high surrogate must pair with \uDC00..\uDFFF.
                            if bytes.get(*pos) != Some(&'\\') || bytes.get(*pos + 1) != Some(&'u') {
                                return None;
                            }
                            let low = parse_hex4(bytes, *pos + 2)?;
                            if !(0xDC00..0xE000).contains(&low) {
                                return None;
                            }
                            *pos += 6;
                            0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00)
                        } else if (0xDC00..0xE000).contains(&code) {
                            return None;
                        } else {
                            code
                        };
                        out.push(char::from_u32(code)?);
                    }
                    _ => return None,
                }
            }
            ch => out.push(ch),
        }
    }
}

fn parse_hex4(bytes: &[char], pos: usize) -> Option<u32> {
    let mut code = 0;
    for offset in 0..4 {
        let digit = bytes.get(pos + offset)?.to_digit(16)?;
        code = code * 16 + digit;
    }
    Some(code)
}

/// Walk over a non-string JSON value (object, array, number, literal).
fn skip_json_value(bytes: &[char], pos: &mut usize) -> bool {
    match bytes.get(*pos) {
        Some('{') | Some('[') => {
            let (open, close) = if bytes[*pos] == '{' {
                ('{', '}')
            } else {
                ('[', ']')
            };
            let mut depth = 0;
            while *pos < bytes.len() {
                match bytes[*pos] {
                    ch if ch == open => depth += 1,
                    ch if ch == close => {
                        depth -= 1;
                        if depth == 0 {
                            *pos += 1;
                            return true;
                        }
                    }
                    '"' => {
                        if parse_json_string(bytes, pos).is_none() {
                            return false;
                        }
                        continue;
                    }
                    _ => {}
                }
                *pos += 1;
            }
            false
        }
        Some(_) => {
            while *pos < bytes.len() && !matches!(bytes[*pos], ',' | '}' | ']') {
                *pos += 1;
            }
            true
        }
        None => false,
    }
}

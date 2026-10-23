//! The CPython 3.12 PEG parser: tokens to AST with CPython's locations.

mod expressions;
mod helpers;
mod invalid;
mod patterns;
mod statements;
mod strings;
mod unicode;
mod unicode_names;
mod unicode_norm;

use crate::ast_generated::{Expr, ExprContext, Mod};
use crate::intern::{Interner, Sym};
use crate::tokenizer as tok;
use crate::tokenizer::{
    PegToken, TokenKind, TokenizeError, TokenizeErrorKind, Tokenizer, parser_source,
};

/// The exception type a failed parse raises, as `front_diff` sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExcKind {
    Syntax,
    Indentation,
    Tab,
    Memory,
    Overflow,
    /// An error CPython propagates unwrapped, outside the SyntaxError family.
    Unwrapped,
}

impl ExcKind {
    pub fn name(self) -> &'static str {
        match self {
            ExcKind::Syntax => "SyntaxError",
            ExcKind::Indentation => "IndentationError",
            ExcKind::Tab => "TabError",
            ExcKind::Memory => "MemoryError",
            ExcKind::Overflow => "OverflowError",
            ExcKind::Unwrapped => "UnicodeDecodeError",
        }
    }

    /// `PyErr_ExceptionMatches(PyExc_SyntaxError)`: the indentation errors
    /// subclass SyntaxError.
    fn matches_syntax(self) -> bool {
        matches!(self, ExcKind::Syntax | ExcKind::Indentation | ExcKind::Tab)
    }
}

/// A pending exception: the parse result on failure. Offsets are 1-based
/// character offsets, `None` where CPython raises without a location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub kind: ExcKind,
    pub message: String,
    pub lineno: Option<i32>,
    pub offset: Option<i32>,
    pub end_lineno: Option<i32>,
    pub end_offset: Option<i32>,
}

/// A syntax error raised before the parser runs (the input pipeline).
pub(crate) fn source_error(error: TokenizeError) -> ParseError {
    let kind = match error.kind {
        // The tokenize view reports unsupported cookies as SyntaxError;
        // decode_str surfaces the same error without a location.
        TokenizeErrorKind::UnsupportedEncoding | TokenizeErrorKind::SyntaxError => ExcKind::Syntax,
        _ => ExcKind::Syntax,
    };
    let located = error
        .line
        .map(|line| line as i32)
        .zip(error.column.map(|c| c as i32));
    ParseError {
        kind,
        message: error.message,
        lineno: located.map(|(l, _)| l),
        offset: located.map(|(_, c)| c),
        end_lineno: None,
        end_offset: None,
    }
}

/// `RAISE_ERROR_KNOWN_LOCATION`'s `CURRENT_POS` sentinel.
const CURRENT_POS: i64 = -5;

/// `_PyPegen`'s recursion guard; the generated rules overflow at this depth.
const MAXSTACK: i32 = 6000;

/// A node's source span: byte columns until `ParseError` rendering, exactly
/// the values the AST carries.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pos {
    pub lineno: i32,
    pub col_offset: i32,
    pub end_lineno: Option<i32>,
    pub end_col_offset: Option<i32>,
}

impl Pos {
    fn new(lineno: u32, col: i64, end_lineno: u32, end_col: i64) -> Self {
        Pos {
            lineno: lineno as i32,
            col_offset: col as i32,
            end_lineno: Some(end_lineno as i32),
            end_col_offset: Some(end_col as i32),
        }
    }

    fn from_token(t: &PegToken) -> Self {
        Pos::new(t.lineno, t.col_offset, t.end_lineno, t.end_col_offset)
    }

    fn from_expr(e: &Expr) -> Self {
        match e {
            Expr::BoolOp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::NamedExpr {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::BinOp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::UnaryOp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Lambda {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::IfExp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Dict {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Set {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::ListComp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::SetComp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::DictComp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::GeneratorExp {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Await {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Yield {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::YieldFrom {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Compare {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Call {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::FormattedValue {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::JoinedStr {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Constant {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Attribute {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Subscript {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Starred {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Name {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::List {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Tuple {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            }
            | Expr::Slice {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => Pos {
                lineno: *lineno,
                col_offset: *col_offset,
                end_lineno: *end_lineno,
                end_col_offset: *end_col_offset,
            },
        }
    }
}

/// One memoized rule's table: `(end mark, result)` per start mark, `None` in
/// the slot meaning a memoized failure.
struct Memo<T> {
    slots: Vec<Option<(usize, Option<T>)>>,
}

impl<T> Default for Memo<T> {
    fn default() -> Self {
        Memo { slots: Vec::new() }
    }
}

impl<T: Clone> Memo<T> {
    fn clear(&mut self) {
        self.slots.clear();
    }

    fn get(&self, mark: usize) -> Option<(usize, Option<T>)> {
        self.slots.get(mark).cloned().flatten()
    }

    fn insert(&mut self, mark: usize, end: usize, res: Option<T>) {
        if self.slots.len() <= mark {
            self.slots.resize(mark + 1, None);
        }
        self.slots[mark] = Some((end, res));
    }

    /// `_PyPegen_update_memo`'s insert-or-replace, checked by the existence of
    /// an entry.
    fn update(&mut self, mark: usize, end: usize, res: Option<T>) {
        self.insert(mark, end, res);
    }
}

#[derive(Default)]
struct Memos {
    // Plain memoized rules.
    arguments: Memo<Expr>,
    await_primary: Memo<Expr>,
    block: Memo<Vec<crate::ast_generated::Stmt>>,
    closed_pattern: Memo<crate::ast_generated::Pattern>,
    conjunction: Memo<Expr>,
    del_target: Memo<Expr>,
    disjunction: Memo<Expr>,
    expression: Memo<Expr>,
    factor: Memo<Expr>,
    invalid_named_expression: Memo<()>,
    inversion: Memo<Expr>,
    simple_stmt: Memo<crate::ast_generated::Stmt>,
    star_expression: Memo<Expr>,
    star_pattern: Memo<crate::ast_generated::Pattern>,
    star_target: Memo<Expr>,
    strings: Memo<Expr>,
    target_with_star_atom: Memo<Expr>,
    type_param: Memo<crate::ast_generated::TypeParam>,
    // Left-recursive rules, grown with the seed algorithm.
    attr: Memo<Expr>,
    bitwise_or: Memo<Expr>,
    bitwise_xor: Memo<Expr>,
    bitwise_and: Memo<Expr>,
    shift_expr: Memo<Expr>,
    dotted_name: Memo<Expr>,
    t_primary: Memo<Expr>,
    primary: Memo<Expr>,
    sum: Memo<Expr>,
    term: Memo<Expr>,
}

impl Memos {
    fn clear(&mut self) {
        self.arguments.clear();
        self.await_primary.clear();
        self.block.clear();
        self.closed_pattern.clear();
        self.conjunction.clear();
        self.del_target.clear();
        self.disjunction.clear();
        self.expression.clear();
        self.factor.clear();
        self.invalid_named_expression.clear();
        self.inversion.clear();
        self.simple_stmt.clear();
        self.star_expression.clear();
        self.star_pattern.clear();
        self.star_target.clear();
        self.strings.clear();
        self.target_with_star_atom.clear();
        self.type_param.clear();
        self.attr.clear();
        self.bitwise_or.clear();
        self.bitwise_xor.clear();
        self.bitwise_and.clear();
        self.shift_expr.clear();
        self.dotted_name.clear();
        self.t_primary.clear();
        self.primary.clear();
        self.sum.clear();
        self.term.clear();
    }
}

pub(crate) struct Parser {
    tokenizer: Tokenizer,
    tokens: Vec<PegToken>,
    pub(crate) mark: usize,
    fill: usize,
    pub(crate) level: i32,
    pub(crate) call_invalid_rules: bool,
    pub(crate) error: Option<ParseError>,
    pub(crate) interner: Interner,

    // The input pipeline's leftovers the error machinery reads.
    tok_str: Vec<u8>,
    filename: Option<std::path::PathBuf>,
    encoding: Option<String>,

    /// The token the string parser pointed an error at, as
    /// `p->known_err_token` records it.
    pub(crate) known_err_token: Option<usize>,

    type_ignore_comments: Vec<(u32, Vec<u8>)>,

    memos: Memos,
}

impl Parser {
    fn new(
        tokenizer: Tokenizer,
        tok_str: Vec<u8>,
        encoding: Option<String>,
        filename: Option<std::path::PathBuf>,
    ) -> Self {
        Parser {
            tokenizer,
            tokens: Vec::new(),
            mark: 0,
            fill: 0,
            level: 0,
            call_invalid_rules: false,
            error: None,
            interner: Interner::new(),
            tok_str,
            filename,
            encoding,
            known_err_token: None,
            type_ignore_comments: Vec::new(),
            memos: Memos::default(),
        }
    }

    // -- token stream ---------------------------------------------------------

    /// `_PyPegen_fill_token`: pull one token. `false` leaves an error pending,
    /// mirroring `initialize_token`'s failure paths.
    fn fill_token(&mut self) -> bool {
        let mut token = self.tokenizer.next_peg_token();
        // '# type: ignore' comments never reach the grammar; `make_module`
        // turns the recorded pairs into the module's type_ignores.
        while token.kind == TokenKind::TypeIgnore {
            self.type_ignore_comments
                .push((token.lineno, token.bytes.to_vec()));
            token = self.tokenizer.next_peg_token();
        }
        if token.kind == TokenKind::ErrorToken {
            self.tokens.push(token);
            self.fill += 1;
            return self.tokenizer_error();
        }
        self.tokens.push(token);
        self.fill += 1;
        true
    }

    /// Ensure the token at `mark` exists.
    pub(crate) fn peek(&mut self) -> bool {
        if self.mark == self.fill {
            return self.fill_token();
        }
        !self.has_error()
    }

    pub(crate) fn has_error(&self) -> bool {
        self.error.is_some()
    }

    pub(crate) fn tok(&self) -> &PegToken {
        &self.tokens[self.mark]
    }

    pub(crate) fn tok_at(&self, index: usize) -> &PegToken {
        &self.tokens[index]
    }

    pub(crate) fn advance(&mut self) {
        self.mark += 1;
    }

    /// `_PyPegen_expect_token` on a token kind.
    pub(crate) fn expect(&mut self, kind: TokenKind) -> bool {
        if !self.peek() || self.tok().kind != kind {
            return false;
        }
        self.advance();
        true
    }

    /// `expect` as an alternative step: `None` fails the enclosing attempt.
    pub(crate) fn expect_opt(&mut self, kind: TokenKind) -> Option<()> {
        self.expect(kind).then_some(())
    }

    /// `_PyPegen_expect_token` on a keyword (a `NAME` with the keyword's
    /// bytes, the type `_get_keyword_or_name_type` refines at fill time).
    pub(crate) fn expect_keyword(&mut self, keyword: &str) -> bool {
        if !self.peek()
            || self.tok().kind != TokenKind::Name
            || self.tok().bytes.as_ref() != keyword.as_bytes()
        {
            return false;
        }
        self.advance();
        true
    }

    /// `_PyPegen_expect_soft_keyword`.
    pub(crate) fn expect_soft_keyword(&mut self, keyword: &str) -> bool {
        self.expect_keyword(keyword)
    }

    /// The keyword and soft-keyword matches as alternative steps.
    pub(crate) fn expect_keyword_opt(&mut self, keyword: &str) -> Option<()> {
        self.expect_keyword(keyword).then_some(())
    }

    pub(crate) fn expect_soft_keyword_opt(&mut self, keyword: &str) -> Option<()> {
        self.expect_soft_keyword(keyword).then_some(())
    }

    /// `_PyPegen_name_token`: a `Name(Load)` node from a `NAME` that
    /// `_get_keyword_or_name_type` left as a name.
    pub(crate) fn name_token(&mut self) -> Option<Expr> {
        if !self.peek() || self.tok().kind != TokenKind::Name || is_hard_keyword(&self.tok().bytes)
        {
            return None;
        }
        let t = self.tok().clone();
        self.advance();
        self.name_from_token(&t)
    }

    /// `_PyPegen_name_from_token` + `_PyPegen_new_identifier`: NFKC on
    /// non-ASCII names.
    pub(crate) fn name_from_token(&mut self, t: &PegToken) -> Option<Expr> {
        let id = self.new_identifier(&t.bytes)?;
        Some(Expr::Name {
            id,
            ctx: Box::new(ExprContext::Load),
            lineno: t.lineno as i32,
            col_offset: t.col_offset as i32,
            end_lineno: Some(t.end_lineno as i32),
            end_col_offset: Some(t.end_col_offset as i32),
        })
    }

    pub(crate) fn new_identifier(&mut self, bytes: &[u8]) -> Option<Sym> {
        // The tokenizer validated the NAME; its bytes decode to UTF-8 unless
        // the whole token failed, which never reaches here.
        let text = std::str::from_utf8(bytes).ok()?;
        let code_points: Vec<u32> = text.chars().map(|c| c as u32).collect();
        let normalized = if text.is_ascii() {
            code_points
        } else {
            unicode::nfkc(&code_points)
        };
        Some(self.interner.intern_code_points(normalized))
    }

    // -- rule plumbing --------------------------------------------------------

    /// The recursion guard's overflow; the caller returns without unwinding
    /// its level, exactly as the generated code does.
    pub(crate) fn stack_overflow(&mut self) {
        self.error = Some(ParseError {
            kind: ExcKind::Memory,
            message: "Parser stack overflowed - Python source too complex to parse".to_string(),
            lineno: None,
            offset: None,
            end_lineno: None,
            end_offset: None,
        });
    }

    /// A generated rule's entry: the overflow check, then the error check
    /// that also catches the overflow it raises.
    pub(crate) fn enter_rule(&mut self) -> bool {
        self.level += 1;
        if self.level == MAXSTACK + 1 {
            self.stack_overflow();
        }
        if self.error.is_some() {
            self.level -= 1;
            return false;
        }
        true
    }

    pub(crate) fn exit_rule(&mut self) {
        self.level -= 1;
    }

    /// One alternative: run it from the current mark, restoring the mark on
    /// failure as the generated `p->mark = _mark` does.
    pub(crate) fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let mark = self.mark;
        match f(self) {
            Some(value) => Some(value),
            None => {
                self.mark = mark;
                None
            }
        }
    }

    /// The rule's alternatives in grammar order: each failure restores the
    /// mark and, once an error is pending, none of the rest run.
    pub(crate) fn alternatives<T>(&mut self, alts: &[fn(&mut Self) -> Option<T>]) -> Option<T> {
        for alt in alts {
            if self.error.is_some() {
                return None;
            }
            if let Some(res) = self.attempt(*alt) {
                return Some(res);
            }
        }
        None
    }

    /// A loop item's boundary: the loop rule resets to here when the item
    /// fails, leaving the separator unconsumed.
    pub(crate) fn repeat0<T>(&mut self, item: fn(&mut Self) -> Option<T>) -> Vec<T> {
        let mut out = Vec::new();
        loop {
            let mark = self.mark;
            match item(self) {
                Some(value) => out.push(value),
                None => {
                    self.mark = mark;
                    break;
                }
            }
        }
        out
    }

    pub(crate) fn repeat1<T>(&mut self, item: fn(&mut Self) -> Option<T>) -> Option<Vec<T>> {
        let first = item(self)?;
        let mut out = vec![first];
        loop {
            let mark = self.mark;
            match item(self) {
                Some(value) => out.push(value),
                None => {
                    self.mark = mark;
                    break;
                }
            }
        }
        Some(out)
    }

    /// `('s' item)+` after a separately parsed first element: one mandatory
    /// `s item`, then as many as match, each failed pair rewound together.
    pub(crate) fn sep_repeat1<T>(
        &mut self,
        item: fn(&mut Self) -> Option<T>,
        sep: fn(&mut Self) -> bool,
    ) -> Option<Vec<T>> {
        let mark = self.mark;
        if !sep(self) {
            self.mark = mark;
            return None;
        }
        let first = item(self)?;
        let mut out = vec![first];
        loop {
            let mark = self.mark;
            if !sep(self) {
                self.mark = mark;
                break;
            }
            match item(self) {
                Some(value) => out.push(value),
                None => {
                    self.mark = mark;
                    break;
                }
            }
        }
        Some(out)
    }

    /// `s.e+` as the generated gather: `e (s e)*`, the separator rewound when
    /// the item after it fails.
    pub(crate) fn gather<T>(
        &mut self,
        item: fn(&mut Self) -> Option<T>,
        sep: fn(&mut Self) -> bool,
    ) -> Option<Vec<T>> {
        let first = item(self)?;
        let mut out = vec![first];
        loop {
            let mark = self.mark;
            if !sep(self) {
                self.mark = mark;
                break;
            }
            match item(self) {
                Some(value) => out.push(value),
                None => {
                    self.mark = mark;
                    break;
                }
            }
        }
        Some(out)
    }

    /// A positive lookahead on a token kind.
    pub(crate) fn lookahead(&mut self, kind: TokenKind) -> bool {
        let mark = self.mark;
        let matched = self.expect(kind);
        self.mark = mark;
        matched
    }

    /// A negative lookahead on a token kind.
    pub(crate) fn not_lookahead(&mut self, kind: TokenKind) -> bool {
        !self.lookahead(kind)
    }

    /// A positive lookahead on a keyword.
    pub(crate) fn lookahead_keyword(&mut self, keyword: &str) -> bool {
        let mark = self.mark;
        let matched = self.expect_keyword(keyword);
        self.mark = mark;
        matched
    }

    /// A positive lookahead on a soft keyword.
    pub(crate) fn lookahead_soft_keyword(&mut self, keyword: &str) -> bool {
        let mark = self.mark;
        let matched = self.expect_soft_keyword(keyword);
        self.mark = mark;
        matched
    }

    /// A negative lookahead on a soft keyword.
    pub(crate) fn not_lookahead_soft_keyword(&mut self, keyword: &str) -> bool {
        !self.lookahead_soft_keyword(keyword)
    }

    /// A positive lookahead on a rule.
    pub(crate) fn lookahead_rule<T>(&mut self, rule: fn(&mut Self) -> Option<T>) -> bool {
        let mark = self.mark;
        let matched = rule(self).is_some();
        self.mark = mark;
        matched
    }

    /// A negative lookahead on a rule.
    pub(crate) fn not_lookahead_rule<T>(&mut self, rule: fn(&mut Self) -> Option<T>) -> bool {
        !self.lookahead_rule(rule)
    }

    /// `_PyPegen_expect_forced_token`: match or raise "expected 'x'" at the
    /// current token.
    pub(crate) fn expect_forced(&mut self, kind: TokenKind, expected: &str) -> bool {
        if self.error.is_some() {
            return false;
        }
        if !self.peek() {
            return false;
        }
        let t = self.tok().clone();
        if t.kind != kind {
            self.raise_known_location(Pos::from_token(&t), format!("expected '{expected}'"));
            return false;
        }
        self.advance();
        true
    }

    /// The `EXTRA` start: the rule's start token position.
    pub(crate) fn pos_at(&self, mark: usize) -> (i32, i32) {
        let t = &self.tokens[mark];
        (t.lineno as i32, t.col_offset as i32)
    }

    // -- memoization ------------------------------------------------------------

    /// `_PyPegen_is_memoized`: a hit moves the mark and yields the stored
    /// result, which may be a memoized failure.
    fn memo_get<T: Clone>(
        &mut self,
        table: impl Fn(&mut Memos) -> &mut Memo<T>,
    ) -> Option<Option<T>> {
        if self.mark == self.fill && !self.fill_token() {
            return Some(None);
        }
        match table(&mut self.memos).get(self.mark) {
            None => None,
            Some((end, res)) => {
                self.mark = end;
                Some(res)
            }
        }
    }

    /// `_PyPegen_insert_memo` at rule success or failure.
    fn memo_insert<T: Clone>(
        &mut self,
        table: impl Fn(&mut Memos) -> &mut Memo<T>,
        mark: usize,
        res: Option<T>,
    ) {
        table(&mut self.memos).insert(mark, self.mark, res);
    }

    /// `_PyPegen_update_memo` in the growing-seed loop.
    fn memo_update<T: Clone>(
        &mut self,
        table: impl Fn(&mut Memos) -> &mut Memo<T>,
        mark: usize,
        res: Option<T>,
    ) {
        table(&mut self.memos).update(mark, self.mark, res);
    }

    /// The wrapper of a left-recursive rule: seed with failure, then grow
    /// while the raw rule reaches further.
    fn left_recursion<T: Clone>(
        &mut self,
        table: impl Fn(&mut Memos) -> &mut Memo<T> + Copy,
        raw: impl Fn(&mut Self) -> Option<T>,
    ) -> Option<T> {
        self.level += 1;
        if self.level == MAXSTACK + 1 {
            self.stack_overflow();
        }
        let mut res: Option<T> = None;
        if let Some(hit) = self.memo_get(table) {
            self.level -= 1;
            return hit;
        }
        let start = self.mark;
        let mut resmark = start;
        loop {
            self.memo_update(table, start, res.clone());
            self.mark = start;
            let grown = raw(self);
            if self.error.is_some() {
                self.level -= 1;
                return None;
            }
            let Some(grown) = grown else { break };
            if self.mark <= resmark {
                break;
            }
            resmark = self.mark;
            res = Some(grown);
        }
        self.mark = resmark;
        self.level -= 1;
        res
    }

    /// `_PyPegen_get_last_nonnwhitespace_token` as an index.
    fn last_nonnws(&self) -> Option<usize> {
        let mut previous = self.mark as isize - 1;
        while previous >= 0 {
            match self.tokens[previous as usize].kind {
                TokenKind::Endmarker
                | TokenKind::Newline
                | TokenKind::Indent
                | TokenKind::Dedent => previous -= 1,
                _ => return Some(previous as usize),
            }
        }
        None
    }

    /// The `EXTRA` span: start token through the last non-whitespace token.
    /// `None` mirrors the generated NULL check, a silent rule failure.
    pub(crate) fn extra(&self, start: (i32, i32)) -> Option<Pos> {
        let end = self.last_nonnws()?;
        let t = &self.tokens[end];
        Some(Pos {
            lineno: start.0,
            col_offset: start.1,
            end_lineno: Some(t.end_lineno as i32),
            end_col_offset: Some(t.end_col_offset as i32),
        })
    }

    /// A token's own span, for the actions that use the token directly.
    pub(crate) fn token_pos(&self, index: usize) -> Pos {
        Pos::from_token(&self.tokens[index])
    }
}

// -- error machinery ------------------------------------------------------------

impl Parser {
    /// `_PyPegen_raise_error_known_location`: byte columns become characters
    /// through the error line. `None` end columns stay `None`.
    fn raise_error_known_location(
        &mut self,
        kind: ExcKind,
        lineno: i64,
        col: i64,
        end_lineno: i64,
        end_col: i64,
        message: String,
    ) {
        if self.error.is_some() {
            return;
        }
        let error_line = self.error_line(lineno);
        let offset = byte_to_char(&error_line, col);
        let end_offset = if end_col >= 0 {
            Some(byte_to_char(&error_line, end_col))
        } else {
            None
        };
        self.error = Some(ParseError {
            kind,
            message,
            lineno: Some(lineno as i32),
            offset: Some(offset as i32),
            end_lineno: if end_lineno > 0 {
                Some(end_lineno as i32)
            } else {
                None
            },
            end_offset: end_offset.map(|c| c as i32),
        });
    }

    /// The `RAISE_ERROR_KNOWN_LOCATION` macro: `CURRENT_POS` resolves from the
    /// tokenizer, other columns shift to their 1-based form.
    pub(crate) fn macro_raise(
        &mut self,
        kind: ExcKind,
        lineno: i64,
        col: i64,
        end_lineno: i64,
        end_col: i64,
        message: String,
    ) -> bool {
        let col = if col == CURRENT_POS {
            self.tokenizer.cur_col_offset()
        } else {
            col + 1
        };
        let end_col = if end_col == CURRENT_POS {
            self.tokenizer.cur_col_offset()
        } else {
            end_col + 1
        };
        let end_lineno = if end_lineno == CURRENT_POS {
            self.tokenizer.cur_lineno() as i64
        } else {
            end_lineno
        };
        self.raise_error_known_location(kind, lineno, col, end_lineno, end_col, message);
        false
    }

    /// `_PyPegen_raise_error`: the position comes from a token, the
    /// tokenizer's cursor when the token is synthetic.
    fn raise_error(&mut self, kind: ExcKind, use_mark: bool, message: String) -> bool {
        if self.error.is_some() {
            return false;
        }
        if self.fill == 0 {
            self.raise_error_known_location(kind, 0, 0, 0, -1, message);
            return false;
        }
        if use_mark && self.mark == self.fill && !self.fill_token() {
            return false;
        }
        let index =
            self.known_err_token
                .unwrap_or(if use_mark { self.mark } else { self.fill - 1 });
        let t = self.tokens[index].clone();
        let col = if t.col_offset == -1 {
            self.tokenizer.cur_col_offset()
        } else {
            t.col_offset + 1
        };
        let end_col = if t.end_col_offset != -1 {
            t.end_col_offset + 1
        } else {
            -1
        };
        self.raise_error_known_location(
            kind,
            t.lineno as i64,
            col,
            t.end_lineno as i64,
            end_col,
            message,
        );
        false
    }

    pub(crate) fn raise_syntax_error(&mut self, message: String) -> bool {
        self.raise_error(ExcKind::Syntax, false, message)
    }

    /// `RAISE_INDENTATION_ERROR`.
    pub(crate) fn raise_indentation_error(&mut self, message: String) -> bool {
        self.raise_error(ExcKind::Indentation, false, message)
    }

    pub(crate) fn raise_syntax_error_on_next_token(&mut self, message: String) -> bool {
        self.raise_error(ExcKind::Syntax, true, message)
    }

    /// `RAISE_SYNTAX_ERROR_KNOWN_RANGE(a, b, ...)`.
    pub(crate) fn raise_known_range(&mut self, a: Pos, b: Pos, message: String) -> bool {
        self.macro_raise(
            ExcKind::Syntax,
            a.lineno as i64,
            a.col_offset as i64,
            b.end_lineno.unwrap_or(a.lineno) as i64,
            b.end_col_offset.unwrap_or(a.col_offset) as i64,
            message,
        );
        false
    }

    /// `RAISE_SYNTAX_ERROR_KNOWN_LOCATION(a, ...)`.
    pub(crate) fn raise_known_location(&mut self, a: Pos, message: String) -> bool {
        self.raise_known_range(a, a, message)
    }

    /// `RAISE_SYNTAX_ERROR_STARTING_FROM(a, ...)`: the end runs to the
    /// tokenizer's cursor.
    pub(crate) fn raise_starting_from(&mut self, a: Pos, message: String) -> bool {
        self.macro_raise(
            ExcKind::Syntax,
            a.lineno as i64,
            a.col_offset as i64,
            CURRENT_POS,
            CURRENT_POS,
            message,
        );
        false
    }

    /// `_Pypegen_raise_decode_error`: the pending `UnicodeDecodeError` from
    /// `E_DECODE`, wrapped in a SyntaxError. String-literal decoding reaches
    /// the same wrapper with `value error` for escape failures in bytes.
    pub(crate) fn raise_decode_error(&mut self, prefix: &str, message: String) -> bool {
        let detail = format!("({prefix}) {message}");
        self.raise_error(ExcKind::Syntax, false, detail)
    }

    /// The same raise from a string literal's action: the reported range ends
    /// at the closing quote rather than past it.
    pub(crate) fn raise_string_decode_error(&mut self, prefix: &str, message: String) -> bool {
        let detail = format!("({prefix}) {message}");
        if self.error.is_some() {
            return false;
        }
        if self.fill == 0 {
            self.raise_error_known_location(ExcKind::Syntax, 0, 0, 0, -1, detail);
            return false;
        }
        let t = self.tokens[self.fill - 1].clone();
        let col = if t.col_offset == -1 {
            self.tokenizer.cur_col_offset()
        } else {
            t.col_offset + 1
        };
        let end_col = if t.end_col_offset != -1 {
            t.end_col_offset - 1
        } else {
            -1
        };
        self.raise_error_known_location(
            ExcKind::Syntax,
            t.lineno as i64,
            col,
            t.end_lineno as i64,
            end_col,
            detail,
        );
        false
    }

    /// `_Pypegen_tokenizer_error` plus `raise_decode_error`, the two paths
    /// `initialize_token` takes for an `ERRORTOKEN`.
    fn tokenizer_error(&mut self) -> bool {
        if let Some(message) = self.tokenizer.decode_error() {
            return self.raise_decode_error("unicode error", message);
        }
        // A tokenizer `_syntaxerror_range` carries its own character
        // positions and propagates unchanged.
        if let Some((message, line, column, end_column)) = self.tokenizer.pending_syntax_error() {
            self.error = Some(ParseError {
                kind: ExcKind::Syntax,
                message,
                lineno: Some(line as i32),
                offset: Some(column as i32),
                end_lineno: Some(line as i32),
                end_offset: end_column.map(|c| c as i32),
            });
            return false;
        }
        let lineno = self.tokenizer.cur_lineno() as i64;
        match self.tokenizer.failure() {
            TFailure::Eof => {
                if let Some((paren_lineno, paren_col, bracket)) = self.tokenizer.paren_top() {
                    return self.macro_raise(
                        ExcKind::Syntax,
                        paren_lineno as i64,
                        paren_col,
                        paren_lineno as i64,
                        -1,
                        format!("'{}' was never closed", bracket as char),
                    );
                }
                self.raise_error(
                    ExcKind::Syntax,
                    false,
                    "unexpected EOF while parsing".to_string(),
                )
            }
            TFailure::Dedent => self.raise_error(
                ExcKind::Indentation,
                false,
                "unindent does not match any outer indentation level".to_string(),
            ),
            TFailure::TabSpace => self.macro_raise(
                ExcKind::Tab,
                lineno,
                0,
                lineno,
                -1,
                "inconsistent use of tabs and spaces in indentation".to_string(),
            ),
            TFailure::Toodeep => self.macro_raise(
                ExcKind::Indentation,
                lineno,
                0,
                lineno,
                -1,
                "too many levels of indentation".to_string(),
            ),
            TFailure::LineCont => {
                let col = self.tokenizer.cur_offset() - 1;
                self.macro_raise(
                    ExcKind::Syntax,
                    lineno,
                    col,
                    lineno,
                    -1,
                    "unexpected character after line continuation character".to_string(),
                )
            }
            TFailure::ColumnOverflow => {
                self.error = Some(ParseError {
                    kind: ExcKind::Overflow,
                    message: "Parser column offset overflow - source line is too big".to_string(),
                    lineno: None,
                    offset: None,
                    end_lineno: None,
                    end_offset: None,
                });
                false
            }
            TFailure::Token => self.macro_raise(
                ExcKind::Syntax,
                lineno,
                0,
                lineno,
                -1,
                "invalid token".to_string(),
            ),
        }
    }

    // -- the error line --------------------------------------------------------

    /// `_PyPegen_raise_error_known_location`'s error line: the file's line for
    /// file input, with the tokenizer buffers as fallbacks.
    fn error_line(&self, lineno: i64) -> Vec<u8> {
        if lineno > 0
            && let Some(line) = self.file_error_line(lineno)
        {
            return line;
        }
        if self.tokenizer.cur_lineno() as i64 <= lineno && self.tokenizer.has_input() {
            // `PyUnicode_DecodeUTF8(line_start, inp - line_start, "replace")`:
            // the fallback lines are decoded text before the byte-to-character
            // conversion runs on them.
            return String::from_utf8_lossy(self.tokenizer.current_line())
                .into_owned()
                .into_bytes();
        }
        self.tokenizer_buffer_line(lineno)
    }

    /// `get_error_line_from_tokenizer_buffers`: walk `tok->str`.
    fn tokenizer_buffer_line(&self, lineno: i64) -> Vec<u8> {
        let buffer = &self.tok_str;
        let mut cur_line = 0usize;
        for _ in 0..lineno.saturating_sub(1) {
            match buffer[cur_line..].iter().position(|&b| b == b'\n') {
                Some(offset) => cur_line += offset + 1,
                None => break,
            }
        }
        let end = buffer[cur_line..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(buffer.len(), |offset| cur_line + offset);
        String::from_utf8_lossy(&buffer[cur_line..end])
            .into_owned()
            .into_bytes()
    }

    /// `_PyErr_ProgramDecodedTextObject` + `err_programtext`: the line as the
    /// file on disk holds it, in up-to-999-byte chunks, decoded through the
    /// tokenizer's encoding.
    fn file_error_line(&self, lineno: i64) -> Option<Vec<u8>> {
        let filename = self.filename.as_ref()?;
        let bytes = std::fs::read(filename).ok()?;
        // `_PyErr_ProgramDecodedTextObject`: no tokenizer encoding falls
        // back to the cookie, then to utf-8.
        let encoding = self
            .encoding
            .clone()
            .or_else(|| find_encoding_filename(&bytes))
            .unwrap_or_else(|| "utf-8".to_string());
        err_programtext(&bytes, lineno, &encoding)
    }
}

/// The tokenizer failure behind an `ERRORTOKEN`, in `_Pypegen_tokenizer_error`'s
/// order.
pub(crate) use crate::tokenizer::TokenFailure as TFailure;

// -- error-line sources ---------------------------------------------------------

/// `_PyPegen_byte_offset_to_character_offset_raw`: the code points in the
/// first `col` bytes of the line, with the line's NUL terminator counting as
/// one character past its end.
fn byte_to_char(line: &[u8], col: i64) -> i64 {
    let len = line.len() as i64;
    let col = if col > len + 1 { len + 1 } else { col };
    let count = |bytes: &[u8]| String::from_utf8_lossy(bytes).chars().count() as i64;
    if col > len {
        // CPython decodes one byte past the buffer: the NUL terminator.
        count(line) + 1
    } else if col >= 0 {
        count(&line[..col as usize])
    } else {
        0
    }
}

/// `err_programtext`: the line as `fgets` chunks read it — up to 999 bytes,
/// stopping at a newline — with universal newlines and a line-1 BOM skip,
/// decoded through the encoding with replacement.
fn err_programtext(bytes: &[u8], lineno: i64, encoding: &str) -> Option<Vec<u8>> {
    let mut pos = 0usize;
    let mut chunk: Option<Vec<u8>> = None;
    let mut i = 0i64;
    while i < lineno {
        // One fgets step: up to 999 bytes or through the newline.
        if pos >= bytes.len() {
            return None;
        }
        let mut end = (pos + 999).min(bytes.len());
        if let Some(offset) = bytes[pos..end].iter().position(|&b| b == b'\n') {
            end = pos + offset + 1;
        }
        let translated = translate_chunk_newlines(&bytes[pos..end]);
        pos = end;
        let full = translated.len() == 999 && translated.last() != Some(&b'\n');
        if i + 1 < lineno && full {
            continue;
        }
        chunk = Some(translated);
        i += 1;
    }
    let mut line = chunk?;
    if lineno == 1 && line.starts_with(&[0xEF, 0xBB, 0xBF]) {
        line.drain(..3);
    }
    Some(decode_replace(&line, encoding))
}

/// The universal-newline translation `_Py_UniversalNewlineFgetsWithSize`
/// applies inside a chunk.
fn translate_chunk_newlines(chunk: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(chunk.len());
    let mut i = 0;
    while i < chunk.len() {
        match chunk[i] {
            b'\r' => {
                out.push(b'\n');
                if i + 1 < chunk.len() && chunk[i + 1] == b'\n' {
                    i += 1;
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    out
}

/// `PyUnicode_Decode(line, size, encoding, "replace")` for the codecs this
/// front end carries.
fn decode_replace(line: &[u8], encoding: &str) -> Vec<u8> {
    let codec = match tok::lookup_codec(encoding) {
        Some(codec) => codec,
        // err_programtext gives up when the codec fails to import.
        None => return Vec::new(),
    };
    // `PyUnicode_Decode(.., "replace")` decodes the whole line at once:
    // one U+FFFD per maximal invalid subpart, not per byte.
    if codec == tok::Codec::Utf8 {
        return String::from_utf8_lossy(line).to_string().into_bytes();
    }
    let mut out = String::new();
    for &b in line {
        match codec {
            tok::Codec::Ascii => {
                if b < 128 {
                    out.push(b as char)
                } else {
                    out.push('\u{FFFD}')
                }
            }
            _ => out.push(tok::decode_byte(codec, b)),
        }
    }
    out.into_bytes()
}

/// `_PyTokenizer_FindEncodingFilename` over the raw file: a BOM, or the
/// cookie of the first two lines, validated like `fp_setreadl`'s codec
/// import.
fn find_encoding_filename(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return Some("utf-8".to_string());
    }
    let mut starts = [0, bytes.len(), bytes.len()];
    let mut found = 1;
    for (i, &b) in bytes.iter().enumerate() {
        if found < 3 && b == b'\n' {
            starts[found] = i + 1;
            found += 1;
        }
    }
    let line = |n: usize| -> &[u8] {
        if n >= found {
            &[]
        } else {
            &bytes[starts[n]..starts[n + 1].min(bytes.len())]
        }
    };
    // The file path passes the line with its newline, as strlen measures it.
    let cookie = |text: &[u8]| -> Option<String> {
        tok::get_coding_spec(text).filter(|name| tok::lookup_codec(name).is_some())
    };
    if let Some(name) = cookie(line(0)) {
        return Some(name);
    }
    let junk = |text: &[u8]| {
        text.iter()
            .take_while(|&&b| !matches!(b, b'#' | b'\n' | b'\r'))
            .any(|&b| !matches!(b, b' ' | b'\t' | 0x0C))
    };
    let line0 = line(0);
    if !line0.is_empty()
        && !junk(line0)
        && let Some(name) = cookie(line(1))
    {
        return Some(name);
    }
    None
}

// -- keywords -------------------------------------------------------------------

/// The reserved keywords of `reserved_keywords`, those `_get_keyword_or_name_type`
/// types away from `NAME` (async and await the tokenizer types itself).
pub(crate) fn is_hard_keyword(bytes: &[u8]) -> bool {
    matches!(
        bytes,
        b"if"
            | b"as"
            | b"in"
            | b"or"
            | b"is"
            | b"del"
            | b"def"
            | b"for"
            | b"try"
            | b"and"
            | b"not"
            | b"from"
            | b"pass"
            | b"with"
            | b"elif"
            | b"else"
            | b"None"
            | b"True"
            | b"raise"
            | b"yield"
            | b"break"
            | b"class"
            | b"while"
            | b"False"
            | b"return"
            | b"import"
            | b"assert"
            | b"global"
            | b"except"
            | b"lambda"
            | b"finally"
            | b"continue"
            | b"nonlocal"
    )
}

// -- the driver ------------------------------------------------------------------

impl Parser {
    /// `_Pypegen_set_syntax_error`: report the failure of both passes from the
    /// tokenizer's state and the last token the first pass reached.
    fn set_syntax_error(&mut self, last_token: usize) {
        if let Some(error) = self.error.take() {
            // A tokenizer that still reads cleanly defers to a full-source
            // check; other errors propagate as raised.
            let is_tok_ok = self.tokenizer.is_tok_ok();
            if is_tok_ok && error.kind.matches_syntax() {
                self.error = Some(error);
                self.tokenize_full_source();
                return;
            }
            self.error = Some(error);
            return;
        }
        if self.fill == 0 {
            self.raise_error_known_location(
                ExcKind::Syntax,
                0,
                0,
                0,
                -1,
                "error at start before reading any input".to_string(),
            );
            return;
        }
        let last = self.tokens[last_token].clone();
        if last.kind == TokenKind::ErrorToken && self.tokenizer.done_is_eof() {
            if let Some((paren_lineno, paren_col, bracket)) = self.tokenizer.paren_top() {
                self.macro_raise(
                    ExcKind::Syntax,
                    paren_lineno as i64,
                    paren_col,
                    paren_lineno as i64,
                    -1,
                    format!("'{}' was never closed", bracket as char),
                );
            } else {
                self.raise_error(
                    ExcKind::Syntax,
                    false,
                    "unexpected EOF while parsing".to_string(),
                );
            }
            return;
        }
        if matches!(last.kind, TokenKind::Indent | TokenKind::Dedent) {
            let message = if last.kind == TokenKind::Indent {
                "unexpected indent"
            } else {
                "unexpected unindent"
            };
            self.raise_error(ExcKind::Indentation, false, message.to_string());
            return;
        }
        // The generic error sits at the first pass's last token so the second
        // pass's further reach cannot move it; the full-source check may then
        // override it with an earlier tokenizer error. `RAISE_ERROR_KNOWN_LOCATION`
        // adds one even to a synthetic token's `-1` column, unlike the cursor
        // fallback `_PyPegen_raise_error` uses.
        let col = last.col_offset + 1;
        let end_col = last.end_col_offset + 1;
        self.raise_error_known_location(
            ExcKind::Syntax,
            last.lineno as i64,
            col,
            last.end_lineno as i64,
            end_col,
            "invalid syntax".to_string(),
        );
        self.tokenize_full_source();
    }

    /// `_PyPegen_tokenize_full_source_to_check_for_errors`: pull the remaining
    /// tokens; an earlier tokenizer failure replaces the pending error,
    /// except inside an f-string.
    fn tokenize_full_source(&mut self) {
        let saved = self.error.take();
        let current_token = self.known_err_token.unwrap_or(self.fill - 1);
        let current_err_line = self.tokens[current_token].lineno;
        let mut raised: Option<ParseError> = None;
        loop {
            let token = self.tokenizer.next_peg_token();
            if token.kind == TokenKind::ErrorToken {
                if let Some((message, line, column, end_column)) =
                    self.tokenizer.pending_syntax_error()
                {
                    raised = Some(ParseError {
                        kind: ExcKind::Syntax,
                        message,
                        lineno: Some(line as i32),
                        offset: Some(column as i32),
                        end_lineno: Some(line as i32),
                        end_offset: end_column.map(|c| c as i32),
                    });
                } else if let Some(message) = self.tokenizer.decode_error() {
                    // The raw `UnicodeDecodeError` stays pending.
                    raised = Some(ParseError {
                        kind: ExcKind::Unwrapped,
                        message,
                        lineno: None,
                        offset: None,
                        end_lineno: None,
                        end_offset: None,
                    });
                } else if let Some((paren_lineno, paren_col, bracket)) = self.tokenizer.paren_top()
                    && current_err_line > paren_lineno
                {
                    raised = Some(self.paren_error_at(paren_lineno as i64, paren_col, bracket));
                }
                break;
            }
            if token.kind == TokenKind::Endmarker {
                break;
            }
        }
        self.error = if raised.is_some() && !self.tokenizer.in_fstring() {
            raised
        } else {
            saved
        };
    }

    /// The SyntaxError `raise_unclosed_parentheses_error` builds, with the
    /// macro's 1-based columns and its `-1` end column carried as zero.
    fn paren_error_at(&self, paren_lineno: i64, paren_col: i64, bracket: u8) -> ParseError {
        let error_line = self.error_line(paren_lineno);
        ParseError {
            kind: ExcKind::Syntax,
            message: format!("'{}' was never closed", bracket as char),
            lineno: Some(paren_lineno as i32),
            offset: Some(byte_to_char(&error_line, paren_col + 1) as i32),
            end_lineno: Some(paren_lineno as i32),
            end_offset: Some(0),
        }
    }
}

/// `_PyPegen_run_parser` over a whole module: parse, and on failure make the
/// second pass with the `invalid_*` rules before fixing up the error.
///
/// The grammar recurses to its `MAXSTACK` limit, so parsing runs on a thread
/// with a stack deep enough for it.
pub fn parse_module(
    source: &[u8],
    filename: Option<&std::path::Path>,
) -> Result<(Mod, Interner), ParseError> {
    let source = source.to_vec();
    let filename = filename.map(std::path::PathBuf::from);
    std::thread::Builder::new()
        .stack_size(1 << 29)
        .spawn(move || {
            let parsed = Parser::parse_module_bytes(&source, filename.as_deref());
            // The interner must outlive the parser thread's stack frame.
            parsed
        })
        .expect("parser thread")
        .join()
        .expect("parser thread panicked")
}

impl Parser {
    fn parse_module_bytes(
        source: &[u8],
        filename: Option<&std::path::Path>,
    ) -> Result<(Mod, Interner), ParseError> {
        let parser_source = parser_source(source).map_err(crate::parser::source_error)?;
        let mut parser = Parser::new(
            Tokenizer::new(parser_source.lines(), false, false),
            parser_source.tok_str,
            parser_source.encoding,
            filename.map(std::path::PathBuf::from),
        );
        let result = statements::file_rule(&mut parser);
        match result {
            Some(module) => {
                let interner = std::mem::take(&mut parser.interner);
                Ok((module, interner))
            }
            None => {
                if let Some(error) = parser.error.take() {
                    if !error.kind.matches_syntax() {
                        // Memory, overflow and decode errors propagate as
                        // raised, without a second pass.
                        return Err(error);
                    }
                    parser.error = Some(error);
                }
                let last_token = parser.fill - 1;
                parser.memos.clear();
                parser.mark = 0;
                parser.call_invalid_rules = true;
                statements::file_rule(&mut parser);
                parser.set_syntax_error(last_token);
                Err(parser.error.take().unwrap_or(ParseError {
                    kind: ExcKind::Syntax,
                    message: "invalid syntax".to_string(),
                    lineno: None,
                    offset: None,
                    end_lineno: None,
                    end_offset: None,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_module;

    /// The reported error: (message, lineno, offset, end_lineno,
    /// end_offset), with `None` standing in for CPython's `-1` "absent" end.
    type Expected = (String, Option<i32>, Option<i32>, Option<i32>, Option<i32>);

    fn error_of(source: &[u8]) -> Expected {
        match parse_module(source, None) {
            Ok(_) => panic!("expected a syntax error"),
            Err(e) => (e.message, e.lineno, e.offset, e.end_lineno, e.end_offset),
        }
    }

    /// Regressions reported by the review and audit of the error paths, with
    /// the positions CPython 3.12 reports for the same bytes.
    #[test]
    fn reported_errors_match_cpython() {
        let cases: &[(&str, Expected)] = &[
            (
                "plain except after except* keeps the except position",
                (
                    "cannot have both 'except' and 'except*' on the same 'try'"
                        .to_string(),
                    Some(5),
                    Some(1),
                    Some(5),
                    Some(7),
                ),
            ),
            (
                "parameter without default follows one with, at the parameter",
                (
                    "parameter without a default follows parameter with a default"
                        .to_string(),
                    Some(1),
                    Some(12),
                    Some(1),
                    Some(13),
                ),
            ),
            (
                "same for lambda parameters",
                (
                    "parameter without a default follows parameter with a default"
                        .to_string(),
                    Some(1),
                    Some(17),
                    Some(1),
                    Some(18),
                ),
            ),
            (
                "unclosed '(' names the bracket one past its column",
                (
                    "'(' was never closed".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(0),
                ),
            ),
            (
                "unclosed '['",
                (
                    "'[' was never closed".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(0),
                ),
            ),
            (
                "unclosed '{'",
                (
                    "'{' was never closed".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(0),
                ),
            ),
            (
                "unclosed '(' reports its own line, not the last",
                (
                    "'(' was never closed".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(0),
                ),
            ),
            (
                "unclosed '(' after a multibyte character counts characters",
                (
                    "'(' was never closed".to_string(),
                    Some(1),
                    Some(2),
                    Some(1),
                    Some(0),
                ),
            ),
            (
                "expected block at EOF keeps the line's end offset",
                (
                    "expected an indented block after 'if' statement on line 1"
                        .to_string(),
                    Some(1),
                    Some(6),
                    Some(1),
                    None,
                ),
            ),
            (
                "expected block after def at EOF",
                (
                    "expected an indented block after function definition on line 1"
                        .to_string(),
                    Some(1),
                    Some(28),
                    Some(1),
                    None,
                ),
            ),
            (
                "expected except or finally at EOF",
                (
                    "expected 'except' or 'finally' block".to_string(),
                    Some(1),
                    Some(10),
                    Some(1),
                    None,
                ),
            ),
            (
                "for target literal spans the target",
                (
                    "cannot assign to literal".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(6),
                ),
            ),
            (
                "for target call spans the target",
                (
                    "cannot assign to function call".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(8),
                ),
            ),
            (
                "generic error after '[' lands on the keyword",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(3),
                    Some(1),
                    Some(8),
                ),
            ),
            (
                "empty argument between commas",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(6),
                ),
            ),
            (
                "decode error at a BOM-like byte counts the line's characters",
                (
                    "(unicode error) 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
                        .to_string(),
                    Some(1),
                    Some(1),
                    Some(1),
                    None,
                ),
            ),
            (
                "decode error offset counts the whole line",
                (
                    "(unicode error) 'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
                        .to_string(),
                    Some(1),
                    Some(2),
                    Some(1),
                    None,
                ),
            ),
            (
                "truncated multibyte at line start",
                (
                    "(unicode error) 'utf-8' codec can't decode byte 0xc3 in position 0: unexpected end of data"
                        .to_string(),
                    Some(1),
                    Some(1),
                    Some(1),
                    None,
                ),
            ),
            (
                "truncated multibyte after a bracket",
                (
                    "(unicode error) 'utf-8' codec can't decode byte 0xc3 in position 0: unexpected end of data"
                        .to_string(),
                    Some(1),
                    Some(2),
                    Some(1),
                    None,
                ),
            ),
            (
                "truncated multibyte after a full character counts it as one",
                (
                    "(unicode error) 'utf-8' codec can't decode byte 0xc3 in position 2: unexpected end of data"
                        .to_string(),
                    Some(1),
                    Some(2),
                    Some(1),
                    None,
                ),
            ),
            (
                "utf-8 cookie leaves the raw bytes to the per-token checks",
                (
                    "unterminated string literal (detected at line 2)".to_string(),
                    Some(2),
                    Some(5),
                    Some(2),
                    None,
                ),
            ),
            (
                "utf8 alias recodes the buffer and fails at init",
                (
                    "'utf8' codec can't decode byte 0xff in position 28: invalid start byte"
                        .to_string(),
                    Some(0),
                    Some(-1),
                    None,
                    None,
                ),
            ),
            (
                "generic error on a synthetic EOF token is at column zero",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(0),
                    Some(1),
                    Some(0),
                ),
            ),
            (
                "keyword-safe kwarg lookahead does not fill at EOF",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(11),
                    Some(1),
                    Some(14),
                ),
            ),
            (
                "declined invalid rule ends the rule without filling EOF",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(29),
                    Some(1),
                    Some(30),
                ),
            ),
            (
                "no ==-hint for a kwarg broken later, plain syntax error",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(10),
                    Some(1),
                    Some(11),
                ),
            ),
            (
                "no ==-hint when the break is a trailing bang",
                (
                    "invalid syntax".to_string(),
                    Some(1),
                    Some(11),
                    Some(1),
                    Some(12),
                ),
            ),
            (
                "==-hint in a group spans the value",
                (
                    "invalid syntax. Maybe you meant '==' or ':=' instead of '='?"
                        .to_string(),
                    Some(1),
                    Some(2),
                    Some(1),
                    Some(7),
                ),
            ),
            (
                "==-hint in a list display",
                (
                    "invalid syntax. Maybe you meant '==' or ':=' instead of '='?"
                        .to_string(),
                    Some(1),
                    Some(2),
                    Some(1),
                    Some(7),
                ),
            ),
            (
                "==-hint inside call parens spans the value",
                (
                    "invalid syntax. Maybe you meant '==' or ':=' instead of '='?"
                        .to_string(),
                    Some(1),
                    Some(4),
                    Some(1),
                    Some(9),
                ),
            ),
            (
                "==-hint for a genexp kwarg spans the equals token",
                (
                    "invalid syntax. Maybe you meant '==' or ':=' instead of '='?"
                        .to_string(),
                    Some(1),
                    Some(5),
                    Some(1),
                    Some(8),
                ),
            ),
        ];
        let sources: &[&[u8]] = &[
            b"try:\n    pass\nexcept* A:\n    pass\nexcept B:\n    pass\n",
            b"def f(a=1, b):\n    pass\n",
            b"f = lambda a=1, b: 1\n",
            b"x = (\n",
            b"x = [\n",
            b"x = {\n",
            b"y = (1,\n  2,\n",
            b"\xc3\xa9(",
            b"if x:",
            b"def f(a, b=1, *args, **kw):",
            b"try: pass",
            b"for 1 in x:\n    pass\n",
            b"for f() in x:\n    pass\n",
            b"[ class",
            b"f(1,,2)\n",
            b"\xff\xfex",
            b"\xff\xfexy",
            b"\xc3(",
            b"(\xc3",
            b"\xc3\xa9\xc3",
            b"# -*- coding: utf-8 -*-\nx = \"\xcf\n",
            b"# -*- coding: utf8 -*-\nx = \"\xff\"\n",
            b"@_Specia",
            b"ns.update(def",
            b"list((x, y) for x in 'abcd' f",
            b"f(a=1, b=!c)\n",
            b"f(a=1, b=c!)\n",
            b"(a = 1)\n",
            b"[a = 1]\n",
            b"f((a = 1))\n",
            b"foo(a = 1 for x in y)\n",
        ];
        assert_eq!(cases.len(), sources.len());
        for ((what, expected), source) in cases.iter().zip(sources) {
            assert_eq!(&error_of(source), expected, "{what}");
        }
    }

    /// A deterministic LCG so failures reproduce from the seed alone.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 16
        }
    }

    /// Fragments that steer the random bytes toward the grammar's deeper
    /// paths: quotes and f-strings, brackets, keywords, patterns, operators.
    const FRAGMENTS: &[&str] = &[
        "def",
        "class",
        "lambda",
        "async",
        "await",
        "yield",
        "match",
        "case",
        "type",
        "_",
        "if",
        "else",
        "elif",
        "for",
        "while",
        "try",
        "except",
        "finally",
        "with",
        "as",
        "import",
        "from",
        "global",
        "nonlocal",
        "del",
        "pass",
        "break",
        "continue",
        "return",
        "raise",
        "assert",
        "in",
        "is",
        "not",
        "and",
        "or",
        "None",
        "True",
        "False",
        "print",
        "self",
        "0x1f",
        "1_000",
        "1e10",
        "1j",
        "0b101",
        "0o777",
        "007",
        "1.",
        "\"\"\"",
        "'''",
        "\"",
        "'",
        "f\"",
        "f'{",
        "rb'",
        "b\"",
        "u\"",
        "={",
        "!r",
        ":>10",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        ":",
        ";",
        ",",
        ".",
        "=",
        "==",
        ":=",
        "->",
        "**",
        "*",
        "+",
        "-",
        "/",
        "//",
        "%",
        "@",
        "|",
        "&",
        "^",
        "~",
        "<<",
        ">>",
        "...",
        "\\\n",
        "# type: int\n",
        "\n",
        "    ",
        "\t",
        "\\N{OX}",
        "\u{0}",
        "\u{ff}",
        "é",
        "中",
    ];

    /// The parser must reject or accept arbitrary bytes without panicking;
    /// `parse_module` re-panics if its thread does, so returning is enough.
    #[test]
    fn random_input_never_panics() {
        let mut rng = Rng(0x5eed_c0de_1234_5678);
        for _ in 0..400 {
            let mut source = Vec::new();
            let parts = 1 + (rng.next() % 80) as usize;
            for _ in 0..parts {
                let roll = rng.next() % 10;
                if roll < 7 {
                    source.extend_from_slice(
                        FRAGMENTS[(rng.next() % FRAGMENTS.len() as u64) as usize].as_bytes(),
                    );
                } else {
                    // Raw bytes, including invalid UTF-8 and controls.
                    source.push((rng.next() % 256) as u8);
                }
            }
            let _unused = parse_module(&source, None);
        }
    }
}

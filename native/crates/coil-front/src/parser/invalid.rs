//! The grammar's `invalid_*` rules: alternatives that exist only to raise
//! the specialized SyntaxError for input CPython rejects. They run in the
//! second pass, after the first pass failed generically.

use super::expressions;
use super::helpers::*;
use super::patterns;
use super::statements;
use super::strings;
use super::{ExcKind, Parser, Pos};
use crate::ast_generated::{Arg, Comprehension, Expr, Pattern};
use crate::tokenizer::TokenKind as T;

/// One invalid rule's alternatives in grammar order: the first whose
/// conditions match ends the rule — its action may raise or decline, as the
/// generated `goto done` skips the rest without restoring the mark — and a
/// failing one restores the mark as `p->mark = _mark` does.
fn run(p: &mut Parser, alts: &[fn(&mut Parser) -> Option<()>]) {
    for alt in alts {
        if p.has_error() {
            return;
        }
        if alt(p).is_some() {
            return;
        }
    }
}

/// The same loop where the grammar uses the invalid rule as a matching item:
/// a matched alternative raised, so `Some` lets the enclosing alternative
/// complete (and its own error check abort the rule).
fn run_matching(p: &mut Parser, alts: &[fn(&mut Parser) -> Option<()>]) -> Option<()> {
    for alt in alts {
        if p.has_error() {
            return None;
        }
        if alt(p).is_some() {
            return Some(());
        }
    }
    None
}

/// `_PyPegen_get_last_comprehension_item` over the clauses' last item.
fn last_comprehension_item(clauses: &[Comprehension]) -> Option<&Expr> {
    let last = clauses.last()?;
    Some(match last.ifs.last() {
        Some(e) => e,
        None => &last.iter,
    })
}

fn expr_pos(e: &Expr) -> Pos {
    Pos::from_expr(e)
}

pub(crate) fn arguments(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_arguments_unpacking,
            invalid_arguments_genexp,
            invalid_arguments_named_genexp,
            invalid_arguments_no_value,
            invalid_arguments_nonparen_genexp,
            invalid_arguments_trailing_genexp,
            invalid_arguments_positional,
        ],
    );
    p.exit_rule();
}

/// `(starred_expression | (assignment_expression | expression !':=') !'=')`.
fn invalid_arguments_item(p: &mut Parser) -> Option<Expr> {
    if let Some(a) = p.attempt(expressions::starred_expression) {
        return Some(a);
    }
    let e = p.alternatives(&[
        expressions::assignment_expression,
        invalid_arguments_plain_item,
    ])?;
    p.not_lookahead(T::Equal).then_some(e)
}

fn invalid_arguments_plain_item(p: &mut Parser) -> Option<Expr> {
    let e = expressions::expression(p)?;
    p.not_lookahead(T::Colonequal).then_some(e)
}

fn invalid_arguments_unpacking(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.alternatives(&[invalid_arguments_unpacking_with_items, expressions::kwargs])?;
        let a = p.mark;
        p.expect_opt(T::Comma)?;
        let gather = p.gather(invalid_arguments_starred_item, comma_sep)?;
        let _ = gather;
        p.raise_starting_from(
            p.token_pos(a),
            "iterable argument unpacking follows keyword argument unpacking".to_string(),
        );
        Some(())
    })
}

fn invalid_arguments_unpacking_with_items(p: &mut Parser) -> Option<Vec<KeywordOrStarred>> {
    p.gather(invalid_arguments_item, comma_sep)?;
    p.expect_opt(T::Comma)?;
    expressions::kwargs(p)
}

fn invalid_arguments_starred_item(p: &mut Parser) -> Option<Expr> {
    let a = expressions::starred_expression(p)?;
    p.not_lookahead(T::Equal).then_some(a)
}

fn comma_sep(p: &mut Parser) -> bool {
    p.expect(T::Comma)
}

fn invalid_arguments_genexp(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::expression(p)?;
        let b = expressions::for_if_clauses(p)?;
        p.expect_opt(T::Comma)?;
        p.attempt(|p| {
            p.alternatives(&[expressions::args, |p| {
                let e = expressions::expression(p)?;
                expressions::for_if_clauses(p)?;
                Some(e)
            }])
        });
        let end = expr_pos(last_comprehension_item(&b)?);
        p.raise_known_range(
            expr_pos(&a),
            end,
            "Generator expression must be parenthesized".to_string(),
        );
        Some(())
    })
}

fn invalid_arguments_named_genexp(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.name_token()?;
        let b = p.mark;
        p.expect_opt(T::Equal)?;
        expressions::expression(p)?;
        expressions::for_if_clauses(p)?;
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "invalid syntax. Maybe you meant '==' or ':=' instead of '='?".to_string(),
        );
        Some(())
    })
}

fn invalid_arguments_no_value(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.attempt(|p| {
            expressions::args(p)?;
            p.expect_opt(T::Comma)?;
            Some(())
        });
        let a = p.mark;
        p.name_token()?;
        let b = p.mark;
        p.expect_opt(T::Equal)?;
        if !(p.lookahead(T::Comma) || p.lookahead(T::Rpar)) {
            return None;
        }
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "expected argument value expression".to_string(),
        );
        Some(())
    })
}

fn invalid_arguments_nonparen_genexp(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::args(p)?;
        let b = expressions::for_if_clauses(p)?;
        p.nonparen_genexp_in_call(&a, &b);
        Some(())
    })
}

fn invalid_arguments_trailing_genexp(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        expressions::args(p)?;
        p.expect_opt(T::Comma)?;
        let a = expressions::expression(p)?;
        let b = expressions::for_if_clauses(p)?;
        let end = expr_pos(last_comprehension_item(&b)?);
        p.raise_known_range(
            expr_pos(&a),
            end,
            "Generator expression must be parenthesized".to_string(),
        );
        Some(())
    })
}

fn invalid_arguments_positional(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::args(p)?;
        p.expect_opt(T::Comma)?;
        expressions::args(p)?;
        p.arguments_parsing_error(&a);
        Some(())
    })
}

pub(crate) fn kwarg(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_kwarg_constant,
            invalid_kwarg_named_genexp,
            invalid_kwarg_expression,
            invalid_kwarg_unpacking,
        ],
    );
    p.exit_rule();
}

fn invalid_kwarg_constant(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        let matched =
            p.expect_keyword("True") || p.expect_keyword("False") || p.expect_keyword("None");
        if !matched {
            return None;
        }
        let b = p.mark;
        p.expect_opt(T::Equal)?;
        let text = String::from_utf8_lossy(&p.tok_at(a).bytes);
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            format!("cannot assign to {text}"),
        );
        Some(())
    })
}

fn invalid_kwarg_named_genexp(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.name_token()?;
        let b = p.mark;
        p.expect_opt(T::Equal)?;
        expressions::expression(p)?;
        expressions::for_if_clauses(p)?;
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "invalid syntax. Maybe you meant '==' or ':=' instead of '='?".to_string(),
        );
        Some(())
    })
}

fn invalid_kwarg_expression(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !invalid_kwarg_not_name_eq(p) {
            return None;
        }
        let a = expressions::expression(p)?;
        let b = p.mark;
        p.expect_opt(T::Equal)?;
        p.raise_known_range(
            expr_pos(&a),
            p.token_pos(b),
            "expression cannot contain assignment, perhaps you meant \"==\"?".to_string(),
        );
        Some(())
    })
}

fn invalid_kwarg_not_name_eq(p: &mut Parser) -> bool {
    let m = p.mark;
    // `NAME` never matches a keyword token, so the lookahead cannot run past
    // one to fill the token after it.
    let name_eq = p.name_token().is_some() && p.expect(T::Equal);
    p.mark = m;
    !name_eq
}

fn invalid_kwarg_unpacking(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_opt(T::Doublestar)?;
        expressions::expression(p)?;
        p.expect_opt(T::Equal)?;
        let b = expressions::expression(p)?;
        p.raise_known_range(
            p.token_pos(a),
            expr_pos(&b),
            "cannot assign to keyword argument unpacking".to_string(),
        );
        Some(())
    })
}

pub(crate) fn legacy_expression(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_legacy_expression_plain]);
    p.exit_rule();
}

fn invalid_legacy_expression_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.name_token()?;
        if !p.not_lookahead(T::Lpar) {
            return None;
        }
        let b = expressions::star_expressions(p)?;
        if check_legacy_stmt(p, &a) {
            let text = utf32_to_string(name_text_id(p, &a));
            p.raise_known_range(
                expr_pos(&a),
                expr_pos(&b),
                format!("Missing parentheses in call to '{text}'. Did you mean {text}(...)?"),
            );
        }
        Some(())
    })
}

fn name_text_id<'a>(p: &'a Parser, e: &'a Expr) -> &'a [u32] {
    match e {
        Expr::Name { id, .. } => p.interner.resolve(*id),
        _ => &[],
    }
}

pub(crate) fn expression(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_expression_comma,
            invalid_expression_missing_else,
            invalid_expression_lambda_fstring,
        ],
    );
    p.exit_rule();
}

/// `!(NAME STRING | SOFT_KEYWORD)`. A `NAME` item never matches a hard
/// keyword: `_get_keyword_or_name_type` types those away from `NAME`.
fn invalid_expression_not_prefixed(p: &mut Parser) -> bool {
    let m = p.mark;
    let name = p.peek() && {
        let t = p.tok();
        t.kind == T::Name
            && !super::is_hard_keyword(&t.bytes)
            && !matches!(t.bytes.as_ref(), b"async" | b"await")
    };
    let name_string = name && p.expect(T::Name) && p.expect(T::String);
    p.mark = m;
    if name_string {
        return false;
    }
    !(["match", "case", "type", "_"]
        .iter()
        .any(|kw| p.lookahead_soft_keyword(kw)))
}

fn invalid_expression_comma(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !invalid_expression_not_prefixed(p) {
            return None;
        }
        let a = expressions::disjunction(p)?;
        let b = expressions::expression_without_invalid(p)?;
        if check_legacy_stmt(p, &a) {
            return Some(());
        }
        // A parenthesized context cannot be a missing comma.
        if p.mark.checked_sub(1).is_none_or(|i| p.tok_at(i).level == 0) {
            return Some(());
        }
        p.raise_known_range(
            expr_pos(&a),
            expr_pos(&b),
            "invalid syntax. Perhaps you forgot a comma?".to_string(),
        );
        Some(())
    })
}

fn invalid_expression_missing_else(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::disjunction(p)?;
        p.expect_keyword_opt("if")?;
        let b = expressions::disjunction(p)?;
        if p.lookahead_keyword("else") || p.lookahead(T::Colon) {
            return None;
        }
        p.raise_known_range(
            expr_pos(&a),
            expr_pos(&b),
            "expected 'else' after 'if' expression".to_string(),
        );
        Some(())
    })
}

fn invalid_expression_lambda_fstring(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("lambda")?;
        p.attempt(expressions::lambda_params);
        let b = p.mark;
        p.expect_opt(T::Colon)?;
        if !p.lookahead(T::FStringMiddle) {
            return None;
        }
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "f-string: lambda expressions are not allowed without parentheses".to_string(),
        );
        Some(())
    })
}

pub(crate) fn named_expression(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    if p.memo_get(|t| &mut t.invalid_named_expression).is_some() {
        p.exit_rule();
        return;
    }
    let m = p.mark;
    run(
        p,
        &[
            invalid_named_expression_walrus,
            invalid_named_expression_assign,
            invalid_named_expression_bitwise,
        ],
    );
    p.memo_insert(|t| &mut t.invalid_named_expression, m, None);
    p.exit_rule();
}

fn invalid_named_expression_walrus(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::expression(p)?;
        p.expect_opt(T::Colonequal)?;
        expressions::expression(p)?;
        p.raise_known_location(
            expr_pos(&a),
            format!(
                "cannot use assignment expressions with {}",
                get_expr_name(&a)
            ),
        );
        Some(())
    })
}

fn invalid_named_expression_assign(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.name_token()?;
        p.expect_opt(T::Equal)?;
        let b = expressions::bitwise_or(p)?;
        if p.lookahead(T::Equal) || p.lookahead(T::Colonequal) {
            return None;
        }
        // `RAISE_SYNTAX_ERROR_KNOWN_RANGE(a, b, ...)`: `b` is the bitwise_or
        // node, so the span runs to the value's end, not past the `=`.
        p.raise_known_range(
            p.token_pos(a),
            expr_pos(&b),
            "invalid syntax. Maybe you meant '==' or ':=' instead of '='?".to_string(),
        );
        Some(())
    })
}

fn invalid_named_expression_bitwise(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if p.attempt(expressions::list).is_some()
            || p.attempt(expressions::tuple).is_some()
            || p.attempt(expressions::genexp).is_some()
            || p.expect_keyword("True")
            || p.expect_keyword("None")
            || p.expect_keyword("False")
        {
            return None;
        }
        let a = expressions::bitwise_or(p)?;
        p.expect_opt(T::Equal)?;
        expressions::bitwise_or(p)?;
        if p.lookahead(T::Equal) || p.lookahead(T::Colonequal) {
            return None;
        }
        p.raise_known_location(
            expr_pos(&a),
            format!(
                "cannot assign to {} here. Maybe you meant '==' instead of '='?",
                get_expr_name(&a)
            ),
        );
        Some(())
    })
}

pub(crate) fn assignment(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_assignment_ann_complex,
            invalid_assignment_ann_tuple,
            invalid_assignment_ann_plain,
            invalid_assignment_chain,
            invalid_assignment_yield,
            invalid_assignment_aug,
        ],
    );
    p.exit_rule();
}

fn invalid_assignment_ann_complex(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = invalid_ann_assign_target(p)?;
        p.expect_opt(T::Colon)?;
        expressions::expression(p)?;
        p.raise_known_location(
            expr_pos(&a),
            format!(
                "only single target (not {}) can be annotated",
                get_expr_name(&a)
            ),
        );
        Some(())
    })
}

fn invalid_assignment_ann_tuple(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::star_named_expression(p)?;
        p.expect_opt(T::Comma)?;
        let _ = p.repeat0(expressions::star_named_expressions);
        p.expect_opt(T::Colon)?;
        expressions::expression(p)?;
        p.raise_known_location(
            expr_pos(&a),
            "only single target (not tuple) can be annotated".to_string(),
        );
        Some(())
    })
}

fn invalid_assignment_ann_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::expression(p)?;
        p.expect_opt(T::Colon)?;
        expressions::expression(p)?;
        p.raise_known_location(expr_pos(&a), "illegal target for annotation".to_string());
        Some(())
    })
}

fn invalid_assignment_chain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        invalid_assignment_star_targets(p);
        let a = expressions::star_expressions(p)?;
        p.expect_opt(T::Equal)?;
        p.raise_invalid_target(&a, TargetsType::Star);
        Some(())
    })
}

fn invalid_assignment_star_targets(p: &mut Parser) {
    loop {
        let matched = p.attempt(|p| {
            expressions::star_targets(p)?;
            p.expect(T::Equal).then_some(())
        });
        if matched.is_none() {
            break;
        }
    }
}

fn invalid_assignment_yield(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        invalid_assignment_star_targets(p);
        let a = expressions::yield_expr(p)?;
        p.expect_opt(T::Equal)?;
        p.raise_known_location(
            expr_pos(&a),
            "assignment to yield expression not possible".to_string(),
        );
        Some(())
    })
}

fn invalid_assignment_aug(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::star_expressions(p)?;
        statements::augassign(p)?;
        p.alternatives(&[expressions::yield_expr, expressions::star_expressions])?;
        p.raise_known_location(
            expr_pos(&a),
            format!(
                "'{}' is an illegal expression for augmented assignment",
                get_expr_name(&a)
            ),
        );
        Some(())
    })
}

fn invalid_ann_assign_target(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        expressions::list,
        expressions::tuple,
        invalid_ann_assign_target_paren,
    ]);
    p.exit_rule();
    res
}

fn invalid_ann_assign_target_paren(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lpar)?;
    let a = invalid_ann_assign_target(p)?;
    p.expect_opt(T::Rpar)?;
    Some(a)
}

pub(crate) fn del_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_del_stmt_plain]);
    p.exit_rule();
}

fn invalid_del_stmt_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("del")?;
        let a = expressions::star_expressions(p)?;
        p.raise_invalid_target(&a, TargetsType::Del);
        Some(())
    })
}

pub(crate) fn block(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_block_plain]);
    p.exit_rule();
}

fn invalid_block_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        p.raise_indentation_error("expected an indented block".to_string());
        Some(())
    })
}

pub(crate) fn comprehension(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_comprehension_unpacking,
            invalid_comprehension_target,
            invalid_comprehension_target_comma,
        ],
    );
    p.exit_rule();
}

fn invalid_comprehension_unpacking(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !(p.expect(T::Lsqb) || p.expect(T::Lpar) || p.expect(T::Lbrace)) {
            return None;
        }
        let a = expressions::starred_expression(p)?;
        expressions::for_if_clauses(p)?;
        p.raise_known_location(
            expr_pos(&a),
            "iterable unpacking cannot be used in comprehension".to_string(),
        );
        Some(())
    })
}

fn invalid_comprehension_target(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !(p.expect(T::Lsqb) || p.expect(T::Lbrace)) {
            return None;
        }
        let a = expressions::star_named_expression(p)?;
        p.expect_opt(T::Comma)?;
        let b = expressions::star_named_expressions(p)?;
        expressions::for_if_clauses(p)?;
        let end = expr_pos(b.last()?);
        p.raise_known_range(
            expr_pos(&a),
            end,
            "did you forget parentheses around the comprehension target?".to_string(),
        );
        Some(())
    })
}

fn invalid_comprehension_target_comma(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !(p.expect(T::Lsqb) || p.expect(T::Lbrace)) {
            return None;
        }
        let a = expressions::star_named_expression(p)?;
        let b = p.mark;
        p.expect_opt(T::Comma)?;
        expressions::for_if_clauses(p)?;
        p.raise_known_range(
            expr_pos(&a),
            p.token_pos(b),
            "did you forget parentheses around the comprehension target?".to_string(),
        );
        Some(())
    })
}

pub(crate) fn dict_comprehension(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_dict_comprehension_plain]);
    p.exit_rule();
}

fn invalid_dict_comprehension_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        let a = p.mark;
        p.expect_opt(T::Doublestar)?;
        expressions::bitwise_or(p)?;
        expressions::for_if_clauses(p)?;
        p.expect_opt(T::Rbrace)?;
        p.raise_known_location(
            p.token_pos(a),
            "dict unpacking cannot be used in dict comprehension".to_string(),
        );
        Some(())
    })
}

pub(crate) fn parameters(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_parameters_bare_slash,
            invalid_parameters_double_slash,
            invalid_parameters_default_order,
            invalid_parameters_parenthesized,
            invalid_parameters_slash_after_star,
            invalid_parameters_missing_comma,
        ],
    );
    p.exit_rule();
}

fn invalid_parameters_bare_slash(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_opt(T::Slash)?;
        p.expect_opt(T::Comma)?;
        p.raise_known_location(
            p.token_pos(a),
            "at least one argument must precede /".to_string(),
        );
        Some(())
    })
}

fn invalid_parameters_double_slash(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        invalid_parameters_slash(p)?;
        let _ = p.repeat0(statements::param_maybe_default);
        let a = p.mark;
        p.expect_opt(T::Slash)?;
        p.raise_known_location(p.token_pos(a), "/ may appear only once".to_string());
        Some(())
    })
}

/// `(slash_no_default | slash_with_default)`.
fn invalid_parameters_slash(p: &mut Parser) -> Option<()> {
    if p.attempt(statements::slash_no_default).is_some() {
        return Some(());
    }
    statements::slash_with_default(p)?;
    Some(())
}

fn invalid_parameters_default_order(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.attempt(statements::slash_no_default);
        let _ = p.repeat0(statements::param_no_default);
        invalid_parameters_helper(p)?;
        let a = p.mark;
        statements::param_no_default(p)?;
        p.raise_known_location(
            p.token_pos(a),
            "parameter without a default follows parameter with a default".to_string(),
        );
        Some(())
    })
}

fn invalid_parameters_parenthesized(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let _ = p.repeat0(statements::param_no_default);
        let a = p.mark;
        p.expect_opt(T::Lpar)?;
        p.repeat1(statements::param_no_default)?;
        p.expect(T::Comma);
        let b = p.mark;
        p.expect_opt(T::Rpar)?;
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "Function parameters cannot be parenthesized".to_string(),
        );
        Some(())
    })
}

fn invalid_parameters_slash_after_star(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.attempt(invalid_parameters_slash);
        let _ = p.repeat0(statements::param_maybe_default);
        p.expect_opt(T::Star)?;
        if !(p.expect(T::Comma) || statements::param_no_default(p).is_some()) {
            return None;
        }
        let _ = p.repeat0(statements::param_maybe_default);
        let a = p.mark;
        p.expect_opt(T::Slash)?;
        p.raise_known_location(p.token_pos(a), "/ must be ahead of *".to_string());
        Some(())
    })
}

fn invalid_parameters_missing_comma(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.repeat1(statements::param_maybe_default)?;
        p.expect_opt(T::Slash)?;
        let a = p.mark;
        p.expect_opt(T::Star)?;
        p.raise_known_location(p.token_pos(a), "expected comma between / and *".to_string());
        Some(())
    })
}

fn invalid_parameters_helper(p: &mut Parser) -> Option<()> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        invalid_parameters_helper_slash,
        invalid_parameters_helper_defaults,
    ]);
    p.exit_rule();
    res
}

fn invalid_parameters_helper_slash(p: &mut Parser) -> Option<()> {
    statements::slash_with_default(p)?;
    Some(())
}

fn invalid_parameters_helper_defaults(p: &mut Parser) -> Option<()> {
    p.repeat1(statements::param_with_default)?;
    Some(())
}

pub(crate) fn default(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_default_plain]);
    p.exit_rule();
}

fn invalid_default_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_opt(T::Equal)?;
        if !(p.lookahead(T::Rpar) || p.lookahead(T::Comma)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "expected default value expression".to_string(),
        );
        Some(())
    })
}

pub(crate) fn star_etc(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_star_etc_bare,
            invalid_star_etc_type_comment,
            invalid_star_etc_default,
            invalid_star_etc_twice,
        ],
    );
    p.exit_rule();
}

fn invalid_star_etc_bare(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_opt(T::Star)?;
        p.alternatives(&[invalid_star_etc_bare_rpar, invalid_star_etc_bare_comma])?;
        p.raise_known_location(
            p.token_pos(a),
            "named arguments must follow bare *".to_string(),
        );
        Some(())
    })
}

fn invalid_star_etc_bare_rpar(p: &mut Parser) -> Option<()> {
    p.expect(T::Rpar).then_some(())
}

fn invalid_star_etc_bare_comma(p: &mut Parser) -> Option<()> {
    p.expect_opt(T::Comma)?;
    if !(p.expect(T::Rpar) || p.expect(T::Doublestar)) {
        return None;
    }
    Some(())
}

fn invalid_star_etc_type_comment(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Star)?;
        p.expect_opt(T::Comma)?;
        p.expect_opt(T::TypeComment)?;
        p.raise_syntax_error("bare * has associated type comment".to_string());
        Some(())
    })
}

fn invalid_star_etc_default(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Star)?;
        statements::param(p)?;
        let a = p.mark;
        p.expect_opt(T::Equal)?;
        p.raise_known_location(
            p.token_pos(a),
            "var-positional argument cannot have default value".to_string(),
        );
        Some(())
    })
}

fn invalid_star_etc_twice(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Star)?;
        if !(statements::param_no_default(p).is_some() || p.expect(T::Comma)) {
            return None;
        }
        let _ = p.repeat0(statements::param_maybe_default);
        let a = p.mark;
        p.expect_opt(T::Star)?;
        if !(statements::param_no_default(p).is_some() || p.expect(T::Comma)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "* argument may appear only once".to_string(),
        );
        Some(())
    })
}

pub(crate) fn kwds(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_kwds_default,
            invalid_kwds_followed_param,
            invalid_kwds_followed_token,
        ],
    );
    p.exit_rule();
}

fn invalid_kwds_default(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        statements::param(p)?;
        let a = p.mark;
        p.expect_opt(T::Equal)?;
        p.raise_known_location(
            p.token_pos(a),
            "var-keyword argument cannot have default value".to_string(),
        );
        Some(())
    })
}

fn invalid_kwds_followed_param(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        statements::param(p)?;
        p.expect_opt(T::Comma)?;
        let a = p.mark;
        statements::param(p)?;
        p.raise_known_location(
            p.token_pos(a),
            "arguments cannot follow var-keyword argument".to_string(),
        );
        Some(())
    })
}

fn invalid_kwds_followed_token(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        statements::param(p)?;
        p.expect_opt(T::Comma)?;
        let a = p.mark;
        if !(p.expect(T::Star) || p.expect(T::Doublestar) || p.expect(T::Slash)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "arguments cannot follow var-keyword argument".to_string(),
        );
        Some(())
    })
}

pub(crate) fn lambda_parameters(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_lambda_parameters_bare_slash,
            invalid_lambda_parameters_double_slash,
            invalid_lambda_parameters_default_order,
            invalid_lambda_parameters_parenthesized,
            invalid_lambda_parameters_slash_after_star,
            invalid_lambda_parameters_missing_comma,
        ],
    );
    p.exit_rule();
}

fn invalid_lambda_parameters_bare_slash(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_opt(T::Slash)?;
        p.expect_opt(T::Comma)?;
        p.raise_known_location(
            p.token_pos(a),
            "at least one argument must precede /".to_string(),
        );
        Some(())
    })
}

fn invalid_lambda_parameters_double_slash(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        invalid_lambda_parameters_slash(p)?;
        let _ = p.repeat0(expressions::lambda_param_maybe_default);
        let a = p.mark;
        p.expect_opt(T::Slash)?;
        p.raise_known_location(p.token_pos(a), "/ may appear only once".to_string());
        Some(())
    })
}

/// `(lambda_slash_no_default | lambda_slash_with_default)`.
fn invalid_lambda_parameters_slash(p: &mut Parser) -> Option<()> {
    if p.attempt(expressions::lambda_slash_no_default).is_some() {
        return Some(());
    }
    expressions::lambda_slash_with_default(p)?;
    Some(())
}

fn invalid_lambda_parameters_default_order(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.attempt(expressions::lambda_slash_no_default);
        let _ = p.repeat0(expressions::lambda_param_no_default);
        invalid_lambda_parameters_helper(p)?;
        let a = p.mark;
        expressions::lambda_param_no_default(p)?;
        p.raise_known_location(
            p.token_pos(a),
            "parameter without a default follows parameter with a default".to_string(),
        );
        Some(())
    })
}

fn invalid_lambda_parameters_parenthesized(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let _ = p.repeat0(expressions::lambda_param_no_default);
        let a = p.mark;
        p.expect_opt(T::Lpar)?;
        p.gather(lambda_param_alt, comma_sep)?;
        p.expect(T::Comma);
        let b = p.mark;
        p.expect_opt(T::Rpar)?;
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "Lambda expression parameters cannot be parenthesized".to_string(),
        );
        Some(())
    })
}

fn lambda_param_alt(p: &mut Parser) -> Option<Arg> {
    expressions::lambda_param(p)
}

fn invalid_lambda_parameters_slash_after_star(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.attempt(invalid_lambda_parameters_slash);
        let _ = p.repeat0(expressions::lambda_param_maybe_default);
        p.expect_opt(T::Star)?;
        if !(p.expect(T::Comma) || expressions::lambda_param_no_default(p).is_some()) {
            return None;
        }
        let _ = p.repeat0(expressions::lambda_param_maybe_default);
        let a = p.mark;
        p.expect_opt(T::Slash)?;
        p.raise_known_location(p.token_pos(a), "/ must be ahead of *".to_string());
        Some(())
    })
}

fn invalid_lambda_parameters_missing_comma(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.repeat1(expressions::lambda_param_maybe_default)?;
        p.expect_opt(T::Slash)?;
        let a = p.mark;
        p.expect_opt(T::Star)?;
        p.raise_known_location(p.token_pos(a), "expected comma between / and *".to_string());
        Some(())
    })
}

fn invalid_lambda_parameters_helper(p: &mut Parser) -> Option<()> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        invalid_lambda_parameters_helper_slash,
        invalid_lambda_parameters_helper_defaults,
    ]);
    p.exit_rule();
    res
}

fn invalid_lambda_parameters_helper_slash(p: &mut Parser) -> Option<()> {
    expressions::lambda_slash_with_default(p)?;
    Some(())
}

fn invalid_lambda_parameters_helper_defaults(p: &mut Parser) -> Option<()> {
    p.repeat1(expressions::lambda_param_with_default)?;
    Some(())
}

pub(crate) fn lambda_star_etc(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_lambda_star_etc_bare,
            invalid_lambda_star_etc_default,
            invalid_lambda_star_etc_twice,
        ],
    );
    p.exit_rule();
}

fn invalid_lambda_star_etc_bare(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Star)?;
        p.alternatives(&[
            invalid_lambda_star_etc_bare_colon,
            invalid_lambda_star_etc_bare_comma,
        ])?;
        p.raise_syntax_error("named arguments must follow bare *".to_string());
        Some(())
    })
}

fn invalid_lambda_star_etc_bare_colon(p: &mut Parser) -> Option<()> {
    p.expect(T::Colon).then_some(())
}

fn invalid_lambda_star_etc_bare_comma(p: &mut Parser) -> Option<()> {
    p.expect_opt(T::Comma)?;
    if !(p.expect(T::Colon) || p.expect(T::Doublestar)) {
        return None;
    }
    Some(())
}

fn invalid_lambda_star_etc_default(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Star)?;
        expressions::lambda_param(p)?;
        let a = p.mark;
        p.expect_opt(T::Equal)?;
        p.raise_known_location(
            p.token_pos(a),
            "var-positional argument cannot have default value".to_string(),
        );
        Some(())
    })
}

fn invalid_lambda_star_etc_twice(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Star)?;
        if !(expressions::lambda_param_no_default(p).is_some() || p.expect(T::Comma)) {
            return None;
        }
        let _ = p.repeat0(expressions::lambda_param_maybe_default);
        let a = p.mark;
        p.expect_opt(T::Star)?;
        if !(expressions::lambda_param_no_default(p).is_some() || p.expect(T::Comma)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "* argument may appear only once".to_string(),
        );
        Some(())
    })
}

pub(crate) fn lambda_kwds(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_lambda_kwds_default,
            invalid_lambda_kwds_followed_param,
            invalid_lambda_kwds_followed_token,
        ],
    );
    p.exit_rule();
}

fn invalid_lambda_kwds_default(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        expressions::lambda_param(p)?;
        let a = p.mark;
        p.expect_opt(T::Equal)?;
        p.raise_known_location(
            p.token_pos(a),
            "var-keyword argument cannot have default value".to_string(),
        );
        Some(())
    })
}

fn invalid_lambda_kwds_followed_param(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        expressions::lambda_param(p)?;
        p.expect_opt(T::Comma)?;
        let a = p.mark;
        expressions::lambda_param(p)?;
        p.raise_known_location(
            p.token_pos(a),
            "arguments cannot follow var-keyword argument".to_string(),
        );
        Some(())
    })
}

fn invalid_lambda_kwds_followed_token(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        expressions::lambda_param(p)?;
        p.expect_opt(T::Comma)?;
        let a = p.mark;
        if !(p.expect(T::Star) || p.expect(T::Doublestar) || p.expect(T::Slash)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "arguments cannot follow var-keyword argument".to_string(),
        );
        Some(())
    })
}

pub(crate) fn double_type_comments(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_double_type_comments_plain]);
    p.exit_rule();
}

fn invalid_double_type_comments_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::TypeComment)?;
        p.expect_opt(T::Newline)?;
        p.expect_opt(T::TypeComment)?;
        p.expect_opt(T::Newline)?;
        p.expect_opt(T::Indent)?;
        p.raise_syntax_error("Cannot have two type comments on def".to_string());
        Some(())
    })
}

pub(crate) fn with_item(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_with_item_plain]);
    p.exit_rule();
}

fn invalid_with_item_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        expressions::expression(p)?;
        p.expect_keyword_opt("as")?;
        let a = expressions::expression(p)?;
        if !(p.lookahead(T::Comma) || p.lookahead(T::Rpar) || p.lookahead(T::Colon)) {
            return None;
        }
        p.raise_invalid_target(&a, TargetsType::Star);
        Some(())
    })
}

pub(crate) fn for_target(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_for_target_plain]);
    p.exit_rule();
}

fn invalid_for_target_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        p.expect_keyword_opt("for")?;
        let a = expressions::star_expressions(p)?;
        p.raise_invalid_target(&a, TargetsType::For);
        Some(())
    })
}

pub(crate) fn group(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_group_starred, invalid_group_double_starred]);
    p.exit_rule();
}

fn invalid_group_starred(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lpar)?;
        let a = expressions::starred_expression(p)?;
        p.expect_opt(T::Rpar)?;
        p.raise_known_location(
            expr_pos(&a),
            "cannot use starred expression here".to_string(),
        );
        Some(())
    })
}

fn invalid_group_double_starred(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lpar)?;
        let a = p.mark;
        p.expect_opt(T::Doublestar)?;
        expressions::expression(p)?;
        p.expect_opt(T::Rpar)?;
        p.raise_known_location(
            p.token_pos(a),
            "cannot use double starred expression here".to_string(),
        );
        Some(())
    })
}

pub(crate) fn import(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_import_plain]);
    p.exit_rule();
}

fn invalid_import_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("import")?;
        p.gather(statements::dotted_name, comma_sep)?;
        p.expect_keyword_opt("from")?;
        statements::dotted_name(p)?;
        p.raise_starting_from(
            p.token_pos(a),
            "Did you mean to use 'from ... import ...' instead?".to_string(),
        );
        Some(())
    })
}

pub(crate) fn import_from_targets(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_import_from_targets_plain]);
    p.exit_rule();
}

fn invalid_import_from_targets_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        statements::import_from_as_names(p)?;
        p.expect_opt(T::Comma)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error(
            "trailing comma not allowed without surrounding parentheses".to_string(),
        );
        Some(())
    })
}

pub(crate) fn with_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_with_stmt_plain, invalid_with_stmt_paren]);
    p.exit_rule();
}

/// `[ASYNC] 'with' ','.(expression ['as' star_target])+ NEWLINE`.
fn invalid_with_stmt_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        p.expect_keyword_opt("with")?;
        p.gather(invalid_with_stmt_item, comma_sep)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_with_stmt_item(p: &mut Parser) -> Option<Expr> {
    let a = expressions::expression(p)?;
    p.attempt(|p| {
        p.expect_keyword_opt("as")?;
        statements::star_target(p)
    });
    Some(a)
}

fn invalid_with_stmt_paren(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        p.expect_keyword_opt("with")?;
        p.expect_opt(T::Lpar)?;
        p.gather(invalid_with_stmt_paren_item, comma_sep)?;
        p.expect(T::Comma);
        p.expect_opt(T::Rpar)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_with_stmt_paren_item(p: &mut Parser) -> Option<Expr> {
    let a = expressions::expressions(p)?;
    p.attempt(|p| {
        p.expect_keyword_opt("as")?;
        statements::star_target(p)
    });
    Some(a)
}

pub(crate) fn with_stmt_indent(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_with_stmt_indent_plain,
            invalid_with_stmt_indent_paren,
        ],
    );
    p.exit_rule();
}

fn invalid_with_stmt_indent_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        let a = p.mark;
        p.expect_keyword_opt("with")?;
        p.gather(invalid_with_stmt_item, comma_sep)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'with' statement on line {lineno}"
        ));
        Some(())
    })
}

fn invalid_with_stmt_indent_paren(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        let a = p.mark;
        p.expect_keyword_opt("with")?;
        p.expect_opt(T::Lpar)?;
        p.gather(invalid_with_stmt_paren_item, comma_sep)?;
        p.expect(T::Comma);
        p.expect_opt(T::Rpar)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'with' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn try_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_try_stmt_no_body,
            invalid_try_stmt_no_handler,
            invalid_try_stmt_mixed_star,
            invalid_try_stmt_mixed_plain,
        ],
    );
    p.exit_rule();
}

fn invalid_try_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("try")?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'try' statement on line {lineno}"
        ));
        Some(())
    })
}

fn invalid_try_stmt_no_handler(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("try")?;
        p.expect_opt(T::Colon)?;
        statements::block(p)?;
        if p.lookahead_keyword("except") || p.lookahead_keyword("finally") {
            return None;
        }
        p.raise_syntax_error("expected 'except' or 'finally' block".to_string());
        Some(())
    })
}

fn invalid_try_stmt_mixed_star(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("try")?;
        p.expect_opt(T::Colon)?;
        let _ = p.repeat0(statements::block);
        p.repeat1(statements::except_block)?;
        let a = p.mark;
        p.expect_keyword_opt("except")?;
        let b = p.mark;
        p.expect_opt(T::Star)?;
        expressions::expression(p)?;
        p.attempt(|p| {
            p.expect_keyword_opt("as")?;
            p.name_token()
        });
        p.expect_opt(T::Colon)?;
        p.raise_known_range(
            p.token_pos(a),
            p.token_pos(b),
            "cannot have both 'except' and 'except*' on the same 'try'".to_string(),
        );
        Some(())
    })
}

fn invalid_try_stmt_mixed_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("try")?;
        p.expect_opt(T::Colon)?;
        let _ = p.repeat0(statements::block);
        p.repeat1(statements::except_star_block)?;
        let a = p.mark;
        p.expect_keyword_opt("except")?;
        // `[expression ['as' NAME]]`: only the whole group is optional — a
        // matching `as` clause failing after the expression must not undo it.
        p.attempt(|p| {
            expressions::expression(p)?;
            p.attempt(|p| {
                p.expect_keyword_opt("as")?;
                p.name_token()
            });
            Some(())
        });
        p.expect_opt(T::Colon)?;
        p.raise_known_location(
            p.token_pos(a),
            "cannot have both 'except' and 'except*' on the same 'try'".to_string(),
        );
        Some(())
    })
}

pub(crate) fn except_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_except_stmt_multiple,
            invalid_except_stmt_no_colon,
            invalid_except_stmt_bare_no_colon,
            invalid_except_stmt_star_no_type,
        ],
    );
    p.exit_rule();
}

fn invalid_except_stmt_multiple(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("except")?;
        p.expect(T::Star);
        let a = expressions::expression(p)?;
        p.expect_opt(T::Comma)?;
        expressions::expressions(p)?;
        p.attempt(|p| {
            p.expect_keyword_opt("as")?;
            p.name_token()
        });
        p.expect_opt(T::Colon)?;
        p.raise_starting_from(
            expr_pos(&a),
            "multiple exception types must be parenthesized".to_string(),
        );
        Some(())
    })
}

fn invalid_except_stmt_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("except")?;
        p.expect(T::Star);
        expressions::expression(p)?;
        p.attempt(|p| {
            p.expect_keyword_opt("as")?;
            p.name_token()
        });
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_except_stmt_bare_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("except")?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_except_stmt_star_no_type(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("except")?;
        p.expect_opt(T::Star)?;
        if !(p.expect(T::Newline) || p.expect(T::Colon)) {
            return None;
        }
        p.raise_syntax_error("expected one or more exception types".to_string());
        Some(())
    })
}

pub(crate) fn finally_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_finally_stmt_plain]);
    p.exit_rule();
}

fn invalid_finally_stmt_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("finally")?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'finally' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn except_stmt_indent(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_except_stmt_indent_expr,
            invalid_except_stmt_indent_plain,
        ],
    );
    p.exit_rule();
}

fn invalid_except_stmt_indent_expr(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("except")?;
        expressions::expression(p)?;
        p.attempt(|p| {
            p.expect_keyword_opt("as")?;
            p.name_token()
        });
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'except' statement on line {lineno}"
        ));
        Some(())
    })
}

fn invalid_except_stmt_indent_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("except")?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'except' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn except_star_stmt_indent(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_except_star_stmt_indent_plain]);
    p.exit_rule();
}

fn invalid_except_star_stmt_indent_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("except")?;
        p.expect_opt(T::Star)?;
        expressions::expression(p)?;
        p.attempt(|p| {
            p.expect_keyword_opt("as")?;
            p.name_token()
        });
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'except*' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn match_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[invalid_match_stmt_no_colon, invalid_match_stmt_no_body],
    );
    p.exit_rule();
}

fn invalid_match_stmt_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !p.expect_soft_keyword("match") {
            return None;
        }
        statements::subject_expr(p)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_match_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        if !p.expect_soft_keyword("match") {
            return None;
        }
        statements::subject_expr(p)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'match' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn case_block(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[invalid_case_block_no_colon, invalid_case_block_no_body],
    );
    p.exit_rule();
}

fn invalid_case_block_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        if !p.expect_soft_keyword("case") {
            return None;
        }
        patterns::patterns(p)?;
        p.attempt(statements::guard_rule);
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_case_block_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        if !p.expect_soft_keyword("case") {
            return None;
        }
        patterns::patterns(p)?;
        p.attempt(statements::guard_rule);
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'case' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn as_pattern(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_as_pattern_wildcard, invalid_as_pattern_target]);
    p.exit_rule();
}

fn invalid_as_pattern_wildcard(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        patterns::or_pattern(p)?;
        p.expect_keyword_opt("as")?;
        let a = p.mark;
        if !p.expect_soft_keyword("_") {
            return None;
        }
        p.raise_known_location(p.token_pos(a), "cannot use '_' as a target".to_string());
        Some(())
    })
}

fn invalid_as_pattern_target(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        patterns::or_pattern(p)?;
        p.expect_keyword_opt("as")?;
        if p.lookahead(T::Name) {
            return None;
        }
        let a = expressions::expression(p)?;
        p.raise_known_location(expr_pos(&a), "invalid pattern target".to_string());
        Some(())
    })
}

pub(crate) fn class_pattern(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_class_pattern_plain]);
    p.exit_rule();
}

fn invalid_class_pattern_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        patterns::name_or_attr(p)?;
        p.expect_opt(T::Lpar)?;
        let a = invalid_class_argument_pattern(p)?;
        let first = pattern_pos(a.first()?);
        let last = pattern_pos(a.last()?);
        p.raise_known_range(
            first,
            last,
            "positional patterns follow keyword patterns".to_string(),
        );
        Some(())
    })
}

/// `[positional_patterns ','] keyword_patterns ',' a=positional_patterns`.
fn invalid_class_argument_pattern(p: &mut Parser) -> Option<Vec<Pattern>> {
    p.attempt(|p| {
        p.attempt(|p| {
            patterns::positional_patterns(p)?;
            p.expect_opt(T::Comma)?;
            Some(())
        });
        patterns::keyword_patterns(p)?;
        p.expect_opt(T::Comma)?;
        patterns::positional_patterns(p)
    })
}

/// A pattern node's span, the fields `RAISE_SYNTAX_ERROR_KNOWN_RANGE` reads.
fn pattern_pos(pattern: &Pattern) -> Pos {
    let fields = |(lineno, col, end_lineno, end_col): (i32, i32, i32, i32)| Pos {
        lineno,
        col_offset: col,
        end_lineno: Some(end_lineno),
        end_col_offset: Some(end_col),
    };
    match pattern {
        Pattern::MatchValue {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchSingleton {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchSequence {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchMapping {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchClass {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchStar {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchAs {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        }
        | Pattern::MatchOr {
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => fields((*lineno, *col_offset, *end_lineno, *end_col_offset)),
    }
}

pub(crate) fn if_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_if_stmt_no_colon, invalid_if_stmt_no_body]);
    p.exit_rule();
}

fn invalid_if_stmt_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("if")?;
        expressions::named_expression(p)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_if_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("if")?;
        expressions::named_expression(p)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'if' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn elif_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_elif_stmt_no_colon, invalid_elif_stmt_no_body]);
    p.exit_rule();
}

fn invalid_elif_stmt_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("elif")?;
        expressions::named_expression(p)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_elif_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("elif")?;
        expressions::named_expression(p)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'elif' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn else_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_else_stmt_no_body]);
    p.exit_rule();
}

fn invalid_else_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("else")?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'else' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn while_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[invalid_while_stmt_no_colon, invalid_while_stmt_no_body],
    );
    p.exit_rule();
}

fn invalid_while_stmt_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("while")?;
        expressions::named_expression(p)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_while_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("while")?;
        expressions::named_expression(p)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'while' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn for_stmt(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_for_stmt_no_colon, invalid_for_stmt_no_body]);
    p.exit_rule();
}

fn invalid_for_stmt_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        p.expect_keyword_opt("for")?;
        statements::star_targets(p)?;
        p.expect_keyword_opt("in")?;
        expressions::star_expressions(p)?;
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_for_stmt_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        let a = p.mark;
        p.expect_keyword_opt("for")?;
        statements::star_targets(p)?;
        p.expect_keyword_opt("in")?;
        expressions::star_expressions(p)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after 'for' statement on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn def_raw(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_def_raw_no_body]);
    p.exit_rule();
}

fn invalid_def_raw_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect(T::Async);
        let a = p.mark;
        p.expect_keyword_opt("def")?;
        p.name_token()?;
        p.attempt(statements::type_params);
        p.expect_opt(T::Lpar)?;
        p.attempt(statements::params);
        p.expect_opt(T::Rpar)?;
        p.attempt(|p| {
            p.expect_opt(T::Rarrow)?;
            expressions::expression(p)
        });
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after function definition on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn class_def_raw(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_class_def_raw_no_colon,
            invalid_class_def_raw_no_body,
        ],
    );
    p.exit_rule();
}

fn invalid_class_def_raw_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_keyword_opt("class")?;
        p.name_token()?;
        p.attempt(statements::type_params);
        p.attempt(|p| {
            p.expect_opt(T::Lpar)?;
            p.attempt(expressions::arguments);
            p.expect_opt(T::Rpar)?;
            Some(())
        });
        p.expect_opt(T::Newline)?;
        p.raise_syntax_error("expected ':'".to_string());
        Some(())
    })
}

fn invalid_class_def_raw_no_body(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_keyword_opt("class")?;
        p.name_token()?;
        p.attempt(statements::type_params);
        p.attempt(|p| {
            p.expect_opt(T::Lpar)?;
            p.attempt(expressions::arguments);
            p.expect_opt(T::Rpar)?;
            Some(())
        });
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        if p.lookahead(T::Indent) {
            return None;
        }
        let lineno = p.token_pos(a).lineno;
        p.raise_indentation_error(format!(
            "expected an indented block after class definition on line {lineno}"
        ));
        Some(())
    })
}

pub(crate) fn double_starred_kvpairs(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_double_starred_kvpairs_trailing,
            invalid_double_starred_kvpairs_starred,
            invalid_double_starred_kvpairs_no_value,
        ],
    );
    p.exit_rule();
}

fn invalid_double_starred_kvpairs_trailing(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.gather(expressions::double_starred_kvpair, comma_sep)?;
        p.expect_opt(T::Comma)?;
        kvpair(p)?;
        Some(())
    })
}

fn invalid_double_starred_kvpairs_starred(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        expressions::expression(p)?;
        p.expect_opt(T::Colon)?;
        let a = p.mark;
        p.expect_opt(T::Star)?;
        expressions::bitwise_or(p)?;
        p.raise_starting_from(
            p.token_pos(a),
            "cannot use a starred expression in a dictionary value".to_string(),
        );
        Some(())
    })
}

fn invalid_double_starred_kvpairs_no_value(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        expressions::expression(p)?;
        let a = p.mark;
        p.expect_opt(T::Colon)?;
        if !(p.lookahead(T::Rbrace) || p.lookahead(T::Comma)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "expression expected after dictionary key and ':'".to_string(),
        );
        Some(())
    })
}

pub(crate) fn kvpair(p: &mut Parser) -> Option<()> {
    if !p.enter_rule() {
        return None;
    }
    let res = run_matching(
        p,
        &[
            invalid_kvpair_no_colon,
            invalid_kvpair_starred,
            invalid_kvpair_no_value,
        ],
    );
    p.exit_rule();
    res
}

fn invalid_kvpair_no_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = expressions::expression(p)?;
        if p.lookahead(T::Colon) {
            return None;
        }
        let pos = expr_pos(&a);
        p.macro_raise(
            ExcKind::Syntax,
            pos.lineno as i64,
            (pos.end_col_offset.unwrap_or(pos.col_offset) - 1) as i64,
            pos.end_lineno.unwrap_or(pos.lineno) as i64,
            -1,
            "':' expected after dictionary key".to_string(),
        );
        Some(())
    })
}

fn invalid_kvpair_starred(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        expressions::expression(p)?;
        p.expect_opt(T::Colon)?;
        let a = p.mark;
        p.expect_opt(T::Star)?;
        expressions::bitwise_or(p)?;
        p.raise_starting_from(
            p.token_pos(a),
            "cannot use a starred expression in a dictionary value".to_string(),
        );
        Some(())
    })
}

fn invalid_kvpair_no_value(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        expressions::expression(p)?;
        let a = p.mark;
        p.expect_opt(T::Colon)?;
        if !(p.lookahead(T::Rbrace) || p.lookahead(T::Comma)) {
            return None;
        }
        p.raise_known_location(
            p.token_pos(a),
            "expression expected after dictionary key and ':'".to_string(),
        );
        Some(())
    })
}

pub(crate) fn starred_expression(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(p, &[invalid_starred_expression_plain]);
    p.exit_rule();
}

fn invalid_starred_expression_plain(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        let a = p.mark;
        p.expect_opt(T::Star)?;
        expressions::expression(p)?;
        p.expect_opt(T::Equal)?;
        let b = expressions::expression(p)?;
        p.raise_known_range(
            p.token_pos(a),
            expr_pos(&b),
            "cannot assign to iterable argument unpacking".to_string(),
        );
        Some(())
    })
}

pub(crate) fn replacement_field(p: &mut Parser) {
    if !p.enter_rule() {
        return;
    }
    run(
        p,
        &[
            invalid_replacement_field_debug,
            invalid_replacement_field_exclaim,
            invalid_replacement_field_colon,
            invalid_replacement_field_brace,
            invalid_replacement_field_no_expr,
            invalid_replacement_field_no_delimiter,
            invalid_replacement_field_no_spec_delimiter,
            invalid_replacement_field_conversion,
            invalid_replacement_field_no_end,
            invalid_replacement_field_no_close,
            invalid_replacement_field_bare_end,
        ],
    );
    p.exit_rule();
}

fn replacement_field_expr(p: &mut Parser) -> Option<Expr> {
    p.alternatives(&[expressions::yield_expr, expressions::star_expressions])
}

fn invalid_replacement_field_debug(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        let a = p.mark;
        p.expect_opt(T::Equal)?;
        p.raise_known_location(
            p.token_pos(a),
            "f-string: valid expression required before '='".to_string(),
        );
        Some(())
    })
}

fn invalid_replacement_field_exclaim(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        let a = p.mark;
        p.expect_opt(T::Exclamation)?;
        p.raise_known_location(
            p.token_pos(a),
            "f-string: valid expression required before '!'".to_string(),
        );
        Some(())
    })
}

fn invalid_replacement_field_colon(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        let a = p.mark;
        p.expect_opt(T::Colon)?;
        p.raise_known_location(
            p.token_pos(a),
            "f-string: valid expression required before ':'".to_string(),
        );
        Some(())
    })
}

fn invalid_replacement_field_brace(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        let a = p.mark;
        p.expect_opt(T::Rbrace)?;
        p.raise_known_location(
            p.token_pos(a),
            "f-string: valid expression required before '}'".to_string(),
        );
        Some(())
    })
}

fn invalid_replacement_field_no_expr(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        if p.attempt(replacement_field_expr).is_some() {
            return None;
        }
        p.raise_syntax_error_on_next_token(
            "f-string: expecting a valid expression after '{'".to_string(),
        );
        Some(())
    })
}

fn invalid_replacement_field_no_delimiter(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        replacement_field_expr(p)?;
        if p.lookahead(T::Equal)
            || p.lookahead(T::Exclamation)
            || p.lookahead(T::Colon)
            || p.lookahead(T::Rbrace)
        {
            return None;
        }
        if !p.has_error() {
            p.raise_syntax_error_on_next_token(
                "f-string: expecting '=', or '!', or ':', or '}'".to_string(),
            );
        }
        Some(())
    })
}

fn invalid_replacement_field_no_spec_delimiter(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        replacement_field_expr(p)?;
        p.expect_opt(T::Equal)?;
        if p.lookahead(T::Exclamation) || p.lookahead(T::Colon) || p.lookahead(T::Rbrace) {
            return None;
        }
        if !p.has_error() {
            p.raise_syntax_error_on_next_token(
                "f-string: expecting '!', or ':', or '}'".to_string(),
            );
        }
        Some(())
    })
}

fn invalid_replacement_field_conversion(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        replacement_field_expr(p)?;
        p.expect(T::Equal);
        conversion_character(p)?;
        Some(())
    })
}

fn invalid_replacement_field_no_end(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        replacement_field_expr(p)?;
        p.expect(T::Equal);
        p.attempt(|p| {
            p.expect_opt(T::Exclamation)?;
            p.name_token()
        });
        if p.lookahead(T::Colon) || p.lookahead(T::Rbrace) {
            return None;
        }
        if !p.has_error() {
            p.raise_syntax_error_on_next_token("f-string: expecting ':' or '}'".to_string());
        }
        Some(())
    })
}

fn invalid_replacement_field_no_close(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        replacement_field_expr(p)?;
        p.expect(T::Equal);
        p.attempt(|p| {
            p.expect_opt(T::Exclamation)?;
            p.name_token()
        });
        p.expect_opt(T::Colon)?;
        let _ = p.repeat0(strings::fstring_format_spec);
        if p.lookahead(T::Rbrace) {
            return None;
        }
        if !p.has_error() {
            p.raise_syntax_error_on_next_token(
                "f-string: expecting '}', or format specs".to_string(),
            );
        }
        Some(())
    })
}

fn invalid_replacement_field_bare_end(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        replacement_field_expr(p)?;
        p.expect(T::Equal);
        if p.lookahead(T::Rbrace) {
            return None;
        }
        if !p.has_error() {
            p.raise_syntax_error_on_next_token("f-string: expecting '}'".to_string());
        }
        Some(())
    })
}

pub(crate) fn conversion_character(p: &mut Parser) -> Option<()> {
    if !p.enter_rule() {
        return None;
    }
    let res = run_matching(
        p,
        &[
            invalid_conversion_character_missing,
            invalid_conversion_character_invalid,
        ],
    );
    p.exit_rule();
    res
}

fn invalid_conversion_character_missing(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Exclamation)?;
        if !(p.lookahead(T::Colon) || p.lookahead(T::Rbrace)) {
            return None;
        }
        p.raise_syntax_error_on_next_token("f-string: missing conversion character".to_string());
        Some(())
    })
}

fn invalid_conversion_character_invalid(p: &mut Parser) -> Option<()> {
    p.attempt(|p| {
        p.expect_opt(T::Exclamation)?;
        if p.lookahead(T::Name) {
            return None;
        }
        p.raise_syntax_error_on_next_token("f-string: invalid conversion character".to_string());
        Some(())
    })
}

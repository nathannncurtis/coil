//! The grammar's pattern rules for `match` statements.

use super::Parser;
use super::helpers::*;
use super::invalid;
use super::strings;
use crate::ast::Constant;
use crate::ast_generated::{Expr, ExprContext, Operator, Pattern, Unaryop};
use crate::tokenizer::TokenKind as T;

fn ends(pos: &super::Pos) -> (i32, i32) {
    (
        pos.end_lineno.unwrap_or(pos.lineno),
        pos.end_col_offset.unwrap_or(pos.col_offset),
    )
}

pub(crate) fn patterns(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[patterns_sequence, patterns_single]);
    p.exit_rule();
    res
}

fn patterns_sequence(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let seq = open_sequence_pattern(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchSequence {
        patterns: seq,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn patterns_single(p: &mut Parser) -> Option<Pattern> {
    pattern(p)
}

pub(crate) fn pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[pattern_as, or_pattern]);
    p.exit_rule();
    res
}

fn pattern_as(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[pattern_as_bound, |p| {
        if p.call_invalid_rules {
            invalid::as_pattern(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn pattern_as_bound(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let pat = or_pattern(p)?;
    p.expect_keyword_opt("as")?;
    let target = pattern_capture_target(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    let Expr::Name { id, .. } = target else {
        return None;
    };
    Some(Pattern::MatchAs {
        pattern: Some(Box::new(pat)),
        name: Some(id),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

pub(crate) fn or_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        let patterns = p.gather(closed_pattern, or_bar)?;
        let pos = p.extra(p.pos_at(m))?;
        let (end_lineno, end_col_offset) = ends(&pos);
        if patterns.len() == 1 {
            patterns.into_iter().next()
        } else {
            Some(Pattern::MatchOr {
                patterns,
                lineno: pos.lineno,
                col_offset: pos.col_offset,
                end_lineno,
                end_col_offset,
            })
        }
    });
    p.exit_rule();
    res
}

fn or_bar(p: &mut Parser) -> bool {
    p.expect(T::Vbar)
}

pub(crate) fn closed_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.closed_pattern) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[
        closed_pattern_literal,
        closed_pattern_capture,
        closed_pattern_wildcard,
        closed_pattern_value,
        closed_pattern_group,
        closed_pattern_sequence,
        closed_pattern_mapping,
        closed_pattern_class,
    ]);
    p.memo_insert(|t| &mut t.closed_pattern, m, res.clone());
    p.exit_rule();
    res
}

fn match_value(value: Expr, pos: super::Pos) -> Pattern {
    let (end_lineno, end_col_offset) = ends(&pos);
    Pattern::MatchValue {
        value: Box::new(value),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    }
}

fn match_singleton(value: Constant, pos: super::Pos) -> Pattern {
    let (end_lineno, end_col_offset) = ends(&pos);
    Pattern::MatchSingleton {
        value,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    }
}

fn closed_pattern_literal(p: &mut Parser) -> Option<Pattern> {
    literal_pattern(p)
}

fn closed_pattern_capture(p: &mut Parser) -> Option<Pattern> {
    capture_pattern(p)
}

fn closed_pattern_wildcard(p: &mut Parser) -> Option<Pattern> {
    wildcard_pattern(p)
}

fn closed_pattern_value(p: &mut Parser) -> Option<Pattern> {
    value_pattern(p)
}

fn closed_pattern_group(p: &mut Parser) -> Option<Pattern> {
    group_pattern(p)
}

fn closed_pattern_sequence(p: &mut Parser) -> Option<Pattern> {
    sequence_pattern(p)
}

fn closed_pattern_mapping(p: &mut Parser) -> Option<Pattern> {
    mapping_pattern(p)
}

fn closed_pattern_class(p: &mut Parser) -> Option<Pattern> {
    class_pattern(p)
}

pub(crate) fn literal_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        literal_pattern_signed_number,
        literal_pattern_complex_number,
        literal_pattern_strings,
        literal_pattern_none,
        literal_pattern_true,
        literal_pattern_false,
    ]);
    p.exit_rule();
    res
}

fn literal_pattern_signed_number(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let value = signed_number(p)?;
    if !not_plus_minus(p) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(match_value(value, pos))
}

fn literal_pattern_complex_number(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let value = complex_number(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(match_value(value, pos))
}

fn literal_pattern_strings(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let value = strings::strings(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(match_value(value, pos))
}

fn literal_pattern_none(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_keyword_opt("None")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(match_singleton(Constant::None, pos))
}

fn literal_pattern_true(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_keyword_opt("True")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(match_singleton(Constant::Bool(true), pos))
}

fn literal_pattern_false(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_keyword_opt("False")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(match_singleton(Constant::Bool(false), pos))
}

fn not_plus_minus(p: &mut Parser) -> bool {
    let m = p.mark;
    let hit = p.expect(T::Plus) || p.expect(T::Minus);
    p.mark = m;
    !hit
}

pub(crate) fn literal_expr(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        literal_expr_signed_number,
        literal_expr_complex_number,
        literal_expr_strings,
        literal_expr_none,
        literal_expr_true,
        literal_expr_false,
    ]);
    p.exit_rule();
    res
}

fn literal_expr_signed_number(p: &mut Parser) -> Option<Expr> {
    let a = signed_number(p)?;
    if not_plus_minus(p) { Some(a) } else { None }
}

fn literal_expr_complex_number(p: &mut Parser) -> Option<Expr> {
    complex_number(p)
}

fn literal_expr_strings(p: &mut Parser) -> Option<Expr> {
    strings::strings(p)
}

fn literal_expr_none(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("None")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Constant {
        value: Constant::None,
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn literal_expr_true(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("True")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Constant {
        value: Constant::Bool(true),
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn literal_expr_false(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("False")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Constant {
        value: Constant::Bool(false),
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn complex_number(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[complex_number_add, complex_number_sub]);
    p.exit_rule();
    res
}

fn complex_number_add(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let real = signed_real_number(p)?;
    p.expect_opt(T::Plus)?;
    let imag = imaginary_number(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::BinOp {
        left: Box::new(real),
        op: Box::new(Operator::Add),
        right: Box::new(imag),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn complex_number_sub(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let real = signed_real_number(p)?;
    p.expect_opt(T::Minus)?;
    let imag = imaginary_number(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::BinOp {
        left: Box::new(real),
        op: Box::new(Operator::Sub),
        right: Box::new(imag),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn signed_number(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[signed_number_plain, signed_number_negative]);
    p.exit_rule();
    res
}

fn signed_number_plain(p: &mut Parser) -> Option<Expr> {
    strings::number_token(p)
}

fn signed_number_negative(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Minus)?;
    let number = strings::number_token(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::UnaryOp {
        op: Box::new(Unaryop::USub),
        operand: Box::new(number),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn signed_real_number(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[signed_real_number_plain, signed_real_number_negative]);
    p.exit_rule();
    res
}

fn signed_real_number_plain(p: &mut Parser) -> Option<Expr> {
    real_number(p)
}

fn signed_real_number_negative(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Minus)?;
    let real = real_number(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::UnaryOp {
        op: Box::new(Unaryop::USub),
        operand: Box::new(real),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn real_number(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let real = strings::number_token(p)?;
        p.ensure_real(&real)
    });
    p.exit_rule();
    res
}

pub(crate) fn imaginary_number(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let imag = strings::number_token(p)?;
        p.ensure_imaginary(&imag)
    });
    p.exit_rule();
    res
}

pub(crate) fn capture_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        let target = pattern_capture_target(p)?;
        let pos = p.extra(p.pos_at(m))?;
        let (end_lineno, end_col_offset) = ends(&pos);
        let Expr::Name { id, .. } = target else {
            return None;
        };
        Some(Pattern::MatchAs {
            pattern: None,
            name: Some(id),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno,
            end_col_offset,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn pattern_capture_target(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        if !p.not_lookahead_soft_keyword("_") {
            return None;
        }
        let name = p.name_token()?;
        if p.lookahead(T::Dot) || p.lookahead(T::Lpar) || p.lookahead(T::Equal) {
            return None;
        }
        Some(set_expr_context(&name, ExprContext::Store))
    });
    p.exit_rule();
    res
}

pub(crate) fn wildcard_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        p.expect_soft_keyword_opt("_")?;
        let pos = p.extra(p.pos_at(m))?;
        let (end_lineno, end_col_offset) = ends(&pos);
        Some(Pattern::MatchAs {
            pattern: None,
            name: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno,
            end_col_offset,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn value_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        let attr = attr(p)?;
        if p.lookahead(T::Dot) || p.lookahead(T::Lpar) || p.lookahead(T::Equal) {
            return None;
        }
        let pos = p.extra(p.pos_at(m))?;
        Some(match_value(attr, pos))
    });
    p.exit_rule();
    res
}

pub(crate) fn attr(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.attr, attr_raw)
}

fn attr_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        let value = name_or_attr(p)?;
        p.expect_opt(T::Dot)?;
        let name = p.name_token()?;
        let pos = p.extra(p.pos_at(m))?;
        let Expr::Name { id, .. } = name else {
            return None;
        };
        Some(Expr::Attribute {
            value: Box::new(value),
            attr: id,
            ctx: Box::new(ExprContext::Load),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn name_or_attr(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[name_or_attr_attr, name_or_attr_name]);
    p.exit_rule();
    res
}

fn name_or_attr_attr(p: &mut Parser) -> Option<Expr> {
    attr(p)
}

fn name_or_attr_name(p: &mut Parser) -> Option<Expr> {
    p.name_token()
}

pub(crate) fn group_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        p.expect_opt(T::Lpar)?;
        let inner = pattern(p)?;
        p.expect_opt(T::Rpar)?;
        Some(inner)
    });
    p.exit_rule();
    res
}

pub(crate) fn sequence_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[sequence_pattern_list, sequence_pattern_paren]);
    p.exit_rule();
    res
}

fn sequence_pattern_list(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Lsqb)?;
    let patterns = p.attempt(maybe_sequence_pattern);
    p.expect_opt(T::Rsqb)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchSequence {
        patterns: patterns.unwrap_or_default(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn sequence_pattern_paren(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Lpar)?;
    let patterns = p.attempt(open_sequence_pattern);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchSequence {
        patterns: patterns.unwrap_or_default(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

pub(crate) fn open_sequence_pattern(p: &mut Parser) -> Option<Vec<Pattern>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let pattern = maybe_star_pattern(p)?;
        p.expect_opt(T::Comma)?;
        let patterns = p.attempt(maybe_sequence_pattern);
        Some(seq_insert_in_front(pattern, patterns))
    });
    p.exit_rule();
    res
}

pub(crate) fn maybe_sequence_pattern(p: &mut Parser) -> Option<Vec<Pattern>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let patterns = p.gather(maybe_star_pattern, pattern_comma)?;
        p.expect(T::Comma);
        Some(patterns)
    });
    p.exit_rule();
    res
}

fn pattern_comma(p: &mut Parser) -> bool {
    p.expect(T::Comma)
}

pub(crate) fn maybe_star_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[maybe_star_pattern_star, maybe_star_pattern_plain]);
    p.exit_rule();
    res
}

fn maybe_star_pattern_star(p: &mut Parser) -> Option<Pattern> {
    star_pattern(p)
}

fn maybe_star_pattern_plain(p: &mut Parser) -> Option<Pattern> {
    pattern(p)
}

pub(crate) fn star_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.star_pattern) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[star_pattern_capture, star_pattern_wildcard]);
    p.memo_insert(|t| &mut t.star_pattern, m, res.clone());
    p.exit_rule();
    res
}

fn star_pattern_capture(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    let target = pattern_capture_target(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    let Expr::Name { id, .. } = target else {
        return None;
    };
    Some(Pattern::MatchStar {
        name: Some(id),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn star_pattern_wildcard(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    wildcard_pattern(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchStar {
        name: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

pub(crate) fn mapping_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        mapping_pattern_empty,
        mapping_pattern_rest,
        mapping_pattern_items_rest,
        mapping_pattern_items,
    ]);
    p.exit_rule();
    res
}

fn mapping_pattern_empty(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchMapping {
        keys: Vec::new(),
        patterns: Vec::new(),
        rest: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn mapping_pattern_rest(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let rest = double_star_pattern(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    let Expr::Name { id, .. } = rest else {
        return None;
    };
    Some(Pattern::MatchMapping {
        keys: Vec::new(),
        patterns: Vec::new(),
        rest: Some(id),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn mapping_pattern_items_rest(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let items = items_pattern(p)?;
    p.expect_opt(T::Comma)?;
    let rest = double_star_pattern(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    let Expr::Name { id, .. } = rest else {
        return None;
    };
    Some(Pattern::MatchMapping {
        keys: get_pattern_keys(&items),
        patterns: get_patterns(&items),
        rest: Some(id),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn mapping_pattern_items(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let items = items_pattern(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchMapping {
        keys: get_pattern_keys(&items),
        patterns: get_patterns(&items),
        rest: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

pub(crate) fn items_pattern(p: &mut Parser) -> Option<Vec<KeyPatternPair>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.gather(key_value_pattern, pattern_comma);
    p.exit_rule();
    res
}

pub(crate) fn key_value_pattern(p: &mut Parser) -> Option<KeyPatternPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let key = p.alternatives(&[|p| literal_expr(p), |p| attr(p)])?;
        p.expect_opt(T::Colon)?;
        let pattern = pattern(p)?;
        Some((key, pattern))
    });
    p.exit_rule();
    res
}

pub(crate) fn double_star_pattern(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        p.expect_opt(T::Doublestar)?;
        pattern_capture_target(p)
    });
    p.exit_rule();
    res
}

pub(crate) fn class_pattern(p: &mut Parser) -> Option<Pattern> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        class_pattern_empty,
        class_pattern_positional,
        class_pattern_keywords,
        class_pattern_both,
        |p| {
            if p.call_invalid_rules {
                invalid::class_pattern(p);
            }
            None
        },
    ]);
    p.exit_rule();
    res
}

fn class_pattern_empty(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let cls = name_or_attr(p)?;
    p.expect_opt(T::Lpar)?;
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchClass {
        cls: Box::new(cls),
        patterns: Vec::new(),
        kwd_attrs: Vec::new(),
        kwd_patterns: Vec::new(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn class_pattern_positional(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let cls = name_or_attr(p)?;
    p.expect_opt(T::Lpar)?;
    let patterns = positional_patterns(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchClass {
        cls: Box::new(cls),
        patterns,
        kwd_attrs: Vec::new(),
        kwd_patterns: Vec::new(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn class_pattern_keywords(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let cls = name_or_attr(p)?;
    p.expect_opt(T::Lpar)?;
    let keywords = keyword_patterns(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchClass {
        cls: Box::new(cls),
        patterns: Vec::new(),
        kwd_attrs: map_names_to_ids(&get_pattern_keys(&keywords)),
        kwd_patterns: get_patterns(&keywords),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn class_pattern_both(p: &mut Parser) -> Option<Pattern> {
    let m = p.mark;
    let cls = name_or_attr(p)?;
    p.expect_opt(T::Lpar)?;
    let patterns = positional_patterns(p)?;
    p.expect_opt(T::Comma)?;
    let keywords = keyword_patterns(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(Pattern::MatchClass {
        cls: Box::new(cls),
        patterns,
        kwd_attrs: map_names_to_ids(&get_pattern_keys(&keywords)),
        kwd_patterns: get_patterns(&keywords),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

pub(crate) fn positional_patterns(p: &mut Parser) -> Option<Vec<Pattern>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.gather(pattern, pattern_comma);
    p.exit_rule();
    res
}

pub(crate) fn keyword_patterns(p: &mut Parser) -> Option<Vec<KeyPatternPair>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.gather(keyword_pattern, pattern_comma);
    p.exit_rule();
    res
}

pub(crate) fn keyword_pattern(p: &mut Parser) -> Option<KeyPatternPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let arg = p.name_token()?;
        p.expect_opt(T::Equal)?;
        let value = pattern(p)?;
        Some((arg, value))
    });
    p.exit_rule();
    res
}

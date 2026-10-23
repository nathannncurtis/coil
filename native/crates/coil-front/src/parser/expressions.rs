//! The grammar's expression rules, from `expressions` down to atoms, targets
//! and type expressions.

use super::Parser;
use super::helpers::*;
use super::invalid;
use super::statements;
use super::strings;
use crate::ast::Constant;
use crate::ast_generated::{
    Boolop, Cmpop, Comprehension, Expr, ExprContext, Keyword, Operator, Unaryop,
};
use crate::intern::Sym;
use crate::tokenizer::TokenKind as T;

fn tuple_expr(elts: Vec<Expr>, pos: super::Pos) -> Expr {
    Expr::Tuple {
        elts,
        ctx: Box::new(ExprContext::Load),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

pub(crate) fn expressions(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[expressions_tuple, expressions_one_comma, expressions_plain]);
    p.exit_rule();
    res
}

fn expressions_tuple(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = expression(p)?;
    let rest = p.sep_repeat1(expression, comma)?;
    p.expect(T::Comma);
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_expr(seq_insert_in_front(a, Some(rest)), pos))
}

fn expressions_one_comma(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = expression(p)?;
    p.expect_opt(T::Comma)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_expr(singleton_seq(a), pos))
}

fn expressions_plain(p: &mut Parser) -> Option<Expr> {
    expression(p)
}

fn comma(p: &mut Parser) -> bool {
    p.expect(T::Comma)
}

pub(crate) fn expression(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.expression) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::expression(p);
            }
            None
        },
        |p| {
            if p.call_invalid_rules {
                invalid::legacy_expression(p);
            }
            None
        },
        expression_ifexp,
        expression_disjunction,
        expression_lambdef,
    ]);
    p.memo_insert(|t| &mut t.expression, m, res.clone());
    p.exit_rule();
    res
}

fn expression_ifexp(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = disjunction(p)?;
    p.expect_keyword_opt("if")?;
    let b = disjunction(p)?;
    p.expect_keyword_opt("else")?;
    let c = expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::IfExp {
        test: Box::new(b),
        body: Box::new(a),
        orelse: Box::new(c),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn expression_disjunction(p: &mut Parser) -> Option<Expr> {
    disjunction(p)
}

fn expression_lambdef(p: &mut Parser) -> Option<Expr> {
    lambdef(p)
}

/// `expression_without_invalid`: the expression alternatives without the
/// invalid ones, as `invalid_expression` needs to probe past them.
pub(crate) fn expression_without_invalid(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[expression_ifexp, expression_disjunction, expression_lambdef]);
    p.exit_rule();
    res
}

pub(crate) fn yield_expr(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[yield_from_form, yield_form]);
    p.exit_rule();
    res
}

fn yield_from_form(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("yield")?;
    p.expect_keyword_opt("from")?;
    let a = expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::YieldFrom {
        value: Box::new(a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn yield_form(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("yield")?;
    let a = p.attempt(star_expressions);
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Yield {
        value: a.map(Box::new),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn star_expressions(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        star_expressions_tuple,
        star_expressions_one_comma,
        star_expressions_plain,
    ]);
    p.exit_rule();
    res
}

fn star_expressions_tuple(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = star_expression(p)?;
    let rest = p.sep_repeat1(star_expression, comma)?;
    p.expect(T::Comma);
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_expr(seq_insert_in_front(a, Some(rest)), pos))
}

fn star_expressions_one_comma(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = star_expression(p)?;
    p.expect_opt(T::Comma)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_expr(singleton_seq(a), pos))
}

fn star_expressions_plain(p: &mut Parser) -> Option<Expr> {
    star_expression(p)
}

pub(crate) fn star_expression(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.star_expression) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[star_expression_star, star_expression_plain]);
    p.memo_insert(|t| &mut t.star_expression, m, res.clone());
    p.exit_rule();
    res
}

fn starred_load(value: Expr, pos: super::Pos) -> Expr {
    Expr::Starred {
        value: Box::new(value),
        ctx: Box::new(ExprContext::Load),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn star_expression_star(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    let a = bitwise_or(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(starred_load(a, pos))
}

fn star_expression_plain(p: &mut Parser) -> Option<Expr> {
    expression(p)
}

pub(crate) fn star_named_expressions(p: &mut Parser) -> Option<Vec<Expr>> {
    if !p.enter_rule() {
        return None;
    }
    let res = star_named_expressions_seq(p);
    p.exit_rule();
    res
}

fn star_named_expressions_seq(p: &mut Parser) -> Option<Vec<Expr>> {
    let seq = p.gather(star_named_expression, comma)?;
    p.expect(T::Comma);
    Some(seq)
}

pub(crate) fn star_named_expression(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[star_named_expression_star, star_named_expression_named]);
    p.exit_rule();
    res
}

fn star_named_expression_star(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    let a = bitwise_or(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(starred_load(a, pos))
}

fn star_named_expression_named(p: &mut Parser) -> Option<Expr> {
    named_expression(p)
}

pub(crate) fn assignment_expression(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    // The '~' after ':=' commits: nothing later in this rule can match.
    let m = p.mark;
    let mut cut = false;
    let res = p.attempt(|p| {
        let a = p.name_token()?;
        p.expect_opt(T::Colonequal)?;
        cut = true;
        let b = expression(p)?;
        let pos = p.extra(p.pos_at(m))?;
        Some(Expr::NamedExpr {
            target: Box::new(set_expr_context(&a, ExprContext::Store)),
            value: Box::new(b),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    let _ = cut;
    p.exit_rule();
    res
}

pub(crate) fn named_expression(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        named_expression_assign,
        |p| {
            if p.call_invalid_rules {
                invalid::named_expression(p);
            }
            None
        },
        named_expression_plain,
    ]);
    p.exit_rule();
    res
}

fn named_expression_assign(p: &mut Parser) -> Option<Expr> {
    assignment_expression(p)
}

fn named_expression_plain(p: &mut Parser) -> Option<Expr> {
    let e = expression(p)?;
    if p.not_lookahead(T::Colonequal) {
        Some(e)
    } else {
        None
    }
}

fn bool_op(op: Boolop, values: Vec<Expr>, pos: super::Pos) -> Expr {
    Expr::BoolOp {
        op: Box::new(op),
        values,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

pub(crate) fn disjunction(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.disjunction) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[disjunction_or, disjunction_plain]);
    p.memo_insert(|t| &mut t.disjunction, m, res.clone());
    p.exit_rule();
    res
}

fn disjunction_or(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = conjunction(p)?;
    let rest = p.sep_repeat1(conjunction, or_keyword)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(bool_op(Boolop::Or, seq_insert_in_front(a, Some(rest)), pos))
}

fn or_keyword(p: &mut Parser) -> bool {
    p.expect_keyword("or")
}

fn disjunction_plain(p: &mut Parser) -> Option<Expr> {
    conjunction(p)
}

pub(crate) fn conjunction(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.conjunction) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[conjunction_and, conjunction_plain]);
    p.memo_insert(|t| &mut t.conjunction, m, res.clone());
    p.exit_rule();
    res
}

fn conjunction_and(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = inversion(p)?;
    let rest = p.sep_repeat1(inversion, and_keyword)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(bool_op(
        Boolop::And,
        seq_insert_in_front(a, Some(rest)),
        pos,
    ))
}

fn and_keyword(p: &mut Parser) -> bool {
    p.expect_keyword("and")
}

fn conjunction_plain(p: &mut Parser) -> Option<Expr> {
    inversion(p)
}

pub(crate) fn inversion(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.inversion) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[inversion_not, inversion_comparison]);
    p.memo_insert(|t| &mut t.inversion, m, res.clone());
    p.exit_rule();
    res
}

fn inversion_not(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("not")?;
    let a = inversion(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::UnaryOp {
        op: Box::new(Unaryop::Not),
        operand: Box::new(a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn inversion_comparison(p: &mut Parser) -> Option<Expr> {
    comparison(p)
}

pub(crate) fn comparison(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[comparison_chain, comparison_plain]);
    p.exit_rule();
    res
}

fn comparison_chain(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = bitwise_or(p)?;
    let rest = p.repeat1(compare_op_bitwise_or_pair)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Compare {
        left: Box::new(a),
        ops: get_cmpops(&rest),
        comparators: get_exprs(&rest),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn comparison_plain(p: &mut Parser) -> Option<Expr> {
    bitwise_or(p)
}

pub(crate) fn compare_op_bitwise_or_pair(p: &mut Parser) -> Option<CmpopExprPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        eq_bitwise_or,
        noteq_bitwise_or,
        lte_bitwise_or,
        lt_bitwise_or,
        gte_bitwise_or,
        gt_bitwise_or,
        notin_bitwise_or,
        in_bitwise_or,
        isnot_bitwise_or,
        is_bitwise_or,
    ]);
    p.exit_rule();
    res
}

fn cmp_pair(op: Cmpop, p: &mut Parser) -> Option<CmpopExprPair> {
    Some((op, bitwise_or(p)?))
}

fn eq_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_opt(T::Eqequal)?;
    cmp_pair(Cmpop::Eq, p)
}

fn noteq_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    // `check_barry_as_flufl` with the default flags accepts every '!='.
    p.expect_opt(T::Notequal)?;
    cmp_pair(Cmpop::NotEq, p)
}

fn lte_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_opt(T::Lessequal)?;
    cmp_pair(Cmpop::LtE, p)
}

fn lt_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_opt(T::Less)?;
    cmp_pair(Cmpop::Lt, p)
}

fn gte_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_opt(T::Greaterequal)?;
    cmp_pair(Cmpop::GtE, p)
}

fn gt_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_opt(T::Greater)?;
    cmp_pair(Cmpop::Gt, p)
}

fn notin_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_keyword_opt("not")?;
    p.expect_keyword_opt("in")?;
    cmp_pair(Cmpop::NotIn, p)
}

fn in_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_keyword_opt("in")?;
    cmp_pair(Cmpop::In, p)
}

fn isnot_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_keyword_opt("is")?;
    p.expect_keyword_opt("not")?;
    cmp_pair(Cmpop::IsNot, p)
}

fn is_bitwise_or(p: &mut Parser) -> Option<CmpopExprPair> {
    p.expect_keyword_opt("is")?;
    cmp_pair(Cmpop::Is, p)
}

// The binary chains are left-recursive: the wrapper grows the seed and the
// raw rule holds the alternatives.

fn binop(a: Expr, op: Operator, b: Expr, pos: super::Pos) -> Expr {
    Expr::BinOp {
        left: Box::new(a),
        op: Box::new(op),
        right: Box::new(b),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

pub(crate) fn bitwise_or(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.bitwise_or, bitwise_or_raw)
}

fn bitwise_or_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[bitwise_or_op, bitwise_or_base]);
    p.exit_rule();
    res
}

fn bitwise_or_op(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = bitwise_or(p)?;
    p.expect_opt(T::Vbar)?;
    let b = bitwise_xor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::BitOr, b, pos))
}

fn bitwise_or_base(p: &mut Parser) -> Option<Expr> {
    bitwise_xor(p)
}

pub(crate) fn bitwise_xor(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.bitwise_xor, bitwise_xor_raw)
}

fn bitwise_xor_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[bitwise_xor_op, bitwise_xor_base]);
    p.exit_rule();
    res
}

fn bitwise_xor_op(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = bitwise_xor(p)?;
    p.expect_opt(T::Circumflex)?;
    let b = bitwise_and(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::BitXor, b, pos))
}

fn bitwise_xor_base(p: &mut Parser) -> Option<Expr> {
    bitwise_and(p)
}

pub(crate) fn bitwise_and(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.bitwise_and, bitwise_and_raw)
}

fn bitwise_and_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[bitwise_and_op, bitwise_and_base]);
    p.exit_rule();
    res
}

fn bitwise_and_op(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = bitwise_and(p)?;
    p.expect_opt(T::Amber)?;
    let b = shift_expr(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::BitAnd, b, pos))
}

fn bitwise_and_base(p: &mut Parser) -> Option<Expr> {
    shift_expr(p)
}

pub(crate) fn shift_expr(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.shift_expr, shift_expr_raw)
}

fn shift_expr_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[shift_expr_left, shift_expr_right, shift_expr_base]);
    p.exit_rule();
    res
}

fn shift_expr_left(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = shift_expr(p)?;
    p.expect_opt(T::Leftshift)?;
    let b = sum(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::LShift, b, pos))
}

fn shift_expr_right(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = shift_expr(p)?;
    p.expect_opt(T::Rightshift)?;
    let b = sum(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::RShift, b, pos))
}

fn shift_expr_base(p: &mut Parser) -> Option<Expr> {
    sum(p)
}

pub(crate) fn sum(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.sum, sum_raw)
}

fn sum_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[sum_add, sum_sub, sum_base]);
    p.exit_rule();
    res
}

fn sum_add(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = sum(p)?;
    p.expect_opt(T::Plus)?;
    let b = term(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::Add, b, pos))
}

fn sum_sub(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = sum(p)?;
    p.expect_opt(T::Minus)?;
    let b = term(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::Sub, b, pos))
}

fn sum_base(p: &mut Parser) -> Option<Expr> {
    term(p)
}

pub(crate) fn term(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.term, term_raw)
}

fn term_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        term_mult,
        term_div,
        term_floordiv,
        term_mod,
        term_matmult,
        term_base,
    ]);
    p.exit_rule();
    res
}

fn term_mult(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = term(p)?;
    p.expect_opt(T::Star)?;
    let b = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::Mult, b, pos))
}

fn term_div(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = term(p)?;
    p.expect_opt(T::Slash)?;
    let b = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::Div, b, pos))
}

fn term_floordiv(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = term(p)?;
    p.expect_opt(T::Doubleslash)?;
    let b = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::FloorDiv, b, pos))
}

fn term_mod(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = term(p)?;
    p.expect_opt(T::Percent)?;
    let b = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::Mod, b, pos))
}

fn term_matmult(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = term(p)?;
    p.expect_opt(T::At)?;
    let b = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::MatMult, b, pos))
}

fn term_base(p: &mut Parser) -> Option<Expr> {
    factor(p)
}

pub(crate) fn factor(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.factor) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[factor_plus, factor_minus, factor_invert, factor_power]);
    p.memo_insert(|t| &mut t.factor, m, res.clone());
    p.exit_rule();
    res
}

fn unary(op: Unaryop, a: Expr, pos: super::Pos) -> Expr {
    Expr::UnaryOp {
        op: Box::new(op),
        operand: Box::new(a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn factor_plus(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Plus)?;
    let a = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(unary(Unaryop::UAdd, a, pos))
}

fn factor_minus(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Minus)?;
    let a = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(unary(Unaryop::USub, a, pos))
}

fn factor_invert(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Tilde)?;
    let a = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(unary(Unaryop::Invert, a, pos))
}

fn factor_power(p: &mut Parser) -> Option<Expr> {
    power(p)
}

pub(crate) fn power(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[power_op, power_base]);
    p.exit_rule();
    res
}

fn power_op(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = await_primary(p)?;
    p.expect_opt(T::Doublestar)?;
    let b = factor(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(binop(a, Operator::Pow, b, pos))
}

fn power_base(p: &mut Parser) -> Option<Expr> {
    await_primary(p)
}

pub(crate) fn await_primary(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.await_primary) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[await_primary_await, await_primary_plain]);
    p.memo_insert(|t| &mut t.await_primary, m, res.clone());
    p.exit_rule();
    res
}

fn await_primary_await(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Await)?;
    let a = primary(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Await {
        value: Box::new(a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn await_primary_plain(p: &mut Parser) -> Option<Expr> {
    primary(p)
}

/// The `Call` a `primary` alternative builds from a parsed `arguments`
/// result, whose func was the parser's dummy name.
fn call_from_arguments(func: Expr, args_call: Expr, pos: super::Pos) -> Option<Expr> {
    let Expr::Call { args, keywords, .. } = args_call else {
        return None;
    };
    Some(Expr::Call {
        func: Box::new(func),
        args,
        keywords,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn primary(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.primary, primary_raw)
}

fn primary_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        primary_attr,
        primary_genexp_call,
        primary_call,
        primary_subscript,
        primary_atom,
    ]);
    p.exit_rule();
    res
}

fn primary_attr(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = primary(p)?;
    p.expect_opt(T::Dot)?;
    let b = p.name_token()?;
    let pos = p.extra(p.pos_at(m))?;
    let Expr::Name { id, .. } = b else {
        return None;
    };
    Some(Expr::Attribute {
        value: Box::new(a),
        attr: id,
        ctx: Box::new(ExprContext::Load),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn primary_genexp_call(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = primary(p)?;
    let b = genexp(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Call {
        func: Box::new(a),
        args: singleton_seq(b),
        keywords: Vec::new(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn primary_call(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = primary(p)?;
    p.expect_opt(T::Lpar)?;
    let b = p.attempt(arguments);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    match b {
        Some(call) => call_from_arguments(a, call, pos),
        None => Some(Expr::Call {
            func: Box::new(a),
            args: Vec::new(),
            keywords: Vec::new(),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        }),
    }
}

fn primary_subscript(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = primary(p)?;
    p.expect_opt(T::Lsqb)?;
    let b = slices(p)?;
    p.expect_opt(T::Rsqb)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Subscript {
        value: Box::new(a),
        slice: Box::new(b),
        ctx: Box::new(ExprContext::Load),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn primary_atom(p: &mut Parser) -> Option<Expr> {
    atom(p)
}

pub(crate) fn slices(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[slices_one, slices_tuple]);
    p.exit_rule();
    res
}

fn slices_one(p: &mut Parser) -> Option<Expr> {
    let a = slice(p)?;
    if p.not_lookahead(T::Comma) {
        Some(a)
    } else {
        None
    }
}

fn slice_or_starred(p: &mut Parser) -> Option<Expr> {
    if let Some(s) = slice(p) {
        return Some(s);
    }
    star_expression(p)
}

fn slices_tuple(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let elts = p.gather(slice_or_starred, comma)?;
    p.expect(T::Comma);
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_expr(elts, pos))
}

pub(crate) fn slice(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[slice_full, slice_named]);
    p.exit_rule();
    res
}

fn slice_full(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = p.attempt(expression);
    p.expect_opt(T::Colon)?;
    let b = p.attempt(expression);
    let c = if p.expect(T::Colon) {
        p.attempt(expression)
    } else {
        None
    };
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Slice {
        lower: a.map(Box::new),
        upper: b.map(Box::new),
        step: c.map(Box::new),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn slice_named(p: &mut Parser) -> Option<Expr> {
    named_expression(p)
}

pub(crate) fn atom(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        atom_name,
        atom_true,
        atom_false,
        atom_none,
        atom_strings,
        atom_number,
        atom_paren,
        atom_bracket,
        atom_brace,
        atom_ellipsis,
    ]);
    p.exit_rule();
    res
}

fn constant(value: Constant, pos: super::Pos) -> Expr {
    Expr::Constant {
        value,
        kind: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn atom_name(p: &mut Parser) -> Option<Expr> {
    p.name_token()
}

fn atom_true(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("True")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(constant(Constant::Bool(true), pos))
}

fn atom_false(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("False")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(constant(Constant::Bool(false), pos))
}

fn atom_none(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_keyword_opt("None")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(constant(Constant::None, pos))
}

fn atom_strings(p: &mut Parser) -> Option<Expr> {
    if !(p.lookahead(T::String) || p.lookahead(T::FStringStart)) {
        return None;
    }
    strings::strings(p)
}

fn atom_number(p: &mut Parser) -> Option<Expr> {
    strings::number_token(p)
}

fn atom_paren(p: &mut Parser) -> Option<Expr> {
    if !p.lookahead(T::Lpar) {
        return None;
    }
    if let Some(t) = tuple(p) {
        return Some(t);
    }
    if let Some(g) = group(p) {
        return Some(g);
    }
    genexp(p)
}

fn atom_bracket(p: &mut Parser) -> Option<Expr> {
    if !p.lookahead(T::Lsqb) {
        return None;
    }
    if let Some(l) = list(p) {
        return Some(l);
    }
    listcomp(p)
}

fn atom_brace(p: &mut Parser) -> Option<Expr> {
    if !p.lookahead(T::Lbrace) {
        return None;
    }
    if let Some(d) = dict(p) {
        return Some(d);
    }
    if let Some(s) = set(p) {
        return Some(s);
    }
    if let Some(c) = dictcomp(p) {
        return Some(c);
    }
    setcomp(p)
}

fn atom_ellipsis(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Ellipsis)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(constant(Constant::Ellipsis, pos))
}

pub(crate) fn group(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[group_paren, |p| {
        if p.call_invalid_rules {
            invalid::group(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn group_paren(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lpar)?;
    let inner = yield_expr(p).or_else(|| named_expression(p))?;
    p.expect_opt(T::Rpar)?;
    Some(inner)
}

// -- lambdas --------------------------------------------------------------------

pub(crate) fn lambdef(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        p.expect_keyword_opt("lambda")?;
        let a = p.attempt(lambda_params);
        p.expect_opt(T::Colon)?;
        let b = expression(p)?;
        let pos = p.extra(p.pos_at(m))?;
        Some(Expr::Lambda {
            args: Box::new(a.unwrap_or_else(Parser::empty_arguments)),
            body: Box::new(b),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn lambda_params(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::lambda_parameters(p);
            }
            None
        },
        lambda_parameters,
    ]);
    p.exit_rule();
    res
}

fn lambda_parameters(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        lambda_parameters_slash_no_default,
        lambda_parameters_slash_with_default,
        lambda_parameters_plain,
        lambda_parameters_with_default,
        lambda_parameters_star,
    ]);
    p.exit_rule();
    res
}

fn lambda_parameters_slash_no_default(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = lambda_slash_no_default(p)?;
    let b = p.repeat0(lambda_param_no_default);
    let c = p.repeat0(lambda_param_with_default);
    let d = p.attempt(lambda_star_etc);
    Some(Parser::make_arguments(Some(a), None, Some(b), Some(c), d))
}

fn lambda_parameters_slash_with_default(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = lambda_slash_with_default(p)?;
    let b = p.repeat0(lambda_param_with_default);
    let c = p.attempt(lambda_star_etc);
    Some(Parser::make_arguments(None, Some(a), None, Some(b), c))
}

fn lambda_parameters_plain(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = p.repeat1(lambda_param_no_default)?;
    let b = p.repeat0(lambda_param_with_default);
    let c = p.attempt(lambda_star_etc);
    Some(Parser::make_arguments(None, None, Some(a), Some(b), c))
}

fn lambda_parameters_with_default(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = p.repeat1(lambda_param_with_default)?;
    let b = p.attempt(lambda_star_etc);
    Some(Parser::make_arguments(None, None, None, Some(a), b))
}

fn lambda_parameters_star(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = lambda_star_etc(p)?;
    Some(Parser::make_arguments(None, None, None, None, Some(a)))
}

pub(crate) fn lambda_slash_no_default(p: &mut Parser) -> Option<Vec<crate::ast_generated::Arg>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[slash_no_default_comma, slash_no_default_colon]);
    p.exit_rule();
    res
}

fn slash_no_default_comma(p: &mut Parser) -> Option<Vec<crate::ast_generated::Arg>> {
    let a = p.repeat1(lambda_param_no_default)?;
    p.expect_opt(T::Slash)?;
    p.expect_opt(T::Comma)?;
    Some(a)
}

fn slash_no_default_colon(p: &mut Parser) -> Option<Vec<crate::ast_generated::Arg>> {
    let a = p.repeat1(lambda_param_no_default)?;
    p.expect_opt(T::Slash)?;
    if !p.lookahead(T::Colon) {
        return None;
    }
    Some(a)
}

pub(crate) fn lambda_slash_with_default(p: &mut Parser) -> Option<SlashWithDefault> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        lambda_slash_with_default_comma,
        lambda_slash_with_default_colon,
    ]);
    p.exit_rule();
    res
}

fn lambda_slash_with_default_comma(p: &mut Parser) -> Option<SlashWithDefault> {
    let a = p.repeat0(lambda_param_no_default);
    let b = p.repeat1(lambda_param_with_default)?;
    p.expect_opt(T::Slash)?;
    p.expect_opt(T::Comma)?;
    Some((a, b))
}

fn lambda_slash_with_default_colon(p: &mut Parser) -> Option<SlashWithDefault> {
    let a = p.repeat0(lambda_param_no_default);
    let b = p.repeat1(lambda_param_with_default)?;
    p.expect_opt(T::Slash)?;
    if !p.lookahead(T::Colon) {
        return None;
    }
    Some((a, b))
}

pub(crate) fn lambda_star_etc(p: &mut Parser) -> Option<StarEtc> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::lambda_star_etc(p);
            }
            None
        },
        lambda_star_etc_named,
        lambda_star_etc_bare,
        lambda_star_etc_kwds,
    ]);
    p.exit_rule();
    res
}

fn lambda_star_etc_named(p: &mut Parser) -> Option<StarEtc> {
    p.expect_opt(T::Star)?;
    let a = lambda_param_no_default(p)?;
    let b = p.repeat0(lambda_param_maybe_default);
    let c = p.attempt(lambda_kwds);
    Some((Some(a), b, c))
}

fn lambda_star_etc_bare(p: &mut Parser) -> Option<StarEtc> {
    p.expect_opt(T::Star)?;
    p.expect_opt(T::Comma)?;
    let b = p.repeat1(lambda_param_maybe_default)?;
    let c = p.attempt(lambda_kwds);
    Some((None, b, c))
}

fn lambda_star_etc_kwds(p: &mut Parser) -> Option<StarEtc> {
    let a = lambda_kwds(p)?;
    Some((None, Vec::new(), Some(a)))
}

pub(crate) fn lambda_kwds(p: &mut Parser) -> Option<crate::ast_generated::Arg> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::lambda_kwds(p);
            }
            None
        },
        lambda_kwds_plain,
    ]);
    p.exit_rule();
    res
}

fn lambda_kwds_plain(p: &mut Parser) -> Option<crate::ast_generated::Arg> {
    p.expect_opt(T::Doublestar)?;
    lambda_param_no_default(p)
}

pub(crate) fn lambda_param_no_default(p: &mut Parser) -> Option<crate::ast_generated::Arg> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[lambda_param_no_default_comma, lambda_param_no_default_colon]);
    p.exit_rule();
    res
}

fn lambda_param_no_default_comma(p: &mut Parser) -> Option<crate::ast_generated::Arg> {
    let a = lambda_param(p)?;
    p.expect_opt(T::Comma)?;
    Some(a)
}

fn lambda_param_no_default_colon(p: &mut Parser) -> Option<crate::ast_generated::Arg> {
    let a = lambda_param(p)?;
    if !p.lookahead(T::Colon) {
        return None;
    }
    Some(a)
}

pub(crate) fn lambda_param_with_default(p: &mut Parser) -> Option<NameDefaultPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        lambda_param_with_default_comma,
        lambda_param_with_default_colon,
    ]);
    p.exit_rule();
    res
}

fn lambda_param_with_default_comma(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = lambda_param(p)?;
    let c = default(p)?;
    p.expect_opt(T::Comma)?;
    Some((a, Some(c)))
}

fn lambda_param_with_default_colon(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = lambda_param(p)?;
    let c = default(p)?;
    if !p.lookahead(T::Colon) {
        return None;
    }
    Some((a, Some(c)))
}

pub(crate) fn lambda_param_maybe_default(p: &mut Parser) -> Option<NameDefaultPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        lambda_param_maybe_default_comma,
        lambda_param_maybe_default_colon,
    ]);
    p.exit_rule();
    res
}

fn lambda_param_maybe_default_comma(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = lambda_param(p)?;
    let c = p.attempt(default);
    p.expect_opt(T::Comma)?;
    Some((a, c))
}

fn lambda_param_maybe_default_colon(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = lambda_param(p)?;
    let c = p.attempt(default);
    if !p.lookahead(T::Colon) {
        return None;
    }
    Some((a, c))
}

pub(crate) fn lambda_param(p: &mut Parser) -> Option<crate::ast_generated::Arg> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        let a = p.name_token()?;
        let pos = p.extra(p.pos_at(m))?;
        let Expr::Name { id, .. } = a else {
            return None;
        };
        Some(crate::ast_generated::Arg {
            arg: id,
            annotation: None,
            type_comment: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn default(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[default_plain, |p| {
        if p.call_invalid_rules {
            invalid::default(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn default_plain(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Equal)?;
    expression(p)
}

// -- displays -------------------------------------------------------------------

pub(crate) fn list(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        p.expect_opt(T::Lsqb)?;
        let a = p.attempt(star_named_expressions);
        p.expect_opt(T::Rsqb)?;
        let pos = p.extra(p.pos_at(m))?;
        Some(Expr::List {
            elts: a.unwrap_or_default(),
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

pub(crate) fn tuple(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let m = p.mark;
        p.expect_opt(T::Lpar)?;
        // [y=star_named_expression ',' z=[star_named_expressions]]
        let a = p.attempt(|p| {
            let y = star_named_expression(p)?;
            p.expect_opt(T::Comma)?;
            let z = p.attempt(star_named_expressions);
            Some(seq_insert_in_front(y, z))
        });
        p.expect_opt(T::Rpar)?;
        let pos = p.extra(p.pos_at(m))?;
        Some(tuple_expr(a.unwrap_or_default(), pos))
    });
    p.exit_rule();
    res
}

pub(crate) fn set(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let m = p.mark;
    let res = p.attempt(|p| {
        p.expect_opt(T::Lbrace)?;
        let a = star_named_expressions(p)?;
        p.expect_opt(T::Rbrace)?;
        let pos = p.extra(p.pos_at(m))?;
        Some(Expr::Set {
            elts: a,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    p.exit_rule();
    res
}

pub(crate) fn dict(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[dict_pairs, dict_brace_invalid]);
    p.exit_rule();
    res
}

fn dict_pairs(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let a = p.attempt(double_starred_kvpairs);
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    let pairs = a.unwrap_or_default();
    Some(Expr::Dict {
        keys: get_keys(&pairs),
        values: get_values(&pairs),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn dict_brace_invalid(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lbrace)?;
    if p.call_invalid_rules {
        invalid::double_starred_kvpairs(p);
    }
    p.expect_opt(T::Rbrace)?;
    None
}

pub(crate) fn double_starred_kvpairs(p: &mut Parser) -> Option<Vec<KeyValuePair>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let seq = p.gather(double_starred_kvpair, comma)?;
        p.expect(T::Comma);
        Some(seq)
    });
    p.exit_rule();
    res
}

pub(crate) fn double_starred_kvpair(p: &mut Parser) -> Option<KeyValuePair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[double_starred_kvpair_unpack, double_starred_kvpair_plain]);
    p.exit_rule();
    res
}

fn double_starred_kvpair_unpack(p: &mut Parser) -> Option<KeyValuePair> {
    p.expect_opt(T::Doublestar)?;
    let a = bitwise_or(p)?;
    Some((None, a))
}

fn double_starred_kvpair_plain(p: &mut Parser) -> Option<KeyValuePair> {
    kvpair(p)
}

pub(crate) fn kvpair(p: &mut Parser) -> Option<KeyValuePair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let a = expression(p)?;
        p.expect_opt(T::Colon)?;
        let b = expression(p)?;
        Some((Some(a), b))
    });
    p.exit_rule();
    res
}

// -- comprehensions ---------------------------------------------------------------

pub(crate) fn for_if_clauses(p: &mut Parser) -> Option<Vec<Comprehension>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.repeat1(for_if_clause);
    p.exit_rule();
    res
}

fn for_if_clause(p: &mut Parser) -> Option<Comprehension> {
    if !p.enter_rule() {
        return None;
    }
    // The '~' after 'in' commits: once past it, later alternatives never run.
    let mut cut = false;
    let mut res = None;
    if !p.has_error() {
        res = p.attempt(|p| {
            p.expect_opt(T::Async)?;
            p.expect_keyword_opt("for")?;
            let a = statements::star_targets(p)?;
            p.expect_keyword_opt("in")?;
            cut = true;
            let b = disjunction(p)?;
            let c = p.repeat0(for_if_clause_if);
            Some(comprehension(a, b, c, 1))
        });
    }
    if res.is_none() && !cut && !p.has_error() {
        res = p.attempt(|p| {
            p.expect_keyword_opt("for")?;
            let a = statements::star_targets(p)?;
            p.expect_keyword_opt("in")?;
            cut = true;
            let b = disjunction(p)?;
            let c = p.repeat0(for_if_clause_if);
            Some(comprehension(a, b, c, 0))
        });
    }
    if res.is_none() && !cut && !p.has_error() && p.call_invalid_rules {
        p.attempt(|p| {
            invalid::for_target(p);
            None::<Comprehension>
        });
    }
    p.exit_rule();
    res
}

fn comprehension(target: Expr, iter: Expr, ifs: Vec<Expr>, is_async: i32) -> Comprehension {
    Comprehension {
        target: Box::new(target),
        iter: Box::new(iter),
        ifs,
        is_async,
    }
}

fn for_if_clause_if(p: &mut Parser) -> Option<Expr> {
    p.expect_keyword_opt("if")?;
    disjunction(p)
}

pub(crate) fn listcomp(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[listcomp_plain, |p| {
        if p.call_invalid_rules {
            invalid::comprehension(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn comp(
    elt: Expr,
    generators: Vec<Comprehension>,
    make: fn(Vec<Comprehension>, super::Pos, Expr) -> Expr,
    pos: super::Pos,
) -> Expr {
    make(generators, pos, elt)
}

fn make_listcomp(generators: Vec<Comprehension>, pos: super::Pos, elt: Expr) -> Expr {
    Expr::ListComp {
        elt: Box::new(elt),
        generators,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn listcomp_plain(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lsqb)?;
    let a = named_expression(p)?;
    let b = for_if_clauses(p)?;
    p.expect_opt(T::Rsqb)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(comp(a, b, make_listcomp, pos))
}

pub(crate) fn setcomp(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[setcomp_plain, |p| {
        if p.call_invalid_rules {
            invalid::comprehension(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn make_setcomp(generators: Vec<Comprehension>, pos: super::Pos, elt: Expr) -> Expr {
    Expr::SetComp {
        elt: Box::new(elt),
        generators,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn setcomp_plain(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let a = named_expression(p)?;
    let b = for_if_clauses(p)?;
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(comp(a, b, make_setcomp, pos))
}

pub(crate) fn genexp(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[genexp_plain, |p| {
        if p.call_invalid_rules {
            invalid::comprehension(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn genexp_element(p: &mut Parser) -> Option<Expr> {
    if let Some(e) = assignment_expression(p) {
        return Some(e);
    }
    let e = expression(p)?;
    if p.not_lookahead(T::Colonequal) {
        Some(e)
    } else {
        None
    }
}

fn genexp_plain(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lpar)?;
    let a = genexp_element(p)?;
    let b = for_if_clauses(p)?;
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::GeneratorExp {
        elt: Box::new(a),
        generators: b,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn dictcomp(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[dictcomp_plain, |p| {
        if p.call_invalid_rules {
            invalid::dict_comprehension(p);
        }
        None
    }]);
    p.exit_rule();
    res
}

fn dictcomp_plain(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lbrace)?;
    let a = kvpair(p)?;
    let b = for_if_clauses(p)?;
    p.expect_opt(T::Rbrace)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::DictComp {
        key: Box::new(a.0.expect("kvpair has a key")),
        value: Box::new(a.1),
        generators: b,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

// -- call arguments ----------------------------------------------------------------

pub(crate) fn arguments(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.arguments) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[arguments_args, |p| {
        if p.call_invalid_rules {
            invalid::arguments(p);
        }
        None
    }]);
    p.memo_insert(|t| &mut t.arguments, m, res.clone());
    p.exit_rule();
    res
}

fn arguments_args(p: &mut Parser) -> Option<Expr> {
    let a = args(p)?;
    p.expect(T::Comma);
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some(a)
}

pub(crate) fn args(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[args_collect, args_kwargs]);
    p.exit_rule();
    res
}

fn args_collect(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = p.gather(args_item, comma)?;
    let b = if p.expect(T::Comma) { kwargs(p) } else { None };
    let pos = p.extra(p.pos_at(m))?;
    p.collect_call_seqs(a, b, pos)
}

fn args_item(p: &mut Parser) -> Option<Expr> {
    if let Some(e) = starred_expression(p) {
        return Some(e);
    }
    let e = assignment_expression(p).or_else(|| {
        let e = expression(p)?;
        if p.not_lookahead(T::Colonequal) {
            Some(e)
        } else {
            None
        }
    })?;
    if p.not_lookahead(T::Equal) {
        Some(e)
    } else {
        None
    }
}

fn args_kwargs(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = kwargs(p)?;
    let pos = p.extra(p.pos_at(m))?;
    // A kwargs-only call: starred exprs become args, keywords stay keywords.
    let args: Vec<Expr> = a
        .iter()
        .filter_map(|item| match item {
            KeywordOrStarred::Starred(e) => Some(e.clone()),
            KeywordOrStarred::Keyword(_) => None,
        })
        .collect();
    let keywords = a
        .into_iter()
        .filter_map(|item| match item {
            KeywordOrStarred::Keyword(k) => Some(k),
            KeywordOrStarred::Starred(_) => None,
        })
        .collect();
    p.collect_call_seqs(args, None, pos)
        .map(|call| replace_call_keywords(call, keywords))
}

fn replace_call_keywords(mut call: Expr, keywords: Vec<Keyword>) -> Expr {
    if let Expr::Call { keywords: k, .. } = &mut call {
        *k = keywords;
    }
    call
}

pub(crate) fn kwargs(p: &mut Parser) -> Option<Vec<KeywordOrStarred>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[kwargs_mixed, kwargs_starred, kwargs_double_starred]);
    p.exit_rule();
    res
}

fn kwargs_mixed(p: &mut Parser) -> Option<Vec<KeywordOrStarred>> {
    let a = p.gather(kwarg_or_starred, comma)?;
    p.expect_opt(T::Comma)?;
    let b = p.gather(kwarg_or_double_starred, comma)?;
    let mut out = a;
    out.extend(b);
    Some(out)
}

fn kwargs_starred(p: &mut Parser) -> Option<Vec<KeywordOrStarred>> {
    p.gather(kwarg_or_starred, comma)
}

fn kwargs_double_starred(p: &mut Parser) -> Option<Vec<KeywordOrStarred>> {
    p.gather(kwarg_or_double_starred, comma)
}

pub(crate) fn starred_expression(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::starred_expression(p);
            }
            None
        },
        starred_expression_star,
        starred_expression_bare,
    ]);
    p.exit_rule();
    res
}

fn starred_expression_star(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    let a = expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(starred_load(a, pos))
}

fn starred_expression_bare(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Star)?;
    p.raise_syntax_error("Invalid star expression".to_string());
    None
}

pub(crate) fn kwarg_or_starred(p: &mut Parser) -> Option<KeywordOrStarred> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::kwarg(p);
            }
            None
        },
        kwarg_or_starred_named,
        kwarg_or_starred_starred,
    ]);
    p.exit_rule();
    res
}

fn kwarg_or_starred_named(p: &mut Parser) -> Option<KeywordOrStarred> {
    let m = p.mark;
    let a = p.name_token()?;
    p.expect_opt(T::Equal)?;
    let b = expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let Expr::Name { id, .. } = a else {
        return None;
    };
    Some(KeywordOrStarred::Keyword(Keyword {
        arg: Some(id),
        value: Box::new(b),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }))
}

fn kwarg_or_starred_starred(p: &mut Parser) -> Option<KeywordOrStarred> {
    let a = starred_expression(p)?;
    Some(KeywordOrStarred::Starred(a))
}

pub(crate) fn kwarg_or_double_starred(p: &mut Parser) -> Option<KeywordOrStarred> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        |p| {
            if p.call_invalid_rules {
                invalid::kwarg(p);
            }
            None
        },
        kwarg_or_double_starred_named,
        kwarg_or_double_starred_unpack,
    ]);
    p.exit_rule();
    res
}

fn kwarg_or_double_starred_named(p: &mut Parser) -> Option<KeywordOrStarred> {
    let m = p.mark;
    let a = p.name_token()?;
    p.expect_opt(T::Equal)?;
    let b = expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let Expr::Name { id, .. } = a else {
        return None;
    };
    Some(KeywordOrStarred::Keyword(Keyword {
        arg: Some(id),
        value: Box::new(b),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }))
}

fn kwarg_or_double_starred_unpack(p: &mut Parser) -> Option<KeywordOrStarred> {
    let m = p.mark;
    p.expect_opt(T::Doublestar)?;
    let a = expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(KeywordOrStarred::Keyword(Keyword {
        arg: None,
        value: Box::new(a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }))
}

// -- assignment targets -------------------------------------------------------------

fn tuple_ctx(elts: Vec<Expr>, ctx: ExprContext, pos: super::Pos) -> Expr {
    Expr::Tuple {
        elts,
        ctx: Box::new(ctx),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

pub(crate) fn star_targets(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[star_targets_single, star_targets_list]);
    p.exit_rule();
    res
}

fn star_targets_single(p: &mut Parser) -> Option<Expr> {
    let a = star_target(p)?;
    if p.not_lookahead(T::Comma) {
        Some(a)
    } else {
        None
    }
}

fn star_targets_list(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = star_target(p)?;
    let rest = p.repeat0(star_targets_item);
    p.expect(T::Comma);
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_ctx(
        seq_insert_in_front(a, Some(rest)),
        ExprContext::Store,
        pos,
    ))
}

fn star_targets_item(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Comma)?;
    star_target(p)
}

pub(crate) fn star_targets_list_seq(p: &mut Parser) -> Option<Vec<Expr>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let seq = p.gather(star_target, comma)?;
        p.expect(T::Comma);
        Some(seq)
    });
    p.exit_rule();
    res
}

pub(crate) fn star_targets_tuple_seq(p: &mut Parser) -> Option<Vec<Expr>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[star_targets_tuple_seq_many, star_targets_tuple_seq_one]);
    p.exit_rule();
    res
}

fn star_targets_tuple_seq_many(p: &mut Parser) -> Option<Vec<Expr>> {
    let a = star_target(p)?;
    let rest = p.repeat1(star_targets_item)?;
    p.expect(T::Comma);
    Some(seq_insert_in_front(a, Some(rest)))
}

fn star_targets_tuple_seq_one(p: &mut Parser) -> Option<Vec<Expr>> {
    let a = star_target(p)?;
    p.expect_opt(T::Comma)?;
    Some(singleton_seq(a))
}

pub(crate) fn star_target(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.star_target) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[star_target_star, star_target_atom]);
    p.memo_insert(|t| &mut t.star_target, m, res.clone());
    p.exit_rule();
    res
}

fn star_target_star(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    if !p.not_lookahead(T::Star) {
        return None;
    }
    let a = star_target(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Starred {
        value: Box::new(set_expr_context(&a, ExprContext::Store)),
        ctx: Box::new(ExprContext::Store),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn star_target_atom(p: &mut Parser) -> Option<Expr> {
    target_with_star_atom(p)
}

pub(crate) fn target_with_star_atom(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.target_with_star_atom) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[
        target_with_star_atom_attr,
        target_with_star_atom_subscript,
        target_with_star_atom_atom,
    ]);
    p.memo_insert(|t| &mut t.target_with_star_atom, m, res.clone());
    p.exit_rule();
    res
}

fn attribute(value: Expr, attr: Sym, ctx: ExprContext, pos: super::Pos) -> Expr {
    Expr::Attribute {
        value: Box::new(value),
        attr,
        ctx: Box::new(ctx),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn subscript(value: Expr, slice: Expr, ctx: ExprContext, pos: super::Pos) -> Expr {
    Expr::Subscript {
        value: Box::new(value),
        slice: Box::new(slice),
        ctx: Box::new(ctx),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn t_primary_name(p: &mut Parser) -> Option<Sym> {
    let b = p.name_token()?;
    let Expr::Name { id, .. } = b else {
        return None;
    };
    Some(id)
}

fn target_with_star_atom_attr(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Dot)?;
    let b = t_primary_name(p)?;
    if !p.not_lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(attribute(a, b, ExprContext::Store, pos))
}

fn target_with_star_atom_subscript(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Lsqb)?;
    let b = slices(p)?;
    p.expect_opt(T::Rsqb)?;
    if !p.not_lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(subscript(a, b, ExprContext::Store, pos))
}

fn target_with_star_atom_atom(p: &mut Parser) -> Option<Expr> {
    star_atom(p)
}

pub(crate) fn star_atom(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        star_atom_name,
        star_atom_paren_target,
        star_atom_paren_tuple,
        star_atom_list,
    ]);
    p.exit_rule();
    res
}

fn star_atom_name(p: &mut Parser) -> Option<Expr> {
    let a = p.name_token()?;
    Some(set_expr_context(&a, ExprContext::Store))
}

fn star_atom_paren_target(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lpar)?;
    let a = target_with_star_atom(p)?;
    p.expect_opt(T::Rpar)?;
    Some(set_expr_context(&a, ExprContext::Store))
}

fn star_atom_paren_tuple(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lpar)?;
    let a = p.attempt(star_targets_tuple_seq);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_ctx(a.unwrap_or_default(), ExprContext::Store, pos))
}

fn star_atom_list(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lsqb)?;
    let a = p.attempt(star_targets_list_seq);
    p.expect_opt(T::Rsqb)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::List {
        elts: a.unwrap_or_default(),
        ctx: Box::new(ExprContext::Store),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn single_target(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        single_target_subscript_attribute,
        single_target_name,
        single_target_paren,
    ]);
    p.exit_rule();
    res
}

fn single_target_subscript_attribute(p: &mut Parser) -> Option<Expr> {
    single_subscript_attribute_target(p)
}

fn single_target_name(p: &mut Parser) -> Option<Expr> {
    let a = p.name_token()?;
    Some(set_expr_context(&a, ExprContext::Store))
}

fn single_target_paren(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lpar)?;
    let a = single_target(p)?;
    p.expect_opt(T::Rpar)?;
    Some(a)
}

pub(crate) fn single_subscript_attribute_target(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        single_subscript_attribute_target_attr,
        single_subscript_attribute_target_subscript,
    ]);
    p.exit_rule();
    res
}

fn single_subscript_attribute_target_attr(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Dot)?;
    let b = t_primary_name(p)?;
    if !p.not_lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(attribute(a, b, ExprContext::Store, pos))
}

fn single_subscript_attribute_target_subscript(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Lsqb)?;
    let b = slices(p)?;
    p.expect_opt(T::Rsqb)?;
    if !p.not_lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(subscript(a, b, ExprContext::Store, pos))
}

pub(crate) fn t_primary(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.t_primary, t_primary_raw)
}

fn t_primary_raw(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        t_primary_attr,
        t_primary_subscript,
        t_primary_genexp,
        t_primary_call,
        t_primary_atom,
    ]);
    p.exit_rule();
    res
}

fn t_primary_attr(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Dot)?;
    let b = t_primary_name(p)?;
    if !p.lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(attribute(a, b, ExprContext::Load, pos))
}

fn t_primary_subscript(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Lsqb)?;
    let b = slices(p)?;
    p.expect_opt(T::Rsqb)?;
    if !p.lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(subscript(a, b, ExprContext::Load, pos))
}

fn t_primary_genexp(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    let b = genexp(p)?;
    if !p.lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::Call {
        func: Box::new(a),
        args: singleton_seq(b),
        keywords: Vec::new(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn t_primary_call(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Lpar)?;
    let b = p.attempt(arguments);
    p.expect_opt(T::Rpar)?;
    if !p.lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    match b {
        Some(call) => call_from_arguments(a, call, pos),
        None => Some(Expr::Call {
            func: Box::new(a),
            args: Vec::new(),
            keywords: Vec::new(),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        }),
    }
}

fn t_primary_atom(p: &mut Parser) -> Option<Expr> {
    let a = atom(p)?;
    if p.lookahead_rule(t_lookahead) {
        Some(a)
    } else {
        None
    }
}

fn t_lookahead(p: &mut Parser) -> Option<()> {
    if p.lookahead(T::Lpar) || p.lookahead(T::Lsqb) || p.lookahead(T::Dot) {
        Some(())
    } else {
        None
    }
}

// -- del targets ---------------------------------------------------------------------

pub(crate) fn del_targets(p: &mut Parser) -> Option<Vec<Expr>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let seq = p.gather(del_target, comma)?;
        p.expect(T::Comma);
        Some(seq)
    });
    p.exit_rule();
    res
}

pub(crate) fn del_target(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.del_target) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[del_target_attr, del_target_subscript, del_t_atom]);
    p.memo_insert(|t| &mut t.del_target, m, res.clone());
    p.exit_rule();
    res
}

fn del_target_attr(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Dot)?;
    let b = t_primary_name(p)?;
    if !p.not_lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(attribute(a, b, ExprContext::Del, pos))
}

fn del_target_subscript(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let a = t_primary(p)?;
    p.expect_opt(T::Lsqb)?;
    let b = slices(p)?;
    p.expect_opt(T::Rsqb)?;
    if !p.not_lookahead_rule(t_lookahead) {
        return None;
    }
    let pos = p.extra(p.pos_at(m))?;
    Some(subscript(a, b, ExprContext::Del, pos))
}

fn del_t_atom(p: &mut Parser) -> Option<Expr> {
    del_t_atom_rule(p)
}

pub(crate) fn del_t_atom_rule(p: &mut Parser) -> Option<Expr> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        del_t_atom_name,
        del_t_atom_paren_target,
        del_t_atom_paren_targets,
        del_t_atom_list,
    ]);
    p.exit_rule();
    res
}

fn del_t_atom_name(p: &mut Parser) -> Option<Expr> {
    let a = p.name_token()?;
    Some(set_expr_context(&a, ExprContext::Del))
}

fn del_t_atom_paren_target(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lpar)?;
    let a = del_target(p)?;
    p.expect_opt(T::Rpar)?;
    Some(set_expr_context(&a, ExprContext::Del))
}

fn del_t_atom_paren_targets(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lpar)?;
    let a = p.attempt(del_targets);
    p.expect_opt(T::Rpar)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(tuple_ctx(a.unwrap_or_default(), ExprContext::Del, pos))
}

fn del_t_atom_list(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    p.expect_opt(T::Lsqb)?;
    let a = p.attempt(del_targets);
    p.expect_opt(T::Rsqb)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Expr::List {
        elts: a.unwrap_or_default(),
        ctx: Box::new(ExprContext::Del),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

// -- function type comments --------------------------------------------------------------

pub(crate) fn func_type_comment(p: &mut Parser) -> Option<()> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        func_type_comment_indented,
        func_type_comment_invalid,
        func_type_comment_plain,
    ]);
    p.exit_rule();
    res
}

fn func_type_comment_indented(p: &mut Parser) -> Option<()> {
    p.expect_opt(T::Newline)?;
    p.expect_opt(T::TypeComment)?;
    let m = p.mark;
    let indented = p.expect(T::Newline) && p.expect(T::Indent);
    p.mark = m;
    indented.then_some(())
}

fn func_type_comment_plain(p: &mut Parser) -> Option<()> {
    p.expect(T::TypeComment).then_some(())
}

fn func_type_comment_invalid(p: &mut Parser) -> Option<()> {
    if p.call_invalid_rules {
        invalid::double_type_comments(p);
    }
    None
}

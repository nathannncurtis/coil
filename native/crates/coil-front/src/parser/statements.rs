//! The grammar's statement rules, from `file` down through compound
//! statements, function parameters and type parameters.

use super::expressions;
use super::helpers::*;
use super::invalid;
use super::patterns;
use super::{Parser, Pos};
use crate::ast_generated::Mod;
use crate::ast_generated::{
    Alias, Arg, Expr, ExprContext, MatchCase, Operator, Stmt, TypeIgnore, TypeParam, Withitem,
};
use crate::intern::Sym;
use crate::tokenizer::TokenKind as T;

fn expr_stmt(value: Expr, pos: Pos) -> Stmt {
    Stmt::Expr {
        value: Box::new(value),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn ends(pos: &Pos) -> (i32, i32) {
    (
        pos.end_lineno.unwrap_or(pos.lineno),
        pos.end_col_offset.unwrap_or(pos.col_offset),
    )
}

// -- starting rules -------------------------------------------------------------

pub(crate) fn file_rule(p: &mut Parser) -> Option<Mod> {
    if !p.enter_rule() {
        return None;
    }
    let body = p.attempt(statements).unwrap_or_default();
    let res = p.expect(T::Endmarker).then(|| {
        // `_PyPegen_make_module`: the '# type: ignore' comments the token
        // reader collected become the module's type_ignores.
        let comments = std::mem::take(&mut p.type_ignore_comments);
        let type_ignores = comments
            .iter()
            .map(|(lineno, tag)| TypeIgnore::TypeIgnore {
                lineno: *lineno as i32,
                tag: p.interner.intern(&String::from_utf8_lossy(tag)),
            })
            .collect();
        Mod::Module { body, type_ignores }
    });
    p.exit_rule();
    res
}

pub(crate) fn statements(p: &mut Parser) -> Option<Vec<Stmt>> {
    if !p.enter_rule() {
        return None;
    }
    let seq = p.repeat1(statement)?;
    p.exit_rule();
    Some(seq.into_iter().flatten().collect())
}

fn statement(p: &mut Parser) -> Option<Vec<Stmt>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[statement_compound, statement_simple]);
    p.exit_rule();
    res
}

fn statement_compound(p: &mut Parser) -> Option<Vec<Stmt>> {
    let a = compound_stmt(p)?;
    Some(vec![a])
}

fn statement_simple(p: &mut Parser) -> Option<Vec<Stmt>> {
    simple_stmts(p)
}

fn simple_stmts(p: &mut Parser) -> Option<Vec<Stmt>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[simple_stmts_single, simple_stmts_semi]);
    p.exit_rule();
    res
}

fn simple_stmts_single(p: &mut Parser) -> Option<Vec<Stmt>> {
    let a = simple_stmt(p)?;
    if !p.not_lookahead(T::Semi) {
        return None;
    }
    p.expect_opt(T::Newline)?;
    Some(vec![a])
}

fn simple_stmts_semi(p: &mut Parser) -> Option<Vec<Stmt>> {
    let a = p.gather(simple_stmt, |p| p.expect(T::Semi))?;
    p.expect(T::Semi);
    p.expect_opt(T::Newline)?;
    Some(a)
}

pub(crate) fn simple_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.simple_stmt) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[
        assignment,
        simple_stmt_type_alias,
        simple_stmt_expr,
        simple_stmt_return,
        simple_stmt_import,
        simple_stmt_raise,
        simple_stmt_pass,
        simple_stmt_del,
        simple_stmt_yield,
        simple_stmt_assert,
        simple_stmt_break,
        simple_stmt_continue,
        simple_stmt_global,
        simple_stmt_nonlocal,
    ]);
    p.memo_insert(|t| &mut t.simple_stmt, m, res.clone());
    p.exit_rule();
    res
}

fn simple_stmt_type_alias(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_soft_keyword("type") {
        return None;
    }
    type_alias(p)
}

fn simple_stmt_expr(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    let e = expressions::star_expressions(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(expr_stmt(e, pos))
}

fn simple_stmt_return(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("return") {
        return None;
    }
    return_stmt(p)
}

fn simple_stmt_import(p: &mut Parser) -> Option<Stmt> {
    if !(p.lookahead_keyword("import") || p.lookahead_keyword("from")) {
        return None;
    }
    import_stmt(p)
}

fn simple_stmt_raise(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("raise") {
        return None;
    }
    raise_stmt(p)
}

fn simple_stmt_pass(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("pass")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Pass {
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn simple_stmt_del(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("del") {
        return None;
    }
    del_stmt(p)
}

fn simple_stmt_yield(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("yield") {
        return None;
    }
    yield_stmt(p)
}

fn simple_stmt_assert(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("assert") {
        return None;
    }
    assert_stmt(p)
}

fn simple_stmt_break(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("break")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Break {
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn simple_stmt_continue(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("continue")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Continue {
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn simple_stmt_global(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("global") {
        return None;
    }
    global_stmt(p)
}

fn simple_stmt_nonlocal(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("nonlocal") {
        return None;
    }
    nonlocal_stmt(p)
}

fn compound_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        compound_stmt_function_def,
        compound_stmt_if,
        compound_stmt_class_def,
        compound_stmt_with,
        compound_stmt_for,
        compound_stmt_try,
        compound_stmt_while,
        match_stmt,
    ]);
    p.exit_rule();
    res
}

fn compound_stmt_function_def(p: &mut Parser) -> Option<Stmt> {
    if !(p.lookahead_keyword("def") || p.lookahead(T::At) || p.lookahead(T::Async)) {
        return None;
    }
    function_def(p)
}

fn compound_stmt_if(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("if") {
        return None;
    }
    if_stmt(p)
}

fn compound_stmt_class_def(p: &mut Parser) -> Option<Stmt> {
    if !(p.lookahead_keyword("class") || p.lookahead(T::At)) {
        return None;
    }
    class_def(p)
}

fn compound_stmt_with(p: &mut Parser) -> Option<Stmt> {
    if !(p.lookahead_keyword("with") || p.lookahead(T::Async)) {
        return None;
    }
    with_stmt(p)
}

fn compound_stmt_for(p: &mut Parser) -> Option<Stmt> {
    if !(p.lookahead_keyword("for") || p.lookahead(T::Async)) {
        return None;
    }
    for_stmt(p)
}

fn compound_stmt_try(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("try") {
        return None;
    }
    try_stmt(p)
}

fn compound_stmt_while(p: &mut Parser) -> Option<Stmt> {
    if !p.lookahead_keyword("while") {
        return None;
    }
    while_stmt(p)
}

// -- simple statements ----------------------------------------------------------

pub(crate) fn assignment(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = p.alternatives(&[assignment_ann_name, assignment_ann_other, assignment_plain]);
    // The augmented-assignment alternative cuts after its operator: a failed
    // right side aborts the rule, skipping `invalid_assignment`.
    let mut cut = false;
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            let m = p.mark;
            let a = single_target(p)?;
            let b = augassign(p)?;
            cut = true;
            let c = annotated_rhs(p)?;
            let pos = p.extra(p.pos_at(m))?;
            Some(Stmt::AugAssign {
                target: Box::new(a),
                op: Box::new(b),
                value: Box::new(c),
                lineno: pos.lineno,
                col_offset: pos.col_offset,
                end_lineno: pos.end_lineno,
                end_col_offset: pos.end_col_offset,
            })
        });
    }
    if res.is_none() && !cut && !p.has_error() && p.call_invalid_rules {
        invalid::assignment(p);
    }
    p.exit_rule();
    res
}

fn assignment_ann_name(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    let a = p.name_token()?;
    p.expect_opt(T::Colon)?;
    let b = expressions::expression(p)?;
    let c = p.attempt(annotated_rhs_assign);
    let pos = p.extra(p.pos_at(m))?;
    let target = set_expr_context(&a, ExprContext::Store);
    Some(Stmt::AnnAssign {
        target: Box::new(target),
        annotation: Box::new(b),
        value: c.map(Box::new),
        simple: 1,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn annotated_rhs_assign(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Equal)?;
    annotated_rhs(p)
}

fn assignment_ann_other(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    let a = p.alternatives(&[ann_other_paren, single_subscript_attribute_target])?;
    p.expect_opt(T::Colon)?;
    let b = expressions::expression(p)?;
    let c = p.attempt(annotated_rhs_assign);
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::AnnAssign {
        target: Box::new(a),
        annotation: Box::new(b),
        value: c.map(Box::new),
        simple: 0,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn ann_other_paren(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Lpar)?;
    let b = single_target(p)?;
    p.expect_opt(T::Rpar)?;
    Some(b)
}

fn assignment_plain(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    let mut targets = Vec::new();
    loop {
        let z = p.attempt(|p| {
            let z = star_targets(p)?;
            p.expect(T::Equal).then_some(z)
        });
        match z {
            Some(target) => targets.push(target),
            None => break,
        }
    }
    if targets.is_empty() {
        return None;
    }
    let b = p.alternatives(&[expressions::yield_expr, expressions::star_expressions])?;
    if !p.not_lookahead(T::Equal) {
        return None;
    }
    // NEW_TYPE_COMMENT yields nothing with the tokenizer's type-comment mode
    // off, the mode ast.parse runs in.
    p.attempt(|p| p.expect_opt(T::TypeComment));
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Assign {
        targets,
        value: Box::new(b),
        type_comment: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn annotated_rhs(p: &mut Parser) -> Option<Expr> {
    p.alternatives(&[expressions::yield_expr, expressions::star_expressions])
}

pub(crate) fn augassign(p: &mut Parser) -> Option<Operator> {
    if !p.enter_rule() {
        return None;
    }
    let ops = [
        (T::Plusequal, Operator::Add),
        (T::Minequal, Operator::Sub),
        (T::Starequal, Operator::Mult),
        (T::Atequal, Operator::MatMult),
        (T::Slashequal, Operator::Div),
        (T::Percentequal, Operator::Mod),
        (T::Amperequal, Operator::BitAnd),
        (T::Vbarequal, Operator::BitOr),
        (T::Circumflexequal, Operator::BitXor),
        (T::Leftshiftequal, Operator::LShift),
        (T::Rightshiftequal, Operator::RShift),
        (T::Doublestarequal, Operator::Pow),
        (T::Doubleslashequal, Operator::FloorDiv),
    ];
    let mut res = None;
    for (kind, op) in ops {
        if p.expect(kind) {
            res = Some(op);
            break;
        }
    }
    p.exit_rule();
    res
}

fn return_stmt(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("return")?;
    let a = p.attempt(expressions::star_expressions);
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Return {
        value: a.map(Box::new),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn raise_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[raise_stmt_from, raise_stmt_bare]);
    p.exit_rule();
    res
}

fn raise_stmt_from(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("raise")?;
    let a = expressions::expression(p)?;
    let b = p.attempt(|p| {
        p.expect_keyword_opt("from")?;
        expressions::expression(p)
    });
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Raise {
        exc: Some(Box::new(a)),
        cause: b.map(Box::new),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn raise_stmt_bare(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("raise")?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Raise {
        exc: None,
        cause: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn global_stmt(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("global")?;
    let a = p.gather(global_name, |p| p.expect(T::Comma))?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Global {
        names: map_names_to_ids(&a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn global_name(p: &mut Parser) -> Option<Expr> {
    p.name_token()
}

fn nonlocal_stmt(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("nonlocal")?;
    let a = p.gather(global_name, |p| p.expect(T::Comma))?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Nonlocal {
        names: map_names_to_ids(&a),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn del_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let m = p.mark;
        p.expect_keyword_opt("del")?;
        let a = expressions::del_targets(p)?;
        if !(p.lookahead(T::Semi) || p.lookahead(T::Newline)) {
            return None;
        }
        let pos = p.extra(p.pos_at(m))?;
        Some(Stmt::Delete {
            targets: a,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::del_stmt(p);
    }
    p.exit_rule();
    res
}

fn yield_stmt(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    let y = expressions::yield_expr(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(expr_stmt(y, pos))
}

fn assert_stmt(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("assert")?;
    let a = expressions::expression(p)?;
    let b = p.attempt(|p| {
        p.expect_opt(T::Comma)?;
        expressions::expression(p)
    });
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Assert {
        test: Box::new(a),
        msg: b.map(Box::new),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

// -- imports --------------------------------------------------------------------

fn import_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::import(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[import_name, import_from]);
    }
    p.exit_rule();
    res
}

fn import_name(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("import")?;
    let a = dotted_as_names(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Import {
        names: a,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn import_from(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[import_from_module, import_from_only_dots]);
    p.exit_rule();
    res
}

fn import_from_dots(p: &mut Parser) -> Option<Vec<u8>> {
    let mut seq = Vec::new();
    loop {
        if p.expect(T::Dot) {
            seq.push(b'.');
        } else if p.expect(T::Ellipsis) {
            seq.extend_from_slice(b"...");
        } else {
            break;
        }
    }
    Some(seq)
}

fn import_from_dots_plus(p: &mut Parser) -> Option<Vec<u8>> {
    let seq = import_from_dots(p)?;
    (!seq.is_empty()).then_some(seq)
}

fn import_from_module(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("from")?;
    let a = import_from_dots(p)?;
    let b = dotted_name(p)?;
    let Expr::Name { id: module, .. } = &b else {
        return None;
    };
    let module = *module;
    p.expect_keyword_opt("import")?;
    let c = import_from_targets(p)?;
    let pos = p.extra(p.pos_at(m))?;
    p.checked_future_import(&module, &c, seq_count_dots(&a), pos)
}

fn import_from_only_dots(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("from")?;
    let a = import_from_dots_plus(p)?;
    p.expect_keyword_opt("import")?;
    let b = import_from_targets(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::ImportFrom {
        module: None,
        names: b,
        level: Some(seq_count_dots(&a)),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn import_from_targets(p: &mut Parser) -> Option<Vec<Alias>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        import_from_targets_paren,
        import_from_targets_plain,
        import_from_targets_star,
    ]);
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::import_from_targets(p);
    }
    p.exit_rule();
    res
}

fn import_from_targets_paren(p: &mut Parser) -> Option<Vec<Alias>> {
    p.expect_opt(T::Lpar)?;
    let a = import_from_as_names(p)?;
    p.expect(T::Comma);
    p.expect_opt(T::Rpar)?;
    Some(a)
}

fn import_from_targets_plain(p: &mut Parser) -> Option<Vec<Alias>> {
    let a = import_from_as_names(p)?;
    if !p.not_lookahead(T::Comma) {
        return None;
    }
    Some(a)
}

fn import_from_targets_star(p: &mut Parser) -> Option<Vec<Alias>> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(vec![p.alias_for_star(pos)])
}

pub(crate) fn import_from_as_names(p: &mut Parser) -> Option<Vec<Alias>> {
    p.gather(import_from_as_name, |p| p.expect(T::Comma))
}

fn import_from_as_name(p: &mut Parser) -> Option<Alias> {
    let m = p.mark;
    let a = p.name_token()?;
    let b = p.attempt(|p| {
        p.expect_keyword_opt("as")?;
        p.name_token()
    });
    let pos = p.extra(p.pos_at(m))?;
    let Expr::Name { id, .. } = a else {
        return None;
    };
    Some(Alias {
        name: id,
        asname: b.and_then(|e| name_id(&e)),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn dotted_as_names(p: &mut Parser) -> Option<Vec<Alias>> {
    p.gather(dotted_as_name, |p| p.expect(T::Comma))
}

fn dotted_as_name(p: &mut Parser) -> Option<Alias> {
    let m = p.mark;
    let a = dotted_name(p)?;
    let b = p.attempt(|p| {
        p.expect_keyword_opt("as")?;
        p.name_token()
    });
    let pos = p.extra(p.pos_at(m))?;
    let Expr::Name { id, .. } = a else {
        return None;
    };
    Some(Alias {
        name: id,
        asname: b.and_then(|e| name_id(&e)),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn dotted_name(p: &mut Parser) -> Option<Expr> {
    p.left_recursion(|t| &mut t.dotted_name, dotted_name_raw)
}

fn dotted_name_raw(p: &mut Parser) -> Option<Expr> {
    p.alternatives(&[dotted_name_dotted, dotted_name_plain])
}

fn dotted_name_dotted(p: &mut Parser) -> Option<Expr> {
    let a = dotted_name(p)?;
    p.expect_opt(T::Dot)?;
    let b = p.name_token()?;
    join_names_with_dot(p, &a, &b)
}

fn dotted_name_plain(p: &mut Parser) -> Option<Expr> {
    p.name_token()
}

fn name_id(e: &Expr) -> Option<Sym> {
    let Expr::Name { id, .. } = e else {
        return None;
    };
    Some(*id)
}

// -- common elements ------------------------------------------------------------

pub(crate) fn block(p: &mut Parser) -> Option<Vec<Stmt>> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.block) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[block_indented, simple_stmts]);
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::block(p);
    }
    p.memo_insert(|t| &mut t.block, m, res.clone());
    p.exit_rule();
    res
}

fn block_indented(p: &mut Parser) -> Option<Vec<Stmt>> {
    p.expect_opt(T::Newline)?;
    p.expect_opt(T::Indent)?;
    let a = statements(p)?;
    p.expect_opt(T::Dedent)?;
    Some(a)
}

fn decorators(p: &mut Parser) -> Option<Vec<Expr>> {
    let mut out = Vec::new();
    loop {
        let f = p.attempt(|p| {
            p.expect_opt(T::At)?;
            let f = expressions::named_expression(p)?;
            p.expect_opt(T::Newline)?;
            Some(f)
        });
        match f {
            Some(f) => out.push(f),
            None => break,
        }
    }
    (!out.is_empty()).then_some(out)
}

// -- class and function definitions ---------------------------------------------

fn class_def(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[class_def_decorated, class_def_raw]);
    p.exit_rule();
    res
}

fn class_def_decorated(p: &mut Parser) -> Option<Stmt> {
    let a = decorators(p)?;
    let b = class_def_raw(p)?;
    Some(Parser::class_def_decorators(a, b))
}

pub(crate) fn class_def_raw(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::class_def_raw(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            let m = p.mark;
            p.expect_keyword_opt("class")?;
            let a = p.name_token()?;
            let t = p.attempt(type_params);
            let b = p.attempt(|p| {
                p.expect_opt(T::Lpar)?;
                let z = p.attempt(expressions::arguments);
                p.expect_opt(T::Rpar)?;
                Some(z)
            });
            p.expect_opt(T::Colon)?;
            let c = block(p)?;
            let pos = p.extra(p.pos_at(m))?;
            let (bases, keywords) = match b.flatten() {
                Some(Expr::Call { args, keywords, .. }) => (args, keywords),
                _ => (Vec::new(), Vec::new()),
            };
            Some(Stmt::ClassDef {
                name: name_id(&a)?,
                bases,
                keywords,
                body: c,
                decorator_list: Vec::new(),
                type_params: t.unwrap_or_default(),
                lineno: pos.lineno,
                col_offset: pos.col_offset,
                end_lineno: pos.end_lineno,
                end_col_offset: pos.end_col_offset,
            })
        });
    }
    p.exit_rule();
    res
}

fn function_def(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[function_def_decorated, function_def_raw]);
    p.exit_rule();
    res
}

fn function_def_decorated(p: &mut Parser) -> Option<Stmt> {
    let d = decorators(p)?;
    let f = function_def_raw(p)?;
    Some(Parser::function_def_decorators(d, f))
}

fn function_def_raw(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::def_raw(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[function_def_raw_def, function_def_raw_async]);
    }
    p.exit_rule();
    res
}

fn function_def_raw_def(p: &mut Parser) -> Option<Stmt> {
    function_def_raw_common(p, false, p.mark)
}

fn function_def_raw_async(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_opt(T::Async)?;
    function_def_raw_common(p, true, m)
}

fn function_def_raw_common(p: &mut Parser, is_async: bool, start: usize) -> Option<Stmt> {
    let m = start;
    p.expect_keyword_opt("def")?;
    let n = p.name_token()?;
    let t = p.attempt(type_params);
    if !p.expect_forced(T::Lpar, "(") {
        return None;
    }
    let raw_params = p.attempt(params);
    p.expect_opt(T::Rpar)?;
    let a = p.attempt(|p| {
        p.expect_opt(T::Rarrow)?;
        expressions::expression(p)
    });
    if !p.expect_forced(T::Colon, ":") {
        return None;
    }
    // NEW_TYPE_COMMENT yields nothing with the tokenizer's type-comment mode
    // off, the mode ast.parse runs in.
    p.attempt(expressions::func_type_comment);
    let b = block(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let args = raw_params.unwrap_or_else(Parser::empty_arguments);
    let fields = (
        name_id(&n)?,
        Box::new(args),
        b,
        a.map(Box::new),
        t.unwrap_or_default(),
        pos,
    );
    let (name, args, body, returns, type_params, pos) = fields;
    if is_async {
        Some(Stmt::AsyncFunctionDef {
            name,
            args,
            body,
            decorator_list: Vec::new(),
            returns,
            type_comment: None,
            type_params,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    } else {
        Some(Stmt::FunctionDef {
            name,
            args,
            body,
            decorator_list: Vec::new(),
            returns,
            type_comment: None,
            type_params,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    }
}

// -- function parameters --------------------------------------------------------

pub(crate) fn params(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::parameters(p);
            None
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = parameters(p);
    }
    p.exit_rule();
    res
}

fn parameters(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        parameters_slash_no_default,
        parameters_slash_with_default,
        parameters_plain,
        parameters_with_default,
        parameters_star_etc,
    ]);
    p.exit_rule();
    res
}

fn parameters_slash_no_default(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = slash_no_default(p)?;
    let b = p.repeat0(param_no_default);
    let c = p.repeat0(param_with_default);
    let d = p.attempt(star_etc);
    Some(Parser::make_arguments(Some(a), None, Some(b), Some(c), d))
}

fn parameters_slash_with_default(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = slash_with_default(p)?;
    let b = p.repeat0(param_with_default);
    let c = p.attempt(star_etc);
    Some(Parser::make_arguments(None, Some(a), None, Some(b), c))
}

fn parameters_plain(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = p.repeat1(param_no_default)?;
    let b = p.repeat0(param_with_default);
    let c = p.attempt(star_etc);
    Some(Parser::make_arguments(None, None, Some(a), Some(b), c))
}

fn parameters_with_default(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = p.repeat1(param_with_default)?;
    let b = p.attempt(star_etc);
    Some(Parser::make_arguments(None, None, None, Some(a), b))
}

fn parameters_star_etc(p: &mut Parser) -> Option<crate::ast_generated::Arguments> {
    let a = star_etc(p)?;
    Some(Parser::make_arguments(None, None, None, None, Some(a)))
}

pub(crate) fn slash_no_default(p: &mut Parser) -> Option<Vec<Arg>> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[slash_no_default_comma, slash_no_default_end]);
    p.exit_rule();
    res
}

fn slash_no_default_comma(p: &mut Parser) -> Option<Vec<Arg>> {
    let a = p.repeat1(param_no_default)?;
    p.expect_opt(T::Slash)?;
    p.expect_opt(T::Comma)?;
    Some(a)
}

fn slash_no_default_end(p: &mut Parser) -> Option<Vec<Arg>> {
    let a = p.repeat1(param_no_default)?;
    p.expect_opt(T::Slash)?;
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some(a)
}

pub(crate) fn slash_with_default(p: &mut Parser) -> Option<SlashWithDefault> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[slash_with_default_comma, slash_with_default_end]);
    p.exit_rule();
    res
}

fn slash_with_default_comma(p: &mut Parser) -> Option<SlashWithDefault> {
    let a = p.repeat0(param_no_default);
    let b = p.repeat1(param_with_default)?;
    p.expect_opt(T::Slash)?;
    p.expect_opt(T::Comma)?;
    Some((a, b))
}

fn slash_with_default_end(p: &mut Parser) -> Option<SlashWithDefault> {
    let a = p.repeat0(param_no_default);
    let b = p.repeat1(param_with_default)?;
    p.expect_opt(T::Slash)?;
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some((a, b))
}

fn star_etc(p: &mut Parser) -> Option<StarEtc> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::star_etc(p);
            None
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[
            star_etc_named,
            star_etc_star_annotation,
            star_etc_bare,
            star_etc_kwds,
        ]);
    }
    p.exit_rule();
    res
}

fn star_etc_named(p: &mut Parser) -> Option<StarEtc> {
    p.expect_opt(T::Star)?;
    let a = param_no_default(p)?;
    let b = p.repeat0(param_maybe_default);
    let c = p.attempt(kwds);
    Some((Some(a), b, c))
}

fn star_etc_star_annotation(p: &mut Parser) -> Option<StarEtc> {
    p.expect_opt(T::Star)?;
    let a = param_no_default_star_annotation(p)?;
    let b = p.repeat0(param_maybe_default);
    let c = p.attempt(kwds);
    Some((Some(a), b, c))
}

fn star_etc_bare(p: &mut Parser) -> Option<StarEtc> {
    p.expect_opt(T::Star)?;
    p.expect_opt(T::Comma)?;
    let b = p.repeat1(param_maybe_default)?;
    let c = p.attempt(kwds);
    Some((None, b, c))
}

fn star_etc_kwds(p: &mut Parser) -> Option<StarEtc> {
    let a = kwds(p)?;
    Some((None, Vec::new(), Some(a)))
}

fn kwds(p: &mut Parser) -> Option<Arg> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::kwds(p);
            None
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            p.expect_opt(T::Doublestar)?;
            param_no_default(p)
        });
    }
    p.exit_rule();
    res
}

pub(crate) fn param_no_default(p: &mut Parser) -> Option<Arg> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[param_no_default_comma, param_no_default_end]);
    p.exit_rule();
    res
}

fn param_no_default_comma(p: &mut Parser) -> Option<Arg> {
    let a = param(p)?;
    p.expect_opt(T::Comma)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    Some(a)
}

fn param_no_default_end(p: &mut Parser) -> Option<Arg> {
    let a = param(p)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some(a)
}

fn param_no_default_star_annotation(p: &mut Parser) -> Option<Arg> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        param_no_default_star_annotation_comma,
        param_no_default_star_annotation_end,
    ]);
    p.exit_rule();
    res
}

fn param_no_default_star_annotation_comma(p: &mut Parser) -> Option<Arg> {
    let a = param_star_annotation(p)?;
    p.expect_opt(T::Comma)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    Some(a)
}

fn param_no_default_star_annotation_end(p: &mut Parser) -> Option<Arg> {
    let a = param_star_annotation(p)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some(a)
}

pub(crate) fn param_with_default(p: &mut Parser) -> Option<NameDefaultPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[param_with_default_comma, param_with_default_end]);
    p.exit_rule();
    res
}

fn param_with_default_comma(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = param(p)?;
    let c = expressions::default(p)?;
    p.expect_opt(T::Comma)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    Some((a, Some(c)))
}

fn param_with_default_end(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = param(p)?;
    let c = expressions::default(p)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some((a, Some(c)))
}

pub(crate) fn param_maybe_default(p: &mut Parser) -> Option<NameDefaultPair> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[param_maybe_default_comma, param_maybe_default_end]);
    p.exit_rule();
    res
}

fn param_maybe_default_comma(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = param(p)?;
    let c = p.attempt(expressions::default);
    p.expect_opt(T::Comma)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    Some((a, c))
}

fn param_maybe_default_end(p: &mut Parser) -> Option<NameDefaultPair> {
    let a = param(p)?;
    let c = p.attempt(expressions::default);
    p.attempt(|p| p.expect_opt(T::TypeComment));
    if !p.lookahead(T::Rpar) {
        return None;
    }
    Some((a, c))
}

pub(crate) fn param(p: &mut Parser) -> Option<Arg> {
    let m = p.mark;
    let a = p.name_token()?;
    let b = p.attempt(annotation);
    let pos = p.extra(p.pos_at(m))?;
    Some(Arg {
        arg: name_id(&a)?,
        annotation: b.map(Box::new),
        type_comment: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn param_star_annotation(p: &mut Parser) -> Option<Arg> {
    let m = p.mark;
    let a = p.name_token()?;
    let b = star_annotation(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Arg {
        arg: name_id(&a)?,
        annotation: Some(Box::new(b)),
        type_comment: None,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn annotation(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Colon)?;
    expressions::expression(p)
}

fn star_annotation(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Colon)?;
    expressions::star_expression(p)
}

// -- conditional statements -----------------------------------------------------

fn if_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::if_stmt(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[if_stmt_elif, if_stmt_else]);
    }
    p.exit_rule();
    res
}

fn if_stmt_elif(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("if")?;
    let a = expressions::named_expression(p)?;
    p.expect_opt(T::Colon)?;
    let b = block(p)?;
    let c = elif_stmt(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::If {
        test: Box::new(a),
        body: b,
        orelse: vec![c],
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn if_stmt_else(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("if")?;
    let a = expressions::named_expression(p)?;
    p.expect_opt(T::Colon)?;
    let b = block(p)?;
    let c = p.attempt(else_block);
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::If {
        test: Box::new(a),
        body: b,
        orelse: c.unwrap_or_default(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn elif_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::elif_stmt(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[elif_stmt_elif, elif_stmt_else]);
    }
    p.exit_rule();
    res
}

fn elif_stmt_elif(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("elif")?;
    let a = expressions::named_expression(p)?;
    p.expect_opt(T::Colon)?;
    let b = block(p)?;
    let c = elif_stmt(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::If {
        test: Box::new(a),
        body: b,
        orelse: vec![c],
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn elif_stmt_else(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("elif")?;
    let a = expressions::named_expression(p)?;
    p.expect_opt(T::Colon)?;
    let b = block(p)?;
    let c = p.attempt(else_block);
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::If {
        test: Box::new(a),
        body: b,
        orelse: c.unwrap_or_default(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn else_block(p: &mut Parser) -> Option<Vec<Stmt>> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::else_stmt(p);
            None::<Vec<Stmt>>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            p.expect_keyword_opt("else")?;
            if !p.expect_forced(T::Colon, ":") {
                return None;
            }
            block(p)
        });
    }
    p.exit_rule();
    res
}

fn while_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::while_stmt(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            let m = p.mark;
            p.expect_keyword_opt("while")?;
            let a = expressions::named_expression(p)?;
            p.expect_opt(T::Colon)?;
            let b = block(p)?;
            let c = p.attempt(else_block);
            let pos = p.extra(p.pos_at(m))?;
            Some(Stmt::While {
                test: Box::new(a),
                body: b,
                orelse: c.unwrap_or_default(),
                lineno: pos.lineno,
                col_offset: pos.col_offset,
                end_lineno: pos.end_lineno,
                end_col_offset: pos.end_col_offset,
            })
        });
    }
    p.exit_rule();
    res
}

fn for_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::for_stmt(p);
            None::<Stmt>
        })
    } else {
        None
    };
    // Both loop forms cut after 'in': once the iterable has begun, a failed
    // body aborts the rule instead of falling through to the target check.
    let mut cut = false;
    if res.is_none() && !p.has_error() {
        res = for_stmt_sync(p, &mut cut, false);
    }
    if res.is_none() && !cut && !p.has_error() {
        res = for_stmt_sync(p, &mut cut, true);
    }
    if res.is_none() && !cut && !p.has_error() && p.call_invalid_rules {
        invalid::for_target(p);
    }
    p.exit_rule();
    res
}

fn for_stmt_sync(p: &mut Parser, cut: &mut bool, is_async: bool) -> Option<Stmt> {
    let m = p.mark;
    if is_async {
        p.expect_opt(T::Async)?;
    }
    p.expect_keyword_opt("for")?;
    let t = star_targets(p)?;
    p.expect_keyword_opt("in")?;
    *cut = true;
    let ex = expressions::star_expressions(p)?;
    p.expect_opt(T::Colon)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    let b = block(p)?;
    let el = p.attempt(else_block);
    let pos = p.extra(p.pos_at(m))?;
    let stmt = if is_async {
        Stmt::AsyncFor {
            target: Box::new(t),
            iter: Box::new(ex),
            body: b,
            orelse: el.unwrap_or_default(),
            type_comment: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        }
    } else {
        Stmt::For {
            target: Box::new(t),
            iter: Box::new(ex),
            body: b,
            orelse: el.unwrap_or_default(),
            type_comment: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        }
    };
    Some(stmt)
}

fn with_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.alternatives(&[
        with_stmt_indent_invalid,
        with_stmt_paren,
        with_stmt_plain,
        with_stmt_async_paren,
        with_stmt_async_plain,
        with_stmt_invalid,
    ]);
    p.exit_rule();
    res
}

fn with_stmt_indent_invalid(p: &mut Parser) -> Option<Stmt> {
    if p.call_invalid_rules {
        invalid::with_stmt_indent(p);
    }
    None
}

fn with_stmt_invalid(p: &mut Parser) -> Option<Stmt> {
    if p.call_invalid_rules {
        invalid::with_stmt(p);
    }
    None
}

fn with_stmt_paren(p: &mut Parser) -> Option<Stmt> {
    with_stmt_common(p, true, false)
}

fn with_stmt_plain(p: &mut Parser) -> Option<Stmt> {
    with_stmt_common(p, false, false)
}

fn with_stmt_async_paren(p: &mut Parser) -> Option<Stmt> {
    with_stmt_common(p, true, true)
}

fn with_stmt_async_plain(p: &mut Parser) -> Option<Stmt> {
    with_stmt_common(p, false, true)
}

fn with_stmt_common(p: &mut Parser, paren: bool, is_async: bool) -> Option<Stmt> {
    let m = p.mark;
    if is_async {
        p.expect_opt(T::Async)?;
    }
    p.expect_keyword_opt("with")?;
    let a = if paren {
        let inner = |p: &mut Parser| -> Option<Vec<Withitem>> {
            p.expect_opt(T::Lpar)?;
            let a = p.gather(with_item, |p| p.expect(T::Comma))?;
            p.expect(T::Comma);
            p.expect_opt(T::Rpar)?;
            Some(a)
        };
        inner(p)?
    } else {
        p.gather(with_item, |p| p.expect(T::Comma))?
    };
    p.expect_opt(T::Colon)?;
    p.attempt(|p| p.expect_opt(T::TypeComment));
    let b = block(p)?;
    let pos = p.extra(p.pos_at(m))?;
    let fields = (a, b, pos);
    let (items, body, pos) = fields;
    if is_async {
        Some(Stmt::AsyncWith {
            items,
            body,
            type_comment: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    } else {
        Some(Stmt::With {
            items,
            body,
            type_comment: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    }
}

pub(crate) fn with_item(p: &mut Parser) -> Option<Withitem> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = p.attempt(|p| {
        let e = expressions::expression(p)?;
        p.expect_keyword_opt("as")?;
        let t = star_target(p)?;
        if !(p.lookahead(T::Comma) || p.lookahead(T::Rpar) || p.lookahead(T::Colon)) {
            return None;
        }
        Some(Withitem {
            context_expr: Box::new(e),
            optional_vars: Some(Box::new(t)),
        })
    });
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::with_item(p);
    }
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            let e = expressions::expression(p)?;
            Some(Withitem {
                context_expr: Box::new(e),
                optional_vars: None,
            })
        });
    }
    p.exit_rule();
    res
}

fn try_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::try_stmt(p);
            None::<Stmt>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[try_stmt_finally, try_stmt_except, try_stmt_except_star]);
    }
    p.exit_rule();
    res
}

fn try_stmt_finally(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("try")?;
    if !p.expect_forced(T::Colon, ":") {
        return None;
    }
    let b = block(p)?;
    let f = finally_block(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Try {
        body: b,
        handlers: Vec::new(),
        orelse: Vec::new(),
        finalbody: f,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn try_stmt_except(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("try")?;
    if !p.expect_forced(T::Colon, ":") {
        return None;
    }
    let b = block(p)?;
    let ex = p.repeat1(except_block)?;
    let el = p.attempt(else_block);
    let f = p.attempt(finally_block);
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::Try {
        body: b,
        handlers: ex,
        orelse: el.unwrap_or_default(),
        finalbody: f.unwrap_or_default(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn try_stmt_except_star(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    p.expect_keyword_opt("try")?;
    if !p.expect_forced(T::Colon, ":") {
        return None;
    }
    let b = block(p)?;
    let ex = p.repeat1(except_star_block)?;
    let el = p.attempt(else_block);
    let f = p.attempt(finally_block);
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::TryStar {
        body: b,
        handlers: ex,
        orelse: el.unwrap_or_default(),
        finalbody: f.unwrap_or_default(),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn except_block(p: &mut Parser) -> Option<crate::ast_generated::Excepthandler> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::except_stmt_indent(p);
            None
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.alternatives(&[except_block_expr, except_block_plain]);
    }
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::except_stmt(p);
    }
    p.exit_rule();
    res
}

fn except_handler(
    e: Option<Expr>,
    name: Option<Sym>,
    b: Vec<Stmt>,
    pos: Pos,
) -> crate::ast_generated::Excepthandler {
    crate::ast_generated::Excepthandler::ExceptHandler {
        r#type: e.map(Box::new),
        name,
        body: b,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    }
}

fn except_block_expr(p: &mut Parser) -> Option<crate::ast_generated::Excepthandler> {
    let m = p.mark;
    p.expect_keyword_opt("except")?;
    let e = expressions::expression(p)?;
    let t = p.attempt(|p| {
        p.expect_keyword_opt("as")?;
        p.name_token()
    });
    p.expect_opt(T::Colon)?;
    let b = block(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(except_handler(Some(e), t.and_then(|t| name_id(&t)), b, pos))
}

fn except_block_plain(p: &mut Parser) -> Option<crate::ast_generated::Excepthandler> {
    let m = p.mark;
    p.expect_keyword_opt("except")?;
    p.expect_opt(T::Colon)?;
    let b = block(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(except_handler(None, None, b, pos))
}

pub(crate) fn except_star_block(p: &mut Parser) -> Option<crate::ast_generated::Excepthandler> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::except_star_stmt_indent(p);
            None
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            let m = p.mark;
            p.expect_keyword_opt("except")?;
            p.expect_opt(T::Star)?;
            let e = expressions::expression(p)?;
            let t = p.attempt(|p| {
                p.expect_keyword_opt("as")?;
                p.name_token()
            });
            p.expect_opt(T::Colon)?;
            let b = block(p)?;
            let pos = p.extra(p.pos_at(m))?;
            Some(except_handler(Some(e), t.and_then(|t| name_id(&t)), b, pos))
        });
    }
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::except_stmt(p);
    }
    p.exit_rule();
    res
}

fn finally_block(p: &mut Parser) -> Option<Vec<Stmt>> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::finally_stmt(p);
            None::<Vec<Stmt>>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            p.expect_keyword_opt("finally")?;
            if !p.expect_forced(T::Colon, ":") {
                return None;
            }
            block(p)
        });
    }
    p.exit_rule();
    res
}

// -- match statements -----------------------------------------------------------

fn match_stmt(p: &mut Parser) -> Option<Stmt> {
    if !p.enter_rule() {
        return None;
    }
    let res = p.attempt(|p| {
        let m = p.mark;
        if !p.expect_soft_keyword("match") {
            return None;
        }
        let subject = subject_expr(p)?;
        p.expect_opt(T::Colon)?;
        p.expect_opt(T::Newline)?;
        p.expect_opt(T::Indent)?;
        let cases = p.repeat1(case_block)?;
        p.expect_opt(T::Dedent)?;
        let pos = p.extra(p.pos_at(m))?;
        Some(Stmt::Match {
            subject: Box::new(subject),
            cases,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    });
    if res.is_none() && !p.has_error() && p.call_invalid_rules {
        invalid::match_stmt(p);
    }
    p.exit_rule();
    res
}

pub(crate) fn subject_expr(p: &mut Parser) -> Option<Expr> {
    p.alternatives(&[subject_expr_tuple, expressions::named_expression])
}

fn subject_expr_tuple(p: &mut Parser) -> Option<Expr> {
    let m = p.mark;
    let value = expressions::star_named_expression(p)?;
    p.expect_opt(T::Comma)?;
    let values = p.attempt(expressions::star_named_expressions);
    let pos = p.extra(p.pos_at(m))?;
    let mut elts = vec![value];
    elts.extend(values.unwrap_or_default());
    Some(Expr::Tuple {
        elts,
        ctx: Box::new(ExprContext::Load),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

fn case_block(p: &mut Parser) -> Option<MatchCase> {
    if !p.enter_rule() {
        return None;
    }
    let mut res = if p.call_invalid_rules {
        p.attempt(|p| {
            invalid::case_block(p);
            None::<MatchCase>
        })
    } else {
        None
    };
    if res.is_none() && !p.has_error() {
        res = p.attempt(|p| {
            if !p.expect_soft_keyword("case") {
                return None;
            }
            let pattern = patterns::patterns(p)?;
            let guard = p.attempt(guard_rule);
            p.expect_opt(T::Colon)?;
            let body = block(p)?;
            Some(MatchCase {
                pattern: Box::new(pattern),
                guard: guard.map(Box::new),
                body,
            })
        });
    }
    p.exit_rule();
    res
}

pub(crate) fn guard_rule(p: &mut Parser) -> Option<Expr> {
    p.expect_keyword_opt("if")?;
    expressions::named_expression(p)
}

// -- type statements ------------------------------------------------------------

fn type_alias(p: &mut Parser) -> Option<Stmt> {
    let m = p.mark;
    if !p.expect_soft_keyword("type") {
        return None;
    }
    let n = p.name_token()?;
    let t = p.attempt(type_params);
    p.expect_opt(T::Equal)?;
    let b = expressions::expression(p)?;
    let pos = p.extra(p.pos_at(m))?;
    Some(Stmt::TypeAlias {
        name: Box::new(set_expr_context(&n, ExprContext::Store)),
        type_params: t.unwrap_or_default(),
        value: Box::new(b),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno: pos.end_lineno,
        end_col_offset: pos.end_col_offset,
    })
}

pub(crate) fn type_params(p: &mut Parser) -> Option<Vec<TypeParam>> {
    p.expect_opt(T::Lsqb)?;
    let t = type_param_seq(p)?;
    p.expect_opt(T::Rsqb)?;
    Some(t)
}

fn type_param_seq(p: &mut Parser) -> Option<Vec<TypeParam>> {
    let a = p.gather(type_param, |p| p.expect(T::Comma))?;
    p.expect(T::Comma);
    Some(a)
}

fn type_param(p: &mut Parser) -> Option<TypeParam> {
    if !p.enter_rule() {
        return None;
    }
    if let Some(hit) = p.memo_get(|t| &mut t.type_param) {
        p.exit_rule();
        return hit;
    }
    let m = p.mark;
    let res = p.alternatives(&[
        type_param_var,
        type_param_star_bound,
        type_param_star,
        type_param_double_star_bound,
        type_param_double_star,
    ]);
    p.memo_insert(|t| &mut t.type_param, m, res.clone());
    p.exit_rule();
    res
}

fn type_param_var(p: &mut Parser) -> Option<TypeParam> {
    let m = p.mark;
    let a = p.name_token()?;
    let b = p.attempt(type_param_bound);
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(TypeParam::TypeVar {
        name: name_id(&a)?,
        bound: b.map(Box::new),
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn type_param_star_bound(p: &mut Parser) -> Option<TypeParam> {
    p.expect_opt(T::Star)?;
    p.name_token()?;
    let colon = p.mark;
    p.expect_opt(T::Colon)?;
    let e = expressions::expression(p)?;
    let message = if matches!(e, Expr::Tuple { .. }) {
        "cannot use constraints with TypeVarTuple"
    } else {
        "cannot use bound with TypeVarTuple"
    };
    p.raise_starting_from(p.token_pos(colon), message.to_string());
    None
}

fn type_param_star(p: &mut Parser) -> Option<TypeParam> {
    let m = p.mark;
    p.expect_opt(T::Star)?;
    let a = p.name_token()?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(TypeParam::TypeVarTuple {
        name: name_id(&a)?,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn type_param_double_star_bound(p: &mut Parser) -> Option<TypeParam> {
    p.expect_opt(T::Doublestar)?;
    p.name_token()?;
    let colon = p.mark;
    p.expect_opt(T::Colon)?;
    let e = expressions::expression(p)?;
    let message = if matches!(e, Expr::Tuple { .. }) {
        "cannot use constraints with ParamSpec"
    } else {
        "cannot use bound with ParamSpec"
    };
    p.raise_starting_from(p.token_pos(colon), message.to_string());
    None
}

fn type_param_double_star(p: &mut Parser) -> Option<TypeParam> {
    let m = p.mark;
    p.expect_opt(T::Doublestar)?;
    let a = p.name_token()?;
    let pos = p.extra(p.pos_at(m))?;
    let (end_lineno, end_col_offset) = ends(&pos);
    Some(TypeParam::ParamSpec {
        name: name_id(&a)?,
        lineno: pos.lineno,
        col_offset: pos.col_offset,
        end_lineno,
        end_col_offset,
    })
}

fn type_param_bound(p: &mut Parser) -> Option<Expr> {
    p.expect_opt(T::Colon)?;
    expressions::expression(p)
}

// -- targets --------------------------------------------------------------------

pub(crate) fn star_targets(p: &mut Parser) -> Option<Expr> {
    expressions::star_targets(p)
}

pub(crate) fn star_target(p: &mut Parser) -> Option<Expr> {
    expressions::star_target(p)
}

pub(crate) fn single_target(p: &mut Parser) -> Option<Expr> {
    expressions::single_target(p)
}

pub(crate) fn single_subscript_attribute_target(p: &mut Parser) -> Option<Expr> {
    expressions::single_subscript_attribute_target(p)
}

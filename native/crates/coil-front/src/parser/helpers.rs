//! The grammar's action helpers (`Parser/action_helpers.c`): AST shaping
//! over already-matched pieces, plus the error raises that need to walk
//! those pieces.

use super::{Parser, Pos};
use crate::ast::Constant;
use crate::ast_generated::{
    Alias, Arg, Arguments, Cmpop, Expr, ExprContext, Keyword, Pattern, Stmt,
};

/// `CmpopExprPair`.
pub(crate) type CmpopExprPair = (Cmpop, Expr);
/// `KeyValuePair`.
pub(crate) type KeyValuePair = (Option<Expr>, Expr);
/// `KeyPatternPair`.
pub(crate) type KeyPatternPair = (Expr, Pattern);
/// `NameDefaultPair`.
pub(crate) type NameDefaultPair = (Arg, Option<Expr>);
/// `SlashWithDefault`.
pub(crate) type SlashWithDefault = (Vec<Arg>, Vec<NameDefaultPair>);
/// `StarEtc`.
pub(crate) type StarEtc = (Option<Arg>, Vec<NameDefaultPair>, Option<Arg>);

/// `KeywordOrStarred`.
#[derive(Clone, Debug)]
pub(crate) enum KeywordOrStarred {
    Keyword(Keyword),
    Starred(Expr),
}

pub(crate) fn singleton_seq<T>(a: T) -> Vec<T> {
    vec![a]
}

pub(crate) fn seq_insert_in_front<T>(a: T, seq: Option<Vec<T>>) -> Vec<T> {
    let mut out = Vec::with_capacity(seq.as_ref().map_or(1, Vec::len) + 1);
    out.push(a);
    if let Some(seq) = seq {
        out.extend(seq);
    }
    out
}

/// `_PyPegen_set_expr_context`: rebuilds with a new context, recursing into
/// the containers that hold targets.
pub(crate) fn set_expr_context(e: &Expr, ctx: ExprContext) -> Expr {
    match e {
        Expr::Name {
            id,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => Expr::Name {
            id: *id,
            ctx: Box::new(ctx),
            lineno: *lineno,
            col_offset: *col_offset,
            end_lineno: *end_lineno,
            end_col_offset: *end_col_offset,
        },
        Expr::Tuple {
            elts,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => Expr::Tuple {
            elts: elts
                .iter()
                .map(|elt| set_expr_context(elt, ctx.clone()))
                .collect(),
            ctx: Box::new(ctx),
            lineno: *lineno,
            col_offset: *col_offset,
            end_lineno: *end_lineno,
            end_col_offset: *end_col_offset,
        },
        Expr::List {
            elts,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => Expr::List {
            elts: elts
                .iter()
                .map(|elt| set_expr_context(elt, ctx.clone()))
                .collect(),
            ctx: Box::new(ctx),
            lineno: *lineno,
            col_offset: *col_offset,
            end_lineno: *end_lineno,
            end_col_offset: *end_col_offset,
        },
        Expr::Subscript {
            value,
            slice,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => Expr::Subscript {
            value: value.clone(),
            slice: slice.clone(),
            ctx: Box::new(ctx),
            lineno: *lineno,
            col_offset: *col_offset,
            end_lineno: *end_lineno,
            end_col_offset: *end_col_offset,
        },
        Expr::Attribute {
            value,
            attr,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => Expr::Attribute {
            value: value.clone(),
            attr: *attr,
            ctx: Box::new(ctx),
            lineno: *lineno,
            col_offset: *col_offset,
            end_lineno: *end_lineno,
            end_col_offset: *end_col_offset,
        },
        Expr::Starred {
            value,
            lineno,
            col_offset,
            end_lineno,
            end_col_offset,
            ..
        } => Expr::Starred {
            value: Box::new(set_expr_context(value, ctx.clone())),
            ctx: Box::new(ctx),
            lineno: *lineno,
            col_offset: *col_offset,
            end_lineno: *end_lineno,
            end_col_offset: *end_col_offset,
        },
        _ => e.clone(),
    }
}

impl Parser {
    // -- parameters ----------------------------------------------------------

    /// `_PyPegen_make_arguments`.
    pub(crate) fn make_arguments(
        slash_without_default: Option<Vec<Arg>>,
        slash_with_default: Option<SlashWithDefault>,
        plain_names: Option<Vec<Arg>>,
        names_with_default: Option<Vec<NameDefaultPair>>,
        star_etc: Option<StarEtc>,
    ) -> Arguments {
        let slash_with_default_names = |slash: &SlashWithDefault| {
            let mut names: Vec<Arg> = slash.0.clone();
            names.extend(slash.1.iter().map(|pair| pair.0.clone()));
            names
        };
        let posonlyargs = if let Some(names) = slash_without_default {
            names
        } else if let Some(slash) = &slash_with_default {
            slash_with_default_names(slash)
        } else {
            Vec::new()
        };
        let mut posargs: Vec<Arg> = plain_names.unwrap_or_default();
        if let Some(pairs) = &names_with_default {
            posargs.extend(pairs.iter().map(|pair| pair.0.clone()));
        }
        let mut posdefaults: Vec<Option<Expr>> = slash_with_default
            .as_ref()
            .map(|slash| slash.1.iter().map(|pair| pair.1.clone()).collect())
            .unwrap_or_default();
        if let Some(pairs) = &names_with_default {
            posdefaults.extend(pairs.iter().map(|pair| pair.1.clone()));
        }
        let (vararg, kwonlyargs, kwarg) = match star_etc {
            Some((vararg, kwonlyargs, kwarg)) => {
                let kwonly = kwonlyargs;
                (vararg, kwonly, kwarg)
            }
            None => (None, Vec::new(), None),
        };
        let kw_defaults = kwonlyargs.iter().map(|pair| pair.1.clone()).collect();
        Arguments {
            posonlyargs,
            args: posargs,
            vararg: vararg.map(Box::new),
            kwonlyargs: kwonlyargs.into_iter().map(|pair| pair.0).collect(),
            kw_defaults,
            kwarg: kwarg.map(Box::new),
            defaults: posdefaults.into_iter().flatten().collect(),
        }
    }

    /// `_PyPegen_empty_arguments`.
    pub(crate) fn empty_arguments() -> Arguments {
        Arguments {
            posonlyargs: Vec::new(),
            args: Vec::new(),
            vararg: None,
            kwonlyargs: Vec::new(),
            kw_defaults: Vec::new(),
            kwarg: None,
            defaults: Vec::new(),
        }
    }

    // -- calls ----------------------------------------------------------------

    /// `_PyPegen_collect_call_seqs`.
    pub(crate) fn collect_call_seqs(
        &mut self,
        a: Vec<Expr>,
        b: Option<Vec<KeywordOrStarred>>,
        pos: Pos,
    ) -> Option<Expr> {
        let Some(b) = b else {
            return Some(self.dummy_name_call(a, Vec::new(), pos));
        };
        let mut args = a;
        args.extend(
            b.iter()
                .filter_map(|item| match item {
                    KeywordOrStarred::Starred(e) => Some(e.clone()),
                    KeywordOrStarred::Keyword(_) => None,
                })
                .collect::<Vec<_>>(),
        );
        let keywords = b
            .into_iter()
            .filter_map(|item| match item {
                KeywordOrStarred::Keyword(k) => Some(k),
                KeywordOrStarred::Starred(_) => None,
            })
            .collect();
        Some(self.dummy_name_call(args, keywords, pos))
    }

    /// `_PyAST_Call(dummy_name(p), ...)` with a `Name` stand-in for the call
    /// target; the caller replaces it with the real callee.
    fn dummy_name_call(&mut self, args: Vec<Expr>, keywords: Vec<Keyword>, pos: Pos) -> Expr {
        Expr::Call {
            func: Box::new(Expr::Name {
                id: self.interner.intern(""),
                ctx: Box::new(ExprContext::Load),
                lineno: pos.lineno,
                col_offset: pos.col_offset,
                end_lineno: pos.end_lineno,
                end_col_offset: pos.end_col_offset,
            }),
            args,
            keywords,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        }
    }

    /// `_PyPegen_arguments_parsing_error`: the message only depends on
    /// whether any keyword argument unpacking appears.
    pub(crate) fn arguments_parsing_error(&mut self, e: &Expr) -> bool {
        let kwarg_unpacking = match e {
            Expr::Call { keywords, .. } => keywords.iter().any(|keyword| keyword.arg.is_none()),
            _ => false,
        };
        let message = if kwarg_unpacking {
            "positional argument follows keyword argument unpacking"
        } else {
            "positional argument follows keyword argument"
        };
        self.raise_syntax_error(message.to_string())
    }

    /// `_PyPegen_nonparen_genexp_in_call`.
    pub(crate) fn nonparen_genexp_in_call(
        &mut self,
        args: &Expr,
        comprehensions: &[crate::ast_generated::Comprehension],
    ) -> bool {
        let Expr::Call {
            args: call_args, ..
        } = args
        else {
            return false;
        };
        if call_args.len() <= 1 {
            return false;
        }
        let last = comprehensions.last().expect("genexp has a comprehension");
        let end = if last.ifs.is_empty() {
            Pos::from_expr(&last.iter)
        } else {
            Pos::from_expr(last.ifs.last().expect("non-empty"))
        };
        self.raise_known_range(
            Pos::from_expr(call_args.last().expect("non-empty")),
            end,
            "Generator expression must be parenthesized".to_string(),
        )
    }
}

/// `TARGETS_TYPE` for `_PyPegen_get_invalid_target`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetsType {
    Star,
    Del,
    For,
}

/// `_PyPegen_get_invalid_target`: the first subexpression that cannot be a
/// target, `None` when the whole expression can.
pub(crate) fn get_invalid_target(e: &Expr, targets_type: TargetsType) -> Option<Expr> {
    match e {
        Expr::List { elts, .. } | Expr::Tuple { elts, .. } => elts
            .iter()
            .find_map(|elt| get_invalid_target(elt, targets_type)),
        Expr::Starred { value, .. } => {
            if targets_type == TargetsType::Del {
                Some(e.clone())
            } else {
                get_invalid_target(value, targets_type)
            }
        }
        Expr::Compare { left, ops, .. } => {
            if targets_type == TargetsType::For {
                if ops.first().is_some_and(|op| matches!(op, Cmpop::In)) {
                    get_invalid_target(left, targets_type)
                } else {
                    None
                }
            } else {
                Some(e.clone())
            }
        }
        Expr::Name { .. } | Expr::Subscript { .. } | Expr::Attribute { .. } => None,
        _ => Some(e.clone()),
    }
}

impl Parser {
    /// `_RAISE_SYNTAX_ERROR_INVALID_TARGET`: declines when every target is
    /// valid, letting the rule fail generically.
    pub(crate) fn raise_invalid_target(
        &mut self,
        e: &Expr,
        targets_type: TargetsType,
    ) -> Option<()> {
        let invalid = get_invalid_target(e, targets_type)?;
        let verb = if targets_type == TargetsType::Del {
            "cannot delete"
        } else {
            "cannot assign to"
        };
        self.raise_known_location(
            Pos::from_expr(&invalid),
            format!("{} {}", verb, get_expr_name(&invalid)),
        );
        None
    }
}

/// `_PyPegen_get_expr_name`.
pub(crate) fn get_expr_name(e: &Expr) -> &'static str {
    match e {
        Expr::Attribute { .. } => "attribute",
        Expr::Subscript { .. } => "subscript",
        Expr::Starred { .. } => "starred",
        Expr::Name { .. } => "name",
        Expr::List { .. } => "list",
        Expr::Tuple { .. } => "tuple",
        Expr::Lambda { .. } => "lambda",
        Expr::Call { .. } => "function call",
        Expr::BoolOp { .. } | Expr::BinOp { .. } | Expr::UnaryOp { .. } => "expression",
        Expr::GeneratorExp { .. } => "generator expression",
        Expr::Yield { .. } | Expr::YieldFrom { .. } => "yield expression",
        Expr::Await { .. } => "await expression",
        Expr::ListComp { .. } => "list comprehension",
        Expr::SetComp { .. } => "set comprehension",
        Expr::DictComp { .. } => "dict comprehension",
        Expr::Dict { .. } => "dict literal",
        Expr::Set { .. } => "set display",
        Expr::JoinedStr { .. } | Expr::FormattedValue { .. } => "f-string expression",
        Expr::Constant { value, .. } => match value {
            Constant::None => "None",
            Constant::Bool(false) => "False",
            Constant::Bool(true) => "True",
            Constant::Ellipsis => "ellipsis",
            _ => "literal",
        },
        Expr::Compare { .. } => "comparison",
        Expr::IfExp { .. } => "conditional expression",
        Expr::NamedExpr { .. } => "named expression",
        Expr::Slice { .. } => "slice",
    }
}

impl Parser {
    /// `_PyPegen_checked_future_import`: the future flag this front end does
    /// not model is a typed error.
    pub(crate) fn checked_future_import(
        &mut self,
        module: &crate::intern::Sym,
        names: &[Alias],
        level: i32,
        pos: Pos,
    ) -> Option<Stmt> {
        if level == 0 {
            let module_name = utf32_to_string(self.interner.resolve(*module));
            if module_name == "__future__" {
                for alias in names {
                    let name = utf32_to_string(self.interner.resolve(alias.name));
                    if name == "barry_as_FLUFL" {
                        self.raise_known_location(
                            pos,
                            "barry_as_FLUFL future import is not supported".to_string(),
                        );
                        return None;
                    }
                }
            }
        }
        Some(Stmt::ImportFrom {
            module: Some(*module),
            names: names.to_vec(),
            level: Some(level),
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        })
    }
}

/// The interner stores code points; messages and comparisons want text.
pub(crate) fn utf32_to_string(code_points: &[u32]) -> String {
    code_points
        .iter()
        .map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}'))
        .collect()
}

/// `_PyPegen_check_legacy_stmt`.
pub(crate) fn check_legacy_stmt(p: &Parser, name: &Expr) -> bool {
    let Expr::Name { id, .. } = name else {
        return false;
    };
    let text = utf32_to_string(p.interner.resolve(*id));
    text == "print" || text == "exec"
}

impl Parser {
    /// `_PyPegen_function_def_decorators`.
    pub(crate) fn function_def_decorators(decorators: Vec<Expr>, function_def: Stmt) -> Stmt {
        match function_def {
            Stmt::FunctionDef {
                name,
                args,
                body,
                returns,
                type_comment,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => Stmt::FunctionDef {
                name,
                args,
                body,
                decorator_list: decorators,
                returns,
                type_comment,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            },
            Stmt::AsyncFunctionDef {
                name,
                args,
                body,
                returns,
                type_comment,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => Stmt::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list: decorators,
                returns,
                type_comment,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            },
            other => other,
        }
    }

    /// `_PyPegen_class_def_decorators`.
    pub(crate) fn class_def_decorators(decorators: Vec<Expr>, class_def: Stmt) -> Stmt {
        match class_def {
            Stmt::ClassDef {
                name,
                bases,
                keywords,
                body,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
                ..
            } => Stmt::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list: decorators,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            },
            other => other,
        }
    }

    /// `_PyPegen_ensure_imaginary`.
    pub(crate) fn ensure_imaginary(&mut self, exp: &Expr) -> Option<Expr> {
        match exp {
            Expr::Constant {
                value: Constant::Complex { .. },
                ..
            } => Some(exp.clone()),
            _ => {
                self.raise_known_location(
                    Pos::from_expr(exp),
                    "imaginary number required in complex literal".to_string(),
                );
                None
            }
        }
    }

    /// `_PyPegen_ensure_real`.
    pub(crate) fn ensure_real(&mut self, exp: &Expr) -> Option<Expr> {
        match exp {
            Expr::Constant {
                value: Constant::Complex { .. },
                ..
            } => {
                self.raise_known_location(
                    Pos::from_expr(exp),
                    "real number required in complex literal".to_string(),
                );
                None
            }
            _ => Some(exp.clone()),
        }
    }

    /// `_PyPegen_alias_for_star`.
    pub(crate) fn alias_for_star(&mut self, pos: Pos) -> Alias {
        Alias {
            name: self.interner.intern("*"),
            asname: None,
            lineno: pos.lineno,
            col_offset: pos.col_offset,
            end_lineno: pos.end_lineno,
            end_col_offset: pos.end_col_offset,
        }
    }
}

/// `_PyPegen_map_names_to_ids`.
pub(crate) fn map_names_to_ids(seq: &[Expr]) -> Vec<crate::intern::Sym> {
    seq.iter()
        .map(|e| match e {
            Expr::Name { id, .. } => *id,
            _ => unreachable!("grammar guarantees names"),
        })
        .collect()
}

pub(crate) fn get_keys(seq: &[KeyValuePair]) -> Vec<Option<Expr>> {
    seq.iter().map(|pair| pair.0.clone()).collect()
}

pub(crate) fn get_values(seq: &[KeyValuePair]) -> Vec<Expr> {
    seq.iter().map(|pair| pair.1.clone()).collect()
}

pub(crate) fn get_pattern_keys(seq: &[KeyPatternPair]) -> Vec<Expr> {
    seq.iter().map(|pair| pair.0.clone()).collect()
}

pub(crate) fn get_patterns(seq: &[KeyPatternPair]) -> Vec<Pattern> {
    seq.iter().map(|pair| pair.1.clone()).collect()
}

pub(crate) fn get_cmpops(seq: &[CmpopExprPair]) -> Vec<Cmpop> {
    seq.iter().map(|pair| pair.0.clone()).collect()
}

pub(crate) fn get_exprs(seq: &[CmpopExprPair]) -> Vec<Expr> {
    seq.iter().map(|pair| pair.1.clone()).collect()
}

/// `_PyPegen_join_names_with_dot`: a `Name` whose id carries the dots, like
/// `dotted_name` builds for imports.
pub(crate) fn join_names_with_dot(p: &mut Parser, a: &Expr, b: &Expr) -> Option<Expr> {
    let Expr::Name { id: first, .. } = a else {
        return None;
    };
    let Expr::Name { id: second, .. } = b else {
        return None;
    };
    let mut joined = utf32_to_string(p.interner.resolve(*first));
    joined.push('.');
    joined.push_str(&utf32_to_string(p.interner.resolve(*second)));
    // The pieces were normalized at match time; the dot between them blocks
    // any further composition, so CPython does not renormalize here.
    let code_points: Vec<u32> = joined.chars().map(|c| c as u32).collect();
    let id = p.interner.intern_code_points(code_points);
    Some(Expr::Name {
        id,
        ctx: Box::new(ExprContext::Load),
        lineno: Pos::from_expr(a).lineno,
        col_offset: Pos::from_expr(a).col_offset,
        end_lineno: Pos::from_expr(b).end_lineno,
        end_col_offset: Pos::from_expr(b).end_col_offset,
    })
}

/// `_PyPegen_seq_count_dots` over already-counted tokens.
pub(crate) fn seq_count_dots(seq: &[u8]) -> i32 {
    seq.len() as i32
}

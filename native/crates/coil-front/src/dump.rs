//! Canonical JSON for AST modules, byte-identical to the encoding
//! tools/front_oracle.py documents: field order is `_type`, the node's
//! `_fields`, then its `_attributes`; separators are `,` and `:`; strings are
//! escaped the way json.dumps(..., ensure_ascii=True) escapes them. The JSON
//! is written directly; there is no serialisation framework.

use std::fmt::Write as _;

use crate::ast::*;
use crate::intern::{Interner, Sym};
use crate::symtable::SymbolTable;

/// Serialises `module` exactly as front_oracle.py serialises the `ast` field
/// of its `--ast` record for the same source.
pub fn dump_module(module: &Mod, interner: &Interner) -> String {
    let mut out = String::new();
    Writer {
        out: &mut out,
        interner,
    }
    .module(module);
    out
}

/// Serialises `table` exactly as front_oracle.py serialises the `symtable`
/// field of its `--symtable` record for the same source.
pub fn dump_symtable(table: &SymbolTable, interner: &Interner) -> String {
    let mut out = String::new();
    Writer {
        out: &mut out,
        interner,
    }
    .symbol_table(table);
    out
}

struct Writer<'a> {
    out: &'a mut String,
    interner: &'a Interner,
}

impl Writer<'_> {
    fn raw(&mut self, text: &str) {
        self.out.push_str(text);
    }

    /// `{"_type":"<name>"`, leaving the object open for its fields.
    fn node(&mut self, type_name: &str) {
        self.raw("{\"_type\":\"");
        self.raw(type_name);
        self.raw("\"");
    }

    /// `,"<name>":` before a field value.
    fn field(&mut self, name: &str) {
        self.raw(",\"");
        self.raw(name);
        self.raw("\":");
    }

    fn end(&mut self) {
        self.raw("}");
    }

    fn int(&mut self, value: i32) {
        let _ = write!(self.out, "{value}");
    }

    fn opt_int(&mut self, value: Option<i32>) {
        match value {
            Some(value) => self.int(value),
            None => self.raw("null"),
        }
    }

    /// The four position attributes whose end_* are optional in the ASDL.
    fn attrs_opt_end(
        &mut self,
        lineno: i32,
        col_offset: i32,
        end_lineno: Option<i32>,
        end_col_offset: Option<i32>,
    ) {
        self.field("lineno");
        self.int(lineno);
        self.field("col_offset");
        self.int(col_offset);
        self.field("end_lineno");
        self.opt_int(end_lineno);
        self.field("end_col_offset");
        self.opt_int(end_col_offset);
    }

    /// The four position attributes, all required (pattern, type_param).
    fn attrs_req_end(
        &mut self,
        lineno: i32,
        col_offset: i32,
        end_lineno: i32,
        end_col_offset: i32,
    ) {
        self.field("lineno");
        self.int(lineno);
        self.field("col_offset");
        self.int(col_offset);
        self.field("end_lineno");
        self.int(end_lineno);
        self.field("end_col_offset");
        self.int(end_col_offset);
    }

    /// A JSON string over code points: every code point outside space .. `~`
    /// is escaped, astral ones as surrogate pairs, hex lowercase.
    fn string(&mut self, code_points: &[u32]) {
        self.raw("\"");
        for &code_point in code_points {
            match code_point {
                0x22 => self.raw("\\\""),
                0x5C => self.raw("\\\\"),
                0x08 => self.raw("\\b"),
                0x09 => self.raw("\\t"),
                0x0A => self.raw("\\n"),
                0x0C => self.raw("\\f"),
                0x0D => self.raw("\\r"),
                0x20..=0x7E => self.out.push(code_point as u8 as char),
                code_point if code_point <= 0xFFFF => {
                    let _ = write!(self.out, "\\u{code_point:04x}");
                }
                code_point => {
                    let pair = code_point - 0x10000;
                    let _ = write!(
                        self.out,
                        "\\u{:04x}\\u{:04x}",
                        0xD800 + (pair >> 10),
                        0xDC00 + (pair & 0x3FF)
                    );
                }
            }
        }
        self.raw("\"");
    }

    fn sym(&mut self, sym: Sym) {
        self.string(self.interner.resolve(sym));
    }

    fn opt_sym(&mut self, sym: Option<Sym>) {
        match sym {
            Some(sym) => self.sym(sym),
            None => self.raw("null"),
        }
    }

    fn list<T>(&mut self, items: &[T], write: fn(&mut Self, &T)) {
        self.raw("[");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                self.raw(",");
            }
            write(self, item);
        }
        self.raw("]");
    }

    /// A list whose elements may be null (Dict keys, kw_defaults).
    fn opt_exprs(&mut self, items: &[Option<Expr>]) {
        self.list(items, |writer, item| match item {
            Some(expr) => writer.expr(expr),
            None => writer.raw("null"),
        });
    }

    fn opt_expr(&mut self, expr: &Option<Box<Expr>>) {
        match expr {
            Some(expr) => self.expr(expr),
            None => self.raw("null"),
        }
    }

    fn opt_arg(&mut self, arg: &Option<Box<Arg>>) {
        match arg {
            Some(arg) => self.arg(arg),
            None => self.raw("null"),
        }
    }

    fn opt_pattern(&mut self, pattern: &Option<Box<Pattern>>) {
        match pattern {
            Some(pattern) => self.pattern(pattern),
            None => self.raw("null"),
        }
    }

    fn constant(&mut self, value: &Constant) {
        match value {
            Constant::Str(code_points) => {
                self.raw("{\"const\":\"str\",\"v\":[");
                for (index, &code_point) in code_points.iter().enumerate() {
                    if index > 0 {
                        self.raw(",");
                    }
                    let _ = write!(self.out, "{code_point}");
                }
                self.raw("]}");
            }
            Constant::Bytes(bytes) => {
                self.raw("{\"const\":\"bytes\",\"v\":\"");
                for &byte in bytes {
                    let _ = write!(self.out, "{byte:02x}");
                }
                self.raw("\"}");
            }
            Constant::Int(digits) => {
                self.raw("{\"const\":\"int\",\"v\":\"");
                self.raw(digits);
                self.raw("\"}");
            }
            Constant::Float(value) => {
                self.raw("{\"const\":\"float\",\"v\":\"");
                self.raw(&python_repr_f64(*value));
                self.raw("\"}");
            }
            Constant::Complex { real, imag } => {
                self.raw("{\"const\":\"complex\",\"real\":\"");
                self.raw(&python_repr_f64(*real));
                self.raw("\",\"imag\":\"");
                self.raw(&python_repr_f64(*imag));
                self.raw("\"}");
            }
            Constant::Bool(value) => {
                self.raw(if *value {
                    "{\"const\":\"bool\",\"v\":true}"
                } else {
                    "{\"const\":\"bool\",\"v\":false}"
                });
            }
            Constant::None => self.raw("{\"const\":\"None\"}"),
            Constant::Ellipsis => self.raw("{\"const\":\"Ellipsis\"}"),
        }
    }

    // One method per ASDL type, constructors and fields in ASDL order.

    fn module(&mut self, module: &Mod) {
        match module {
            Mod::Module { body, type_ignores } => {
                self.node("Module");
                self.field("body");
                self.list(body, Self::stmt);
                self.field("type_ignores");
                self.list(type_ignores, Self::type_ignore);
                self.end();
            }
            Mod::Interactive { body } => {
                self.node("Interactive");
                self.field("body");
                self.list(body, Self::stmt);
                self.end();
            }
            Mod::Expression { body } => {
                self.node("Expression");
                self.field("body");
                self.expr(body);
                self.end();
            }
            Mod::FunctionType { argtypes, returns } => {
                self.node("FunctionType");
                self.field("argtypes");
                self.list(argtypes, Self::expr);
                self.field("returns");
                self.expr(returns);
                self.end();
            }
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::FunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_comment,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("FunctionDef");
                self.field("name");
                self.sym(*name);
                self.field("args");
                self.arguments(args);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("decorator_list");
                self.list(decorator_list, Self::expr);
                self.field("returns");
                self.opt_expr(returns);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.field("type_params");
                self.list(type_params, Self::type_param);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::AsyncFunctionDef {
                name,
                args,
                body,
                decorator_list,
                returns,
                type_comment,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("AsyncFunctionDef");
                self.field("name");
                self.sym(*name);
                self.field("args");
                self.arguments(args);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("decorator_list");
                self.list(decorator_list, Self::expr);
                self.field("returns");
                self.opt_expr(returns);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.field("type_params");
                self.list(type_params, Self::type_param);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::ClassDef {
                name,
                bases,
                keywords,
                body,
                decorator_list,
                type_params,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("ClassDef");
                self.field("name");
                self.sym(*name);
                self.field("bases");
                self.list(bases, Self::expr);
                self.field("keywords");
                self.list(keywords, Self::keyword);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("decorator_list");
                self.list(decorator_list, Self::expr);
                self.field("type_params");
                self.list(type_params, Self::type_param);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Return {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Return");
                self.field("value");
                self.opt_expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Delete {
                targets,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Delete");
                self.field("targets");
                self.list(targets, Self::expr);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Assign {
                targets,
                value,
                type_comment,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Assign");
                self.field("targets");
                self.list(targets, Self::expr);
                self.field("value");
                self.expr(value);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::TypeAlias {
                name,
                type_params,
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("TypeAlias");
                self.field("name");
                self.expr(name);
                self.field("type_params");
                self.list(type_params, Self::type_param);
                self.field("value");
                self.expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::AugAssign {
                target,
                op,
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("AugAssign");
                self.field("target");
                self.expr(target);
                self.field("op");
                self.operator(op);
                self.field("value");
                self.expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::AnnAssign {
                target,
                annotation,
                value,
                simple,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("AnnAssign");
                self.field("target");
                self.expr(target);
                self.field("annotation");
                self.expr(annotation);
                self.field("value");
                self.opt_expr(value);
                self.field("simple");
                self.int(*simple);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::For {
                target,
                iter,
                body,
                orelse,
                type_comment,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("For");
                self.field("target");
                self.expr(target);
                self.field("iter");
                self.expr(iter);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("orelse");
                self.list(orelse, Self::stmt);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::AsyncFor {
                target,
                iter,
                body,
                orelse,
                type_comment,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("AsyncFor");
                self.field("target");
                self.expr(target);
                self.field("iter");
                self.expr(iter);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("orelse");
                self.list(orelse, Self::stmt);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::While {
                test,
                body,
                orelse,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("While");
                self.field("test");
                self.expr(test);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("orelse");
                self.list(orelse, Self::stmt);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::If {
                test,
                body,
                orelse,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("If");
                self.field("test");
                self.expr(test);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("orelse");
                self.list(orelse, Self::stmt);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::With {
                items,
                body,
                type_comment,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("With");
                self.field("items");
                self.list(items, Self::withitem);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::AsyncWith {
                items,
                body,
                type_comment,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("AsyncWith");
                self.field("items");
                self.list(items, Self::withitem);
                self.field("body");
                self.list(body, Self::stmt);
                self.field("type_comment");
                self.opt_sym(*type_comment);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Match {
                subject,
                cases,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Match");
                self.field("subject");
                self.expr(subject);
                self.field("cases");
                self.list(cases, Self::match_case);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Raise {
                exc,
                cause,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Raise");
                self.field("exc");
                self.opt_expr(exc);
                self.field("cause");
                self.opt_expr(cause);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Try {
                body,
                handlers,
                orelse,
                finalbody,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Try");
                self.field("body");
                self.list(body, Self::stmt);
                self.field("handlers");
                self.list(handlers, Self::excepthandler);
                self.field("orelse");
                self.list(orelse, Self::stmt);
                self.field("finalbody");
                self.list(finalbody, Self::stmt);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::TryStar {
                body,
                handlers,
                orelse,
                finalbody,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("TryStar");
                self.field("body");
                self.list(body, Self::stmt);
                self.field("handlers");
                self.list(handlers, Self::excepthandler);
                self.field("orelse");
                self.list(orelse, Self::stmt);
                self.field("finalbody");
                self.list(finalbody, Self::stmt);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Assert {
                test,
                msg,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Assert");
                self.field("test");
                self.expr(test);
                self.field("msg");
                self.opt_expr(msg);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Import {
                names,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Import");
                self.field("names");
                self.list(names, Self::alias);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::ImportFrom {
                module,
                names,
                level,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("ImportFrom");
                self.field("module");
                self.opt_sym(*module);
                self.field("names");
                self.list(names, Self::alias);
                self.field("level");
                self.opt_int(*level);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Global {
                names,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Global");
                self.field("names");
                self.list(names, |writer, &sym| writer.sym(sym));
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Nonlocal {
                names,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Nonlocal");
                self.field("names");
                self.list(names, |writer, &sym| writer.sym(sym));
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Expr {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Expr");
                self.field("value");
                self.expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Pass {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Pass");
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Break {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Break");
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Stmt::Continue {
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Continue");
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::BoolOp {
                op,
                values,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("BoolOp");
                self.field("op");
                self.boolop(op);
                self.field("values");
                self.list(values, Self::expr);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::NamedExpr {
                target,
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("NamedExpr");
                self.field("target");
                self.expr(target);
                self.field("value");
                self.expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::BinOp {
                left,
                op,
                right,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("BinOp");
                self.field("left");
                self.expr(left);
                self.field("op");
                self.operator(op);
                self.field("right");
                self.expr(right);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::UnaryOp {
                op,
                operand,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("UnaryOp");
                self.field("op");
                self.unaryop(op);
                self.field("operand");
                self.expr(operand);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Lambda {
                args,
                body,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Lambda");
                self.field("args");
                self.arguments(args);
                self.field("body");
                self.expr(body);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::IfExp {
                test,
                body,
                orelse,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("IfExp");
                self.field("test");
                self.expr(test);
                self.field("body");
                self.expr(body);
                self.field("orelse");
                self.expr(orelse);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Dict {
                keys,
                values,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Dict");
                self.field("keys");
                self.opt_exprs(keys);
                self.field("values");
                self.list(values, Self::expr);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Set {
                elts,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Set");
                self.field("elts");
                self.list(elts, Self::expr);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::ListComp {
                elt,
                generators,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("ListComp");
                self.field("elt");
                self.expr(elt);
                self.field("generators");
                self.list(generators, Self::comprehension);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::SetComp {
                elt,
                generators,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("SetComp");
                self.field("elt");
                self.expr(elt);
                self.field("generators");
                self.list(generators, Self::comprehension);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::DictComp {
                key,
                value,
                generators,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("DictComp");
                self.field("key");
                self.expr(key);
                self.field("value");
                self.expr(value);
                self.field("generators");
                self.list(generators, Self::comprehension);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::GeneratorExp {
                elt,
                generators,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("GeneratorExp");
                self.field("elt");
                self.expr(elt);
                self.field("generators");
                self.list(generators, Self::comprehension);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Await {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Await");
                self.field("value");
                self.expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Yield {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Yield");
                self.field("value");
                self.opt_expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::YieldFrom {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("YieldFrom");
                self.field("value");
                self.expr(value);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Compare {
                left,
                ops,
                comparators,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Compare");
                self.field("left");
                self.expr(left);
                self.field("ops");
                self.list(ops, Self::cmpop);
                self.field("comparators");
                self.list(comparators, Self::expr);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Call {
                func,
                args,
                keywords,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Call");
                self.field("func");
                self.expr(func);
                self.field("args");
                self.list(args, Self::expr);
                self.field("keywords");
                self.list(keywords, Self::keyword);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::FormattedValue {
                value,
                conversion,
                format_spec,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("FormattedValue");
                self.field("value");
                self.expr(value);
                self.field("conversion");
                self.int(*conversion);
                self.field("format_spec");
                self.opt_expr(format_spec);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::JoinedStr {
                values,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("JoinedStr");
                self.field("values");
                self.list(values, Self::expr);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Constant {
                value,
                kind,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Constant");
                self.field("value");
                self.constant(value);
                self.field("kind");
                self.opt_sym(*kind);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Attribute {
                value,
                attr,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Attribute");
                self.field("value");
                self.expr(value);
                self.field("attr");
                self.sym(*attr);
                self.field("ctx");
                self.expr_context(ctx);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Subscript {
                value,
                slice,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Subscript");
                self.field("value");
                self.expr(value);
                self.field("slice");
                self.expr(slice);
                self.field("ctx");
                self.expr_context(ctx);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Starred {
                value,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Starred");
                self.field("value");
                self.expr(value);
                self.field("ctx");
                self.expr_context(ctx);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Name {
                id,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Name");
                self.field("id");
                self.sym(*id);
                self.field("ctx");
                self.expr_context(ctx);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::List {
                elts,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("List");
                self.field("elts");
                self.list(elts, Self::expr);
                self.field("ctx");
                self.expr_context(ctx);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Tuple {
                elts,
                ctx,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Tuple");
                self.field("elts");
                self.list(elts, Self::expr);
                self.field("ctx");
                self.expr_context(ctx);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Expr::Slice {
                lower,
                upper,
                step,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("Slice");
                self.field("lower");
                self.opt_expr(lower);
                self.field("upper");
                self.opt_expr(upper);
                self.field("step");
                self.opt_expr(step);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
        }
    }

    fn expr_context(&mut self, ctx: &ExprContext) {
        self.node(match ctx {
            ExprContext::Load => "Load",
            ExprContext::Store => "Store",
            ExprContext::Del => "Del",
        });
        self.end();
    }

    fn boolop(&mut self, op: &Boolop) {
        self.node(match op {
            Boolop::And => "And",
            Boolop::Or => "Or",
        });
        self.end();
    }

    fn operator(&mut self, op: &Operator) {
        self.node(match op {
            Operator::Add => "Add",
            Operator::Sub => "Sub",
            Operator::Mult => "Mult",
            Operator::MatMult => "MatMult",
            Operator::Div => "Div",
            Operator::Mod => "Mod",
            Operator::Pow => "Pow",
            Operator::LShift => "LShift",
            Operator::RShift => "RShift",
            Operator::BitOr => "BitOr",
            Operator::BitXor => "BitXor",
            Operator::BitAnd => "BitAnd",
            Operator::FloorDiv => "FloorDiv",
        });
        self.end();
    }

    fn unaryop(&mut self, op: &Unaryop) {
        self.node(match op {
            Unaryop::Invert => "Invert",
            Unaryop::Not => "Not",
            Unaryop::UAdd => "UAdd",
            Unaryop::USub => "USub",
        });
        self.end();
    }

    fn cmpop(&mut self, op: &Cmpop) {
        self.node(match op {
            Cmpop::Eq => "Eq",
            Cmpop::NotEq => "NotEq",
            Cmpop::Lt => "Lt",
            Cmpop::LtE => "LtE",
            Cmpop::Gt => "Gt",
            Cmpop::GtE => "GtE",
            Cmpop::Is => "Is",
            Cmpop::IsNot => "IsNot",
            Cmpop::In => "In",
            Cmpop::NotIn => "NotIn",
        });
        self.end();
    }

    fn comprehension(&mut self, comprehension: &Comprehension) {
        self.node("comprehension");
        self.field("target");
        self.expr(&comprehension.target);
        self.field("iter");
        self.expr(&comprehension.iter);
        self.field("ifs");
        self.list(&comprehension.ifs, Self::expr);
        self.field("is_async");
        self.int(comprehension.is_async);
        self.end();
    }

    fn excepthandler(&mut self, handler: &Excepthandler) {
        match handler {
            Excepthandler::ExceptHandler {
                r#type,
                name,
                body,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("ExceptHandler");
                self.field("type");
                self.opt_expr(r#type);
                self.field("name");
                self.opt_sym(*name);
                self.field("body");
                self.list(body, Self::stmt);
                self.attrs_opt_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
        }
    }

    fn arguments(&mut self, arguments: &Arguments) {
        self.node("arguments");
        self.field("posonlyargs");
        self.list(&arguments.posonlyargs, Self::arg);
        self.field("args");
        self.list(&arguments.args, Self::arg);
        self.field("vararg");
        self.opt_arg(&arguments.vararg);
        self.field("kwonlyargs");
        self.list(&arguments.kwonlyargs, Self::arg);
        self.field("kw_defaults");
        self.opt_exprs(&arguments.kw_defaults);
        self.field("kwarg");
        self.opt_arg(&arguments.kwarg);
        self.field("defaults");
        self.list(&arguments.defaults, Self::expr);
        self.end();
    }

    fn arg(&mut self, arg: &Arg) {
        self.node("arg");
        self.field("arg");
        self.sym(arg.arg);
        self.field("annotation");
        self.opt_expr(&arg.annotation);
        self.field("type_comment");
        self.opt_sym(arg.type_comment);
        self.attrs_opt_end(
            arg.lineno,
            arg.col_offset,
            arg.end_lineno,
            arg.end_col_offset,
        );
        self.end();
    }

    fn keyword(&mut self, keyword: &Keyword) {
        self.node("keyword");
        self.field("arg");
        self.opt_sym(keyword.arg);
        self.field("value");
        self.expr(&keyword.value);
        self.attrs_opt_end(
            keyword.lineno,
            keyword.col_offset,
            keyword.end_lineno,
            keyword.end_col_offset,
        );
        self.end();
    }

    fn alias(&mut self, alias: &Alias) {
        self.node("alias");
        self.field("name");
        self.sym(alias.name);
        self.field("asname");
        self.opt_sym(alias.asname);
        self.attrs_opt_end(
            alias.lineno,
            alias.col_offset,
            alias.end_lineno,
            alias.end_col_offset,
        );
        self.end();
    }

    fn withitem(&mut self, item: &Withitem) {
        self.node("withitem");
        self.field("context_expr");
        self.expr(&item.context_expr);
        self.field("optional_vars");
        self.opt_expr(&item.optional_vars);
        self.end();
    }

    fn match_case(&mut self, case: &MatchCase) {
        self.node("match_case");
        self.field("pattern");
        self.pattern(&case.pattern);
        self.field("guard");
        self.opt_expr(&case.guard);
        self.field("body");
        self.list(&case.body, Self::stmt);
        self.end();
    }

    fn pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::MatchValue {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchValue");
                self.field("value");
                self.expr(value);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchSingleton {
                value,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchSingleton");
                self.field("value");
                self.constant(value);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchSequence {
                patterns,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchSequence");
                self.field("patterns");
                self.list(patterns, Self::pattern);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchMapping {
                keys,
                patterns,
                rest,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchMapping");
                self.field("keys");
                self.list(keys, Self::expr);
                self.field("patterns");
                self.list(patterns, Self::pattern);
                self.field("rest");
                self.opt_sym(*rest);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchClass {
                cls,
                patterns,
                kwd_attrs,
                kwd_patterns,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchClass");
                self.field("cls");
                self.expr(cls);
                self.field("patterns");
                self.list(patterns, Self::pattern);
                self.field("kwd_attrs");
                self.list(kwd_attrs, |writer, &sym| writer.sym(sym));
                self.field("kwd_patterns");
                self.list(kwd_patterns, Self::pattern);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchStar {
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchStar");
                self.field("name");
                self.opt_sym(*name);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchAs {
                pattern,
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchAs");
                self.field("pattern");
                self.opt_pattern(pattern);
                self.field("name");
                self.opt_sym(*name);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            Pattern::MatchOr {
                patterns,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("MatchOr");
                self.field("patterns");
                self.list(patterns, Self::pattern);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
        }
    }

    fn type_ignore(&mut self, type_ignore: &TypeIgnore) {
        match type_ignore {
            TypeIgnore::TypeIgnore { lineno, tag } => {
                self.node("TypeIgnore");
                self.field("lineno");
                self.int(*lineno);
                self.field("tag");
                self.sym(*tag);
                self.end();
            }
        }
    }

    fn type_param(&mut self, type_param: &TypeParam) {
        match type_param {
            TypeParam::TypeVar {
                name,
                bound,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("TypeVar");
                self.field("name");
                self.sym(*name);
                self.field("bound");
                self.opt_expr(bound);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            TypeParam::ParamSpec {
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("ParamSpec");
                self.field("name");
                self.sym(*name);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
            TypeParam::TypeVarTuple {
                name,
                lineno,
                col_offset,
                end_lineno,
                end_col_offset,
            } => {
                self.node("TypeVarTuple");
                self.field("name");
                self.sym(*name);
                self.attrs_req_end(*lineno, *col_offset, *end_lineno, *end_col_offset);
                self.end();
            }
        }
    }

    /// A JSON string over a Rust `&str`, escaped like `string`.
    fn text(&mut self, text: &str) {
        let code_points: Vec<u32> = text.chars().map(u32::from).collect();
        self.string(&code_points);
    }

    /// `encode_table` in front_oracle.py: the table's fields in its order,
    /// symbols sorted by name code points, each mapping to its sorted true
    /// `symtable.Symbol` predicates, then the children in table order.
    fn symbol_table(&mut self, table: &SymbolTable) {
        let top: Vec<u32> = "top".chars().map(u32::from).collect();
        // `SymbolTable.lookup`'s `module_scope`: the table name is "top".
        let module_scope = self.interner.resolve(table.name) == top.as_slice();
        self.raw("{\"name\":");
        self.sym(table.name);
        self.raw(",\"type\":");
        self.text(table.kind.reported_name());
        self.raw(",\"lineno\":");
        self.int(table.lineno);
        self.raw(",\"is_optimized\":");
        self.raw(if table.kind.is_optimized() {
            "true"
        } else {
            "false"
        });
        self.raw(",\"is_nested\":");
        self.raw(if table.is_nested { "true" } else { "false" });
        self.raw(",\"symbols\":{");
        let mut symbols = table.symbols.clone();
        symbols.sort_by(|a, b| self.interner.resolve(a.0).cmp(self.interner.resolve(b.0)));
        for (index, (name, flags)) in symbols.iter().enumerate() {
            if index > 0 {
                self.raw(",");
            }
            self.sym(*name);
            self.raw(":[");
            // `Symbol.is_namespace`: a child block bound to the same name.
            let is_namespace = table.children.iter().any(|child| child.name == *name);
            for (index, predicate) in
                SymbolTable::symbol_predicates(*flags, module_scope, is_namespace)
                    .into_iter()
                    .enumerate()
            {
                if index > 0 {
                    self.raw(",");
                }
                self.text(predicate);
            }
            self.raw("]");
        }
        self.raw("},\"children\":[");
        for (index, child) in table.children.iter().enumerate() {
            if index > 0 {
                self.raw(",");
            }
            self.symbol_table(child);
        }
        self.raw("]}");
    }
}

/// CPython's `repr` for an f64: the shortest digits that round-trip, fixed
/// notation while the decimal point sits at -3..16, otherwise `d.ddde±dd`
/// with a signed two-digit exponent; "inf", "-inf" and "nan" verbatim.
/// Scientific-notation digits of a finite non-negative value: (digits, exponent).
fn scientific(text: &str) -> (String, i32) {
    let (mantissa, exponent) = text.split_at(text.find('e').unwrap());
    let digits = mantissa.chars().filter(|&c| c != '.').collect();
    (digits, exponent[1..].parse().unwrap())
}

/// The digits Python's repr uses and the position of the decimal point: the
/// shortest count that round-trips, correctly rounded with ties to even.
/// Rust's `{:e}` gives the right count but breaks exact decimal ties upwards
/// (`185262743923125.125` -> `...13`, Python `...12`), so the digits are
/// re-rounded from the exact expansion.
fn shortest_digits(value: f64) -> (String, i32) {
    let n = scientific(&format!("{value:e}")).0.len();
    // 767 significant digits hold any f64 exactly. Its exponent can be one
    // below the shortest form's (1e23 is 9.99...e22); rounding carries it back.
    let (exact, exponent) = scientific(&format!("{value:.767e}"));
    let (head, tail) = exact.split_at(n);
    let tail = tail.trim_end_matches('0');
    let round_up = match tail.as_bytes().first() {
        None => false,
        Some(b'5') if tail.len() == 1 => (head.as_bytes()[n - 1] - b'0') % 2 == 1,
        Some(&d) => d >= b'5',
    };
    let mut digits = head.as_bytes().to_vec();
    let mut exponent = exponent;
    if round_up {
        let mut i = n;
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                digits.pop();
                exponent += 1;
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let digits = String::from_utf8(digits).unwrap();
    let trimmed = digits.trim_end_matches('0');
    let digits = if trimmed.is_empty() { "0" } else { trimmed };
    (digits.to_string(), exponent + 1)
}

fn python_repr_f64(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf".to_string()
        } else {
            "inf".to_string()
        };
    }
    let negative = value.is_sign_negative();
    let (digits, decimal_point) = shortest_digits(value.abs());
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if !(-3..=16).contains(&decimal_point) {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let exponent = decimal_point - 1;
        out.push(if exponent < 0 { '-' } else { '+' });
        let exponent = exponent.unsigned_abs();
        if exponent < 10 {
            out.push('0');
        }
        let _ = write!(out, "{exponent}");
    } else if decimal_point <= 0 {
        out.push_str("0.");
        for _ in 0..(-decimal_point) {
            out.push('0');
        }
        out.push_str(&digits);
    } else if decimal_point as usize >= digits.len() {
        out.push_str(&digits);
        for _ in 0..(decimal_point as usize - digits.len()) {
            out.push('0');
        }
        out.push_str(".0");
    } else {
        out.push_str(&digits[..decimal_point as usize]);
        out.push('.');
        out.push_str(&digits[decimal_point as usize..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::dump_module;
    use crate::ast::*;
    use crate::intern::Interner;

    /// (col_offset, end_col_offset) on a single line.
    type Cols = (i32, i32);

    fn name(interner: &mut Interner, id: &str, ctx: ExprContext, line: i32, cols: Cols) -> Expr {
        Expr::Name {
            id: interner.intern(id),
            ctx: Box::new(ctx),
            lineno: line,
            col_offset: cols.0,
            end_lineno: Some(line),
            end_col_offset: Some(cols.1),
        }
    }

    fn constant(value: Constant, line: i32, cols: Cols) -> Expr {
        Expr::Constant {
            value,
            kind: None,
            lineno: line,
            col_offset: cols.0,
            end_lineno: Some(line),
            end_col_offset: Some(cols.1),
        }
    }

    fn assign(target: Expr, value: Expr, line: i32, cols: Cols) -> Stmt {
        Stmt::Assign {
            targets: vec![target],
            value: Box::new(value),
            type_comment: None,
            lineno: line,
            col_offset: cols.0,
            end_lineno: Some(line),
            end_col_offset: Some(cols.1),
        }
    }

    fn module(body: Vec<Stmt>) -> Mod {
        Mod::Module {
            body,
            type_ignores: Vec::new(),
        }
    }

    fn check(module: &Mod, interner: &Interner, expected: &str) {
        assert_eq!(dump_module(module, interner), expected);
    }

    /// The AST for `x = <value>` on line 1.
    fn x_constant(interner: &mut Interner, value: Constant, end_col: i32) -> Mod {
        let target = name(interner, "x", ExprContext::Store, 1, (0, 1));
        module(vec![assign(
            target,
            constant(value, 1, (4, end_col)),
            1,
            (0, end_col),
        )])
    }

    // Oracle output for: x = 'hi'
    #[test]
    fn constant_str() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::Str(vec![104, 105]), 8);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"str","v":[104,105]},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":8},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":8}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = 'aéb\nc'  (col offsets are UTF-8 byte offsets)
    #[test]
    fn constant_str_with_escapes() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::Str(vec![97, 233, 98, 10, 99]), 13);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"str","v":[97,233,98,10,99]},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":13},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":13}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = '\ud800\U0001F600'  (lone surrogate and astral code point)
    #[test]
    fn constant_str_with_surrogates() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::Str(vec![0xD800, 0x1F600]), 22);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"str","v":[55296,128512]},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":22},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":22}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = b'\x00\xff'
    #[test]
    fn constant_bytes() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::Bytes(vec![0x00, 0xFF]), 15);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"bytes","v":"00ff"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":15},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":15}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = 12
    #[test]
    fn constant_int() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::Int("12".to_string()), 6);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"int","v":"12"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":6},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":6}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = 1000000000000000000000000000000
    #[test]
    fn constant_int_beyond_64_bits() {
        let mut interner = Interner::new();
        let module = x_constant(
            &mut interner,
            Constant::Int("1000000000000000000000000000000".to_string()),
            35,
        );
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"int","v":"1000000000000000000000000000000"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":35},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":35}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = 1.5 / y = 1e16 / z = 1e-5 / w = 1e400 (a literal inf)
    #[test]
    fn constant_floats() {
        let mut interner = Interner::new();
        let module = module(vec![
            assign(
                name(&mut interner, "x", ExprContext::Store, 1, (0, 1)),
                constant(Constant::Float(1.5), 1, (4, 7)),
                1,
                (0, 7),
            ),
            assign(
                name(&mut interner, "y", ExprContext::Store, 2, (0, 1)),
                constant(Constant::Float(1e16), 2, (4, 8)),
                2,
                (0, 8),
            ),
            assign(
                name(&mut interner, "z", ExprContext::Store, 3, (0, 1)),
                constant(Constant::Float(1e-5), 3, (4, 8)),
                3,
                (0, 8),
            ),
            assign(
                name(&mut interner, "w", ExprContext::Store, 4, (0, 1)),
                constant(Constant::Float(f64::INFINITY), 4, (4, 9)),
                4,
                (0, 9),
            ),
        ]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"float","v":"1.5"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":7},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":7},{"_type":"Assign","targets":[{"_type":"Name","id":"y","ctx":{"_type":"Store"},"lineno":2,"col_offset":0,"end_lineno":2,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"float","v":"1e+16"},"kind":null,"lineno":2,"col_offset":4,"end_lineno":2,"end_col_offset":8},"type_comment":null,"lineno":2,"col_offset":0,"end_lineno":2,"end_col_offset":8},{"_type":"Assign","targets":[{"_type":"Name","id":"z","ctx":{"_type":"Store"},"lineno":3,"col_offset":0,"end_lineno":3,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"float","v":"1e-05"},"kind":null,"lineno":3,"col_offset":4,"end_lineno":3,"end_col_offset":8},"type_comment":null,"lineno":3,"col_offset":0,"end_lineno":3,"end_col_offset":8},{"_type":"Assign","targets":[{"_type":"Name","id":"w","ctx":{"_type":"Store"},"lineno":4,"col_offset":0,"end_lineno":4,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"float","v":"inf"},"kind":null,"lineno":4,"col_offset":4,"end_lineno":4,"end_col_offset":9},"type_comment":null,"lineno":4,"col_offset":0,"end_lineno":4,"end_col_offset":9}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = 2+3j
    #[test]
    fn constant_complex() {
        let mut interner = Interner::new();
        let value = Expr::BinOp {
            left: Box::new(constant(Constant::Int("2".to_string()), 1, (4, 5))),
            op: Box::new(Operator::Add),
            right: Box::new(constant(
                Constant::Complex {
                    real: 0.0,
                    imag: 3.0,
                },
                1,
                (6, 8),
            )),
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(8),
        };
        let target = name(&mut interner, "x", ExprContext::Store, 1, (0, 1));
        let module = module(vec![assign(target, value, 1, (0, 8))]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"BinOp","left":{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":5},"op":{"_type":"Add"},"right":{"_type":"Constant","value":{"const":"complex","real":"0.0","imag":"3.0"},"kind":null,"lineno":1,"col_offset":6,"end_lineno":1,"end_col_offset":8},"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":8},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":8}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = True / y = False
    #[test]
    fn constant_bools() {
        let mut interner = Interner::new();
        let module = module(vec![
            assign(
                name(&mut interner, "x", ExprContext::Store, 1, (0, 1)),
                constant(Constant::Bool(true), 1, (4, 8)),
                1,
                (0, 8),
            ),
            assign(
                name(&mut interner, "y", ExprContext::Store, 2, (0, 1)),
                constant(Constant::Bool(false), 2, (4, 9)),
                2,
                (0, 9),
            ),
        ]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"bool","v":true},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":8},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":8},{"_type":"Assign","targets":[{"_type":"Name","id":"y","ctx":{"_type":"Store"},"lineno":2,"col_offset":0,"end_lineno":2,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"bool","v":false},"kind":null,"lineno":2,"col_offset":4,"end_lineno":2,"end_col_offset":9},"type_comment":null,"lineno":2,"col_offset":0,"end_lineno":2,"end_col_offset":9}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = None
    #[test]
    fn constant_none() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::None, 8);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"None"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":8},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":8}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = ...
    #[test]
    fn constant_ellipsis() {
        let mut interner = Interner::new();
        let module = x_constant(&mut interner, Constant::Ellipsis, 7);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Constant","value":{"const":"Ellipsis"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":7},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":7}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: pass
    #[test]
    fn pass_statement() {
        let interner = Interner::new();
        let module = module(vec![Stmt::Pass {
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(1),
            end_col_offset: Some(4),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Pass","lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":4}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: for i in range(3): / pass / else: / pass
    #[test]
    fn for_statement() {
        let mut interner = Interner::new();
        let iter = Expr::Call {
            func: Box::new(name(&mut interner, "range", ExprContext::Load, 1, (9, 14))),
            args: vec![constant(Constant::Int("3".to_string()), 1, (15, 16))],
            keywords: Vec::new(),
            lineno: 1,
            col_offset: 9,
            end_lineno: Some(1),
            end_col_offset: Some(17),
        };
        let module = module(vec![Stmt::For {
            target: Box::new(name(&mut interner, "i", ExprContext::Store, 1, (4, 5))),
            iter: Box::new(iter),
            body: vec![Stmt::Pass {
                lineno: 2,
                col_offset: 4,
                end_lineno: Some(2),
                end_col_offset: Some(8),
            }],
            orelse: vec![Stmt::Pass {
                lineno: 4,
                col_offset: 4,
                end_lineno: Some(4),
                end_col_offset: Some(8),
            }],
            type_comment: None,
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(4),
            end_col_offset: Some(8),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"For","target":{"_type":"Name","id":"i","ctx":{"_type":"Store"},"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":5},"iter":{"_type":"Call","func":{"_type":"Name","id":"range","ctx":{"_type":"Load"},"lineno":1,"col_offset":9,"end_lineno":1,"end_col_offset":14},"args":[{"_type":"Constant","value":{"const":"int","v":"3"},"kind":null,"lineno":1,"col_offset":15,"end_lineno":1,"end_col_offset":16}],"keywords":[],"lineno":1,"col_offset":9,"end_lineno":1,"end_col_offset":17},"body":[{"_type":"Pass","lineno":2,"col_offset":4,"end_lineno":2,"end_col_offset":8}],"orelse":[{"_type":"Pass","lineno":4,"col_offset":4,"end_lineno":4,"end_col_offset":8}],"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":4,"end_col_offset":8}],"type_ignores":[]}"#,
        );
    }

    fn arg(interner: &mut Interner, id: &str, line: i32, cols: Cols) -> Arg {
        Arg {
            arg: interner.intern(id),
            annotation: None,
            type_comment: None,
            lineno: line,
            col_offset: cols.0,
            end_lineno: Some(line),
            end_col_offset: Some(cols.1),
        }
    }

    // Oracle output for:
    //   @deco
    //   def f(a, b=1, *args, c: int = 2, **kw) -> str:
    //       return a
    #[test]
    fn function_def_statement() {
        let mut interner = Interner::new();
        let arguments = Arguments {
            posonlyargs: Vec::new(),
            args: vec![
                arg(&mut interner, "a", 2, (6, 7)),
                arg(&mut interner, "b", 2, (9, 10)),
            ],
            vararg: Some(Box::new(arg(&mut interner, "args", 2, (15, 19)))),
            kwonlyargs: vec![Arg {
                annotation: Some(Box::new(name(
                    &mut interner,
                    "int",
                    ExprContext::Load,
                    2,
                    (24, 27),
                ))),
                ..arg(&mut interner, "c", 2, (21, 27))
            }],
            kw_defaults: vec![Some(constant(Constant::Int("2".to_string()), 2, (30, 31)))],
            kwarg: Some(Box::new(arg(&mut interner, "kw", 2, (35, 37)))),
            defaults: vec![constant(Constant::Int("1".to_string()), 2, (11, 12))],
        };
        let module = module(vec![Stmt::FunctionDef {
            name: interner.intern("f"),
            args: Box::new(arguments),
            body: vec![Stmt::Return {
                value: Some(Box::new(name(
                    &mut interner,
                    "a",
                    ExprContext::Load,
                    3,
                    (11, 12),
                ))),
                lineno: 3,
                col_offset: 4,
                end_lineno: Some(3),
                end_col_offset: Some(12),
            }],
            decorator_list: vec![name(&mut interner, "deco", ExprContext::Load, 1, (1, 5))],
            returns: Some(Box::new(name(
                &mut interner,
                "str",
                ExprContext::Load,
                2,
                (42, 45),
            ))),
            type_comment: None,
            type_params: Vec::new(),
            lineno: 2,
            col_offset: 0,
            end_lineno: Some(3),
            end_col_offset: Some(12),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"FunctionDef","name":"f","args":{"_type":"arguments","posonlyargs":[],"args":[{"_type":"arg","arg":"a","annotation":null,"type_comment":null,"lineno":2,"col_offset":6,"end_lineno":2,"end_col_offset":7},{"_type":"arg","arg":"b","annotation":null,"type_comment":null,"lineno":2,"col_offset":9,"end_lineno":2,"end_col_offset":10}],"vararg":{"_type":"arg","arg":"args","annotation":null,"type_comment":null,"lineno":2,"col_offset":15,"end_lineno":2,"end_col_offset":19},"kwonlyargs":[{"_type":"arg","arg":"c","annotation":{"_type":"Name","id":"int","ctx":{"_type":"Load"},"lineno":2,"col_offset":24,"end_lineno":2,"end_col_offset":27},"type_comment":null,"lineno":2,"col_offset":21,"end_lineno":2,"end_col_offset":27}],"kw_defaults":[{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":2,"col_offset":30,"end_lineno":2,"end_col_offset":31}],"kwarg":{"_type":"arg","arg":"kw","annotation":null,"type_comment":null,"lineno":2,"col_offset":35,"end_lineno":2,"end_col_offset":37},"defaults":[{"_type":"Constant","value":{"const":"int","v":"1"},"kind":null,"lineno":2,"col_offset":11,"end_lineno":2,"end_col_offset":12}]},"body":[{"_type":"Return","value":{"_type":"Name","id":"a","ctx":{"_type":"Load"},"lineno":3,"col_offset":11,"end_lineno":3,"end_col_offset":12},"lineno":3,"col_offset":4,"end_lineno":3,"end_col_offset":12}],"decorator_list":[{"_type":"Name","id":"deco","ctx":{"_type":"Load"},"lineno":1,"col_offset":1,"end_lineno":1,"end_col_offset":5}],"returns":{"_type":"Name","id":"str","ctx":{"_type":"Load"},"lineno":2,"col_offset":42,"end_lineno":2,"end_col_offset":45},"type_comment":null,"type_params":[],"lineno":2,"col_offset":0,"end_lineno":3,"end_col_offset":12}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: def g(*, a, b=2): / pass  (kw_defaults holds a null)
    #[test]
    fn function_def_null_kw_default() {
        let mut interner = Interner::new();
        let arguments = Arguments {
            posonlyargs: Vec::new(),
            args: Vec::new(),
            vararg: None,
            kwonlyargs: vec![
                arg(&mut interner, "a", 1, (9, 10)),
                arg(&mut interner, "b", 1, (12, 13)),
            ],
            kw_defaults: vec![
                None,
                Some(constant(Constant::Int("2".to_string()), 1, (14, 15))),
            ],
            kwarg: None,
            defaults: Vec::new(),
        };
        let module = module(vec![Stmt::FunctionDef {
            name: interner.intern("g"),
            args: Box::new(arguments),
            body: vec![Stmt::Pass {
                lineno: 2,
                col_offset: 4,
                end_lineno: Some(2),
                end_col_offset: Some(8),
            }],
            decorator_list: Vec::new(),
            returns: None,
            type_comment: None,
            type_params: Vec::new(),
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(2),
            end_col_offset: Some(8),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"FunctionDef","name":"g","args":{"_type":"arguments","posonlyargs":[],"args":[],"vararg":null,"kwonlyargs":[{"_type":"arg","arg":"a","annotation":null,"type_comment":null,"lineno":1,"col_offset":9,"end_lineno":1,"end_col_offset":10},{"_type":"arg","arg":"b","annotation":null,"type_comment":null,"lineno":1,"col_offset":12,"end_lineno":1,"end_col_offset":13}],"kw_defaults":[null,{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":1,"col_offset":14,"end_lineno":1,"end_col_offset":15}],"kwarg":null,"defaults":[]},"body":[{"_type":"Pass","lineno":2,"col_offset":4,"end_lineno":2,"end_col_offset":8}],"decorator_list":[],"returns":null,"type_comment":null,"type_params":[],"lineno":1,"col_offset":0,"end_lineno":2,"end_col_offset":8}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: import os.path as osp / from .. import x as y, z
    #[test]
    fn import_statements() {
        let mut interner = Interner::new();
        let import = Stmt::Import {
            names: vec![Alias {
                name: interner.intern("os.path"),
                asname: Some(interner.intern("osp")),
                lineno: 1,
                col_offset: 7,
                end_lineno: Some(1),
                end_col_offset: Some(21),
            }],
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(1),
            end_col_offset: Some(21),
        };
        let import_from = Stmt::ImportFrom {
            module: None,
            names: vec![
                Alias {
                    name: interner.intern("x"),
                    asname: Some(interner.intern("y")),
                    lineno: 2,
                    col_offset: 15,
                    end_lineno: Some(2),
                    end_col_offset: Some(21),
                },
                Alias {
                    name: interner.intern("z"),
                    asname: None,
                    lineno: 2,
                    col_offset: 23,
                    end_lineno: Some(2),
                    end_col_offset: Some(24),
                },
            ],
            level: Some(2),
            lineno: 2,
            col_offset: 0,
            end_lineno: Some(2),
            end_col_offset: Some(24),
        };
        let module = module(vec![import, import_from]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Import","names":[{"_type":"alias","name":"os.path","asname":"osp","lineno":1,"col_offset":7,"end_lineno":1,"end_col_offset":21}],"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":21},{"_type":"ImportFrom","module":null,"names":[{"_type":"alias","name":"x","asname":"y","lineno":2,"col_offset":15,"end_lineno":2,"end_col_offset":21},{"_type":"alias","name":"z","asname":null,"lineno":2,"col_offset":23,"end_lineno":2,"end_col_offset":24}],"level":2,"lineno":2,"col_offset":0,"end_lineno":2,"end_col_offset":24}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: try: / pass / except ValueError as e: / pass
    #[test]
    fn try_statement() {
        let mut interner = Interner::new();
        let pass = |line: i32| Stmt::Pass {
            lineno: line,
            col_offset: 4,
            end_lineno: Some(line),
            end_col_offset: Some(8),
        };
        let handler = Excepthandler::ExceptHandler {
            r#type: Some(Box::new(name(
                &mut interner,
                "ValueError",
                ExprContext::Load,
                3,
                (7, 17),
            ))),
            name: Some(interner.intern("e")),
            body: vec![pass(4)],
            lineno: 3,
            col_offset: 0,
            end_lineno: Some(4),
            end_col_offset: Some(8),
        };
        let module = module(vec![Stmt::Try {
            body: vec![pass(2)],
            handlers: vec![handler],
            orelse: Vec::new(),
            finalbody: Vec::new(),
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(4),
            end_col_offset: Some(8),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Try","body":[{"_type":"Pass","lineno":2,"col_offset":4,"end_lineno":2,"end_col_offset":8}],"handlers":[{"_type":"ExceptHandler","type":{"_type":"Name","id":"ValueError","ctx":{"_type":"Load"},"lineno":3,"col_offset":7,"end_lineno":3,"end_col_offset":17},"name":"e","body":[{"_type":"Pass","lineno":4,"col_offset":4,"end_lineno":4,"end_col_offset":8}],"lineno":3,"col_offset":0,"end_lineno":4,"end_col_offset":8}],"orelse":[],"finalbody":[],"lineno":1,"col_offset":0,"end_lineno":4,"end_col_offset":8}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: match x: / case 1: / pass
    #[test]
    fn match_statement() {
        let mut interner = Interner::new();
        let case = MatchCase {
            pattern: Box::new(Pattern::MatchValue {
                value: Box::new(constant(Constant::Int("1".to_string()), 2, (9, 10))),
                lineno: 2,
                col_offset: 9,
                end_lineno: 2,
                end_col_offset: 10,
            }),
            guard: None,
            body: vec![Stmt::Pass {
                lineno: 3,
                col_offset: 8,
                end_lineno: Some(3),
                end_col_offset: Some(12),
            }],
        };
        let module = module(vec![Stmt::Match {
            subject: Box::new(name(&mut interner, "x", ExprContext::Load, 1, (6, 7))),
            cases: vec![case],
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(3),
            end_col_offset: Some(12),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Match","subject":{"_type":"Name","id":"x","ctx":{"_type":"Load"},"lineno":1,"col_offset":6,"end_lineno":1,"end_col_offset":7},"cases":[{"_type":"match_case","pattern":{"_type":"MatchValue","value":{"_type":"Constant","value":{"const":"int","v":"1"},"kind":null,"lineno":2,"col_offset":9,"end_lineno":2,"end_col_offset":10},"lineno":2,"col_offset":9,"end_lineno":2,"end_col_offset":10},"guard":null,"body":[{"_type":"Pass","lineno":3,"col_offset":8,"end_lineno":3,"end_col_offset":12}]}],"lineno":1,"col_offset":0,"end_lineno":3,"end_col_offset":12}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: f(1, *a, k=2)
    #[test]
    fn call_expression() {
        let mut interner = Interner::new();
        let call = Expr::Call {
            func: Box::new(name(&mut interner, "f", ExprContext::Load, 1, (0, 1))),
            args: vec![
                constant(Constant::Int("1".to_string()), 1, (2, 3)),
                Expr::Starred {
                    value: Box::new(name(&mut interner, "a", ExprContext::Load, 1, (6, 7))),
                    ctx: Box::new(ExprContext::Load),
                    lineno: 1,
                    col_offset: 5,
                    end_lineno: Some(1),
                    end_col_offset: Some(7),
                },
            ],
            keywords: vec![Keyword {
                arg: Some(interner.intern("k")),
                value: Box::new(constant(Constant::Int("2".to_string()), 1, (11, 12))),
                lineno: 1,
                col_offset: 9,
                end_lineno: Some(1),
                end_col_offset: Some(12),
            }],
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(1),
            end_col_offset: Some(13),
        };
        let module = module(vec![Stmt::Expr {
            value: Box::new(call),
            lineno: 1,
            col_offset: 0,
            end_lineno: Some(1),
            end_col_offset: Some(13),
        }]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Expr","value":{"_type":"Call","func":{"_type":"Name","id":"f","ctx":{"_type":"Load"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1},"args":[{"_type":"Constant","value":{"const":"int","v":"1"},"kind":null,"lineno":1,"col_offset":2,"end_lineno":1,"end_col_offset":3},{"_type":"Starred","value":{"_type":"Name","id":"a","ctx":{"_type":"Load"},"lineno":1,"col_offset":6,"end_lineno":1,"end_col_offset":7},"ctx":{"_type":"Load"},"lineno":1,"col_offset":5,"end_lineno":1,"end_col_offset":7}],"keywords":[{"_type":"keyword","arg":"k","value":{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":1,"col_offset":11,"end_lineno":1,"end_col_offset":12},"lineno":1,"col_offset":9,"end_lineno":1,"end_col_offset":12}],"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":13},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":13}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: d = {1: 2, **rest}  (a null Dict key)
    #[test]
    fn dict_display_with_unpacking() {
        let mut interner = Interner::new();
        let dict = Expr::Dict {
            keys: vec![
                Some(constant(Constant::Int("1".to_string()), 1, (5, 6))),
                None,
            ],
            values: vec![
                constant(Constant::Int("2".to_string()), 1, (8, 9)),
                name(&mut interner, "rest", ExprContext::Load, 1, (13, 17)),
            ],
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(18),
        };
        let target = name(&mut interner, "d", ExprContext::Store, 1, (0, 1));
        let module = module(vec![assign(target, dict, 1, (0, 18))]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"d","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Dict","keys":[{"_type":"Constant","value":{"const":"int","v":"1"},"kind":null,"lineno":1,"col_offset":5,"end_lineno":1,"end_col_offset":6},null],"values":[{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":1,"col_offset":8,"end_lineno":1,"end_col_offset":9},{"_type":"Name","id":"rest","ctx":{"_type":"Load"},"lineno":1,"col_offset":13,"end_lineno":1,"end_col_offset":17}],"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":18},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":18}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = 1 + 2 < 3 and True
    #[test]
    fn operators() {
        let mut interner = Interner::new();
        let binop = Expr::BinOp {
            left: Box::new(constant(Constant::Int("1".to_string()), 1, (4, 5))),
            op: Box::new(Operator::Add),
            right: Box::new(constant(Constant::Int("2".to_string()), 1, (8, 9))),
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(9),
        };
        let compare = Expr::Compare {
            left: Box::new(binop),
            ops: vec![Cmpop::Lt],
            comparators: vec![constant(Constant::Int("3".to_string()), 1, (12, 13))],
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(13),
        };
        let boolop = Expr::BoolOp {
            op: Box::new(Boolop::And),
            values: vec![compare, constant(Constant::Bool(true), 1, (18, 22))],
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(22),
        };
        let target = name(&mut interner, "x", ExprContext::Store, 1, (0, 1));
        let module = module(vec![assign(target, boolop, 1, (0, 22))]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"BoolOp","op":{"_type":"And"},"values":[{"_type":"Compare","left":{"_type":"BinOp","left":{"_type":"Constant","value":{"const":"int","v":"1"},"kind":null,"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":5},"op":{"_type":"Add"},"right":{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":1,"col_offset":8,"end_lineno":1,"end_col_offset":9},"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":9},"ops":[{"_type":"Lt"}],"comparators":[{"_type":"Constant","value":{"const":"int","v":"3"},"kind":null,"lineno":1,"col_offset":12,"end_lineno":1,"end_col_offset":13}],"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":13},{"_type":"Constant","value":{"const":"bool","v":true},"kind":null,"lineno":1,"col_offset":18,"end_lineno":1,"end_col_offset":22}],"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":22},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":22}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: y = [i for i in xs if i]
    #[test]
    fn list_comprehension() {
        let mut interner = Interner::new();
        let comprehension = Comprehension {
            target: Box::new(name(&mut interner, "i", ExprContext::Store, 1, (11, 12))),
            iter: Box::new(name(&mut interner, "xs", ExprContext::Load, 1, (16, 18))),
            ifs: vec![name(&mut interner, "i", ExprContext::Load, 1, (22, 23))],
            is_async: 0,
        };
        let listcomp = Expr::ListComp {
            elt: Box::new(name(&mut interner, "i", ExprContext::Load, 1, (5, 6))),
            generators: vec![comprehension],
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(24),
        };
        let target = name(&mut interner, "y", ExprContext::Store, 1, (0, 1));
        let module = module(vec![assign(target, listcomp, 1, (0, 24))]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"y","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"ListComp","elt":{"_type":"Name","id":"i","ctx":{"_type":"Load"},"lineno":1,"col_offset":5,"end_lineno":1,"end_col_offset":6},"generators":[{"_type":"comprehension","target":{"_type":"Name","id":"i","ctx":{"_type":"Store"},"lineno":1,"col_offset":11,"end_lineno":1,"end_col_offset":12},"iter":{"_type":"Name","id":"xs","ctx":{"_type":"Load"},"lineno":1,"col_offset":16,"end_lineno":1,"end_col_offset":18},"ifs":[{"_type":"Name","id":"i","ctx":{"_type":"Load"},"lineno":1,"col_offset":22,"end_lineno":1,"end_col_offset":23}],"is_async":0}],"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":24},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":24}],"type_ignores":[]}"#,
        );
    }

    // Oracle output for: x = a.b[1:2]
    #[test]
    fn subscript_with_slice() {
        let mut interner = Interner::new();
        let attribute = Expr::Attribute {
            value: Box::new(name(&mut interner, "a", ExprContext::Load, 1, (4, 5))),
            attr: interner.intern("b"),
            ctx: Box::new(ExprContext::Load),
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(7),
        };
        let slice = Expr::Slice {
            lower: Some(Box::new(constant(
                Constant::Int("1".to_string()),
                1,
                (8, 9),
            ))),
            upper: Some(Box::new(constant(
                Constant::Int("2".to_string()),
                1,
                (10, 11),
            ))),
            step: None,
            lineno: 1,
            col_offset: 8,
            end_lineno: Some(1),
            end_col_offset: Some(11),
        };
        let subscript = Expr::Subscript {
            value: Box::new(attribute),
            slice: Box::new(slice),
            ctx: Box::new(ExprContext::Load),
            lineno: 1,
            col_offset: 4,
            end_lineno: Some(1),
            end_col_offset: Some(12),
        };
        let target = name(&mut interner, "x", ExprContext::Store, 1, (0, 1));
        let module = module(vec![assign(target, subscript, 1, (0, 12))]);
        check(
            &module,
            &interner,
            r#"{"_type":"Module","body":[{"_type":"Assign","targets":[{"_type":"Name","id":"x","ctx":{"_type":"Store"},"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":1}],"value":{"_type":"Subscript","value":{"_type":"Attribute","value":{"_type":"Name","id":"a","ctx":{"_type":"Load"},"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":5},"attr":"b","ctx":{"_type":"Load"},"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":7},"slice":{"_type":"Slice","lower":{"_type":"Constant","value":{"const":"int","v":"1"},"kind":null,"lineno":1,"col_offset":8,"end_lineno":1,"end_col_offset":9},"upper":{"_type":"Constant","value":{"const":"int","v":"2"},"kind":null,"lineno":1,"col_offset":10,"end_lineno":1,"end_col_offset":11},"step":null,"lineno":1,"col_offset":8,"end_lineno":1,"end_col_offset":11},"ctx":{"_type":"Load"},"lineno":1,"col_offset":4,"end_lineno":1,"end_col_offset":12},"type_comment":null,"lineno":1,"col_offset":0,"end_lineno":1,"end_col_offset":12}],"type_ignores":[]}"#,
        );
    }

    // repr(f64) values cross-checked against CPython 3.12.10.
    #[test]
    fn float_repr_matches_cpython() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.5, "1.5"),
            (0.1, "0.1"),
            (2.0 / 3.0, "0.6666666666666666"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (1.2345678901234568e18, "1.2345678901234568e+18"),
            (0.0001, "0.0001"),
            (1e-5, "1e-05"),
            (1e300, "1e+300"),
            (1e-300, "1e-300"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
            // The exact expansion is 9.99...e22; rounding carries the exponent.
            (1e23, "1e+23"),
        ];
        for &(value, expected) in cases {
            assert_eq!(super::python_repr_f64(value), expected);
        }
        // Exact decimal ties round half to even. Parsed from text: as literals
        // clippy would shorten them to look like different numbers.
        for (text, expected) in [
            ("-185262743923125.125", "-185262743923125.12"),
            ("211947665204525.125", "211947665204525.12"),
            ("-718075613223142.25", "-718075613223142.2"),
        ] {
            assert_eq!(super::python_repr_f64(text.parse().unwrap()), expected);
        }
    }
}

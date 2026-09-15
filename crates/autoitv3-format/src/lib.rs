//! Pretty-printer that renders an AST back to AutoIt source text.
//!
//! This is the foundation for deobfuscation: a *normalized* re-print that
//! strips comments, normalizes whitespace, and (in later passes) substitutes
//! constants. Structure is preserved faithfully so the output remains valid.

/// Arrays longer than this are printed a few entries per line, with `_`
/// continuations, so a deobfuscated table does not become one enormous line.
const WRAP_ARRAY_AFTER: usize = 24;
const ARRAY_ITEMS_PER_LINE: usize = 8;

use std::fmt::Write;
use autoitv3_ast::ast::*;

pub struct PrettyPrinter {
    out: String,
    indent: usize,
    /// When true, `;` comments are dropped from the output (for deobfuscation).
    /// Defaults to false so comments are preserved.
    strip_comments: bool,
}

impl PrettyPrinter {
    pub fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
            strip_comments: false,
        }
    }

    /// Builder: configure whether `;` comments are stripped. Default keeps them.
    pub fn strip_comments(mut self, strip: bool) -> Self {
        self.strip_comments = strip;
        self
    }

    pub fn print_program(&mut self, prog: &Program) -> String {
        self.out.clear();
        self.indent = 0;
        let mut ci = 0; // cursor into prog.comments
        for item in &prog.items {
            self.emit_comments_until(prog, &mut ci, item.span.start.line);
            self.print_item(item);
        }
        // Any comments after the last item.
        self.emit_comments_until(prog, &mut ci, u32::MAX);
        self.out.clone()
    }

    /// Emit (or skip, when stripping) comments whose start line is <= `line`.
    fn emit_comments_until(&mut self, prog: &Program, ci: &mut usize, line: u32) {
        while *ci < prog.comments.len() {
            let c = &prog.comments[*ci];
            if c.span.start.line > line {
                break;
            }
            if !self.strip_comments {
                if c.block {
                    // A `#cs ... #ce` block already carries its own delimiters
                    // and layout, so it is reproduced verbatim rather than
                    // re-spelled as `;` line comments.
                    let _ = writeln!(self.out, "{}", c.text);
                } else {
                    self.pad();
                    let _ = writeln!(self.out, ";{}", c.text);
                }
            }
            *ci += 1;
        }
    }

    fn pad(&mut self) {
        for _ in 0..self.indent {
            self.out.push_str("    ");
        }
    }

    fn nl(&mut self) {
        self.out.push('\n');
    }

    fn print_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Directive(name) => {
                self.pad();
                let _ = writeln!(self.out, "#{name}");
            }
            ItemKind::Func(f) => self.print_func(f),
            ItemKind::Stmt(s) => {
                self.print_stmt(s);
                self.nl();
            }
            ItemKind::Region(r) => {
                for it in &r.items {
                    self.print_item(it);
                }
            }
        }
    }

    fn print_func(&mut self, f: &FuncDef) {
        self.pad();
        if f.is_volatile {
            let _ = write!(self.out, "Volatile ");
        }
        let _ = write!(self.out, "Func {}", f.name.name);
        // The parameter list is always printed, even when it is empty: AutoIt
        // requires `Func Foo()`, so dropping the `()` would produce source a
        // real interpreter rejects. (The *parser* is lenient and accepts a
        // missing list; the printer is not.)
        let _ = write!(self.out, "(");
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                let _ = write!(self.out, ", ");
            }
            if p.by_ref {
                let _ = write!(self.out, "ByRef ");
            }
            let _ = write!(self.out, "{}", p.name.name);
            if let Some(d) = &p.default {
                let _ = write!(self.out, " = ");
                self.print_expr(d);
            }
        }
        let _ = write!(self.out, ")");
        self.nl();
        self.indent += 1;
        for s in &f.body {
            self.print_stmt(s);
            self.nl();
        }
        self.indent -= 1;
        self.pad();
        let _ = writeln!(self.out, "EndFunc");
    }

    fn print_stmt(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Directive(name) => {
                self.pad();
                let _ = writeln!(self.out, "#{name}");
            }
            StmtKind::VarDecl(v) => {
                self.pad();
                let kw = match v.kind {
                    VarKind::Local => "Local",
                    VarKind::Global => "Global",
                    VarKind::Dim => "Dim",
                    VarKind::Static => "Static",
                };
                if v.is_redim {
                    let _ = write!(self.out, "ReDim");
                } else if v.is_enum && v.kind == VarKind::Local {
                    // A bare `Enum` is local by default; printing `Local Enum`
                    // would be noise (and `Global Enum` must keep `Global`).
                } else {
                    let _ = write!(self.out, "{kw}");
                }
                if v.is_const && !v.is_enum && !v.is_redim {
                    let _ = write!(self.out, " Const");
                }
                if v.is_enum {
                    let _ = write!(self.out, " Enum");
                }
                if let Some(step) = &v.enum_step {
                    let _ = write!(self.out, " Step ");
                    self.print_expr(step);
                }
                for (i, item) in v.vars.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(self.out, ",");
                    }
                    let _ = write!(self.out, " {}", item.name.name);
                    for d in &item.dims {
                        let _ = write!(self.out, "[");
                        // `$a[]` (empty brackets) is recorded by the parser as
                        // a `Null` placeholder dimension. Printing it would
                        // turn `Local $a[] = [...]` into `Local $a[Null]`,
                        // silently changing the declaration's meaning.
                        if !is_empty_dim(d) {
                            self.print_expr(d);
                        }
                        let _ = write!(self.out, "]");
                    }
                    if let Some(init) = &item.init {
                        let _ = write!(self.out, " = ");
                        self.print_expr(init);
                    }
                }
            }
            StmtKind::Expr(e) => {
                self.pad();
                self.print_expr(e);
            }
            StmtKind::Return(e) => {
                self.pad();
                let _ = write!(self.out, "Return");
                if let Some(e) = e {
                    let _ = write!(self.out, " ");
                    self.print_expr(e);
                }
            }
            StmtKind::Exit(e) => {
                self.pad();
                let _ = write!(self.out, "Exit");
                if let Some(e) = e {
                    let _ = write!(self.out, " ");
                    self.print_expr(e);
                }
            }
            StmtKind::ExitLoop(e) => {
                self.pad();
                let _ = write!(self.out, "ExitLoop");
                if let Some(e) = e {
                    let _ = write!(self.out, " ");
                    self.print_expr(e);
                }
            }
            StmtKind::ContinueLoop(e) => {
                self.pad();
                let _ = write!(self.out, "ContinueLoop");
                if let Some(e) = e {
                    let _ = write!(self.out, " ");
                    self.print_expr(e);
                }
            }
            StmtKind::ContinueCase => {
                self.pad();
                let _ = write!(self.out, "ContinueCase");
            }
            StmtKind::If(if_) => {
                let is_single = if_.then_stmt.is_some()
                    && if_.then_block.is_empty()
                    && if_.else_ifs.is_empty()
                    && if_.else_block.is_empty();
                self.pad();
                let _ = write!(self.out, "If ");
                self.print_expr(&if_.cond);
                let _ = write!(self.out, " Then");
                if let Some(ts) = &if_.then_stmt {
                    let _ = write!(self.out, " ");
                    self.print_stmt_inline(ts);
                }
                if is_single {
                    // `If cond Then stmt` — no EndIf required.
                    return;
                }
                self.nl();
                self.indent += 1;
                for b in &if_.then_block {
                    self.print_stmt(b);
                    self.nl();
                }
                self.indent -= 1;
                for (c, body) in &if_.else_ifs {
                    self.pad();
                    let _ = write!(self.out, "ElseIf ");
                    self.print_expr(c);
                    let _ = write!(self.out, " Then");
                    self.nl();
                    self.indent += 1;
                    for b in body {
                        self.print_stmt(b);
                        self.nl();
                    }
                    self.indent -= 1;
                }
                // The `Else` keyword is easy to forget here, and forgetting it
                // silently moves the `Else` body into the `Then` branch — the
                // output still parses, it just means something else.
                if !if_.else_block.is_empty() {
                    self.pad();
                    let _ = write!(self.out, "Else");
                    self.nl();
                    self.indent += 1;
                    for b in &if_.else_block {
                        self.print_stmt(b);
                        self.nl();
                    }
                    self.indent -= 1;
                }
                self.pad();
                let _ = write!(self.out, "EndIf");
            }
            StmtKind::While(w) => {
                self.pad();
                let _ = write!(self.out, "While ");
                self.print_expr(&w.cond);
                self.nl();
                self.indent += 1;
                for b in &w.body {
                    self.print_stmt(b);
                    self.nl();
                }
                self.indent -= 1;
                self.pad();
                let _ = write!(self.out, "WEnd");
            }
            StmtKind::DoUntil(d) => {
                self.pad();
                let _ = write!(self.out, "Do");
                self.nl();
                self.indent += 1;
                for b in &d.body {
                    self.print_stmt(b);
                    self.nl();
                }
                self.indent -= 1;
                self.pad();
                let _ = write!(self.out, "Until ");
                self.print_expr(&d.cond);
            }
            StmtKind::For(f) => {
                self.pad();
                if let Some(iter) = &f.iter {
                    let _ = write!(self.out, "For {} In ", f.var.name);
                    self.print_expr(iter);
                } else {
                    let _ = write!(self.out, "For {} = ", f.var.name);
                    self.print_expr(&f.from);
                    let _ = write!(self.out, " To ");
                    self.print_expr(&f.to);
                    if let Some(st) = &f.step {
                        let _ = write!(self.out, " Step ");
                        self.print_expr(st);
                    }
                }
                self.nl();
                self.indent += 1;
                for b in &f.body {
                    self.print_stmt(b);
                    self.nl();
                }
                self.indent -= 1;
                self.pad();
                let _ = write!(self.out, "Next");
            }
            StmtKind::Select(cases) => {
                self.pad();
                let _ = write!(self.out, "Select");
                self.nl();
                self.indent += 1;
                for c in cases {
                    self.print_case(c);
                }
                self.indent -= 1;
                self.pad();
                let _ = write!(self.out, "EndSelect");
            }
            StmtKind::Switch(sw) => {
                self.pad();
                let _ = write!(self.out, "Switch ");
                self.print_expr(&sw.expr);
                self.nl();
                self.indent += 1;
                for c in &sw.cases {
                    self.print_case(c);
                }
                self.indent -= 1;
                self.pad();
                let _ = write!(self.out, "EndSwitch");
            }
            StmtKind::With(w) => {
                self.pad();
                let _ = write!(self.out, "With ");
                self.print_expr(&w.expr);
                self.nl();
                self.indent += 1;
                for b in &w.body {
                    self.print_stmt(b);
                    self.nl();
                }
                self.indent -= 1;
                self.pad();
                let _ = write!(self.out, "EndWith");
            }
        }
    }

    fn print_case(&mut self, c: &CaseClause) {
        self.pad();
        let _ = write!(self.out, "Case ");
        if c.is_else {
            let _ = write!(self.out, "Else");
        } else {
            for (i, v) in c.values.iter().enumerate() {
                if i > 0 {
                    let _ = write!(self.out, ", ");
                }
                self.print_expr(v);
            }
        }
        self.nl();
        self.indent += 1;
        for b in &c.body {
            self.print_stmt(b);
            self.nl();
        }
        self.indent -= 1;
    }

    /// Print a statement without indentation or trailing newline, for
    /// single-line `If ... Then stmt`.
    fn print_stmt_inline(&mut self, s: &Stmt) {
        let mut tmp = Self::new();
        tmp.indent = 0;
        match &s.kind {
            StmtKind::Expr(e) => tmp.print_expr(e),
            StmtKind::VarDecl(v) => {
                let kw = match v.kind {
                    VarKind::Local => "Local",
                    VarKind::Global => "Global",
                    VarKind::Dim => "Dim",
                    VarKind::Static => "Static",
                };
                let _ = write!(tmp.out, "{kw}");
                for (i, item) in v.vars.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(tmp.out, ",");
                    }
                    let _ = write!(tmp.out, " {}", item.name.name);
                    for d in &item.dims {
                        let _ = write!(tmp.out, "[");
                        if !is_empty_dim(d) {
                            tmp.print_expr(d);
                        }
                        let _ = write!(tmp.out, "]");
                    }
                    if let Some(init) = &item.init {
                        let _ = write!(tmp.out, " = ");
                        tmp.print_expr(init);
                    }
                }
            }
            StmtKind::Return(e) => {
                let _ = write!(tmp.out, "Return");
                if let Some(e) = e {
                    let _ = write!(tmp.out, " ");
                    tmp.print_expr(e);
                }
            }
            StmtKind::Exit(e) => {
                let _ = write!(tmp.out, "Exit");
                if let Some(e) = e {
                    let _ = write!(tmp.out, " ");
                    tmp.print_expr(e);
                }
            }
            StmtKind::ExitLoop(e) => {
                let _ = write!(tmp.out, "ExitLoop");
                if let Some(e) = e {
                    let _ = write!(tmp.out, " ");
                    tmp.print_expr(e);
                }
            }
            StmtKind::ContinueLoop(e) => {
                let _ = write!(tmp.out, "ContinueLoop");
                if let Some(e) = e {
                    let _ = write!(tmp.out, " ");
                    tmp.print_expr(e);
                }
            }
            StmtKind::ContinueCase => {
                let _ = write!(tmp.out, "ContinueCase");
            }
            _ => {
                let _ = write!(tmp.out, "[inline-stmt]");
            }
        }
        self.out.push_str(&tmp.out);
    }

    fn print_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Lit(l) => match &l.kind {
                LitKind::Int(v) => {
                    let _ = write!(self.out, "{v}");
                }
                LitKind::Float(v) => {
                    let _ = write!(self.out, "{v}");
                }
                LitKind::Str(s) => self.out.push_str(&string_literal(s)),
                LitKind::Bool(b) => {
                    let _ = write!(self.out, "{}", if *b { "True" } else { "False" });
                }
                LitKind::Default => {
                    let _ = write!(self.out, "Default");
                }
                LitKind::Null => {
                    let _ = write!(self.out, "Null");
                }
            },
            ExprKind::Var(v) => {
                let _ = write!(self.out, "{}", v.name.name);
                for idx in &v.indices {
                    let _ = write!(self.out, "[");
                    self.print_expr(idx);
                    let _ = write!(self.out, "]");
                }
            }
            ExprKind::Macro(m) => {
                let _ = write!(self.out, "{m}");
            }
            ExprKind::Ident(i) => {
                let _ = write!(self.out, "{}", i.name);
            }
            ExprKind::Call(c) => {
                let _ = write!(self.out, "{}(", c.callee.name);
                for (i, a) in c.args.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(self.out, ", ");
                    }
                    self.print_expr(a);
                }
                let _ = write!(self.out, ")");
            }
            ExprKind::Binary(op, a, b) => {
                let sym = match op {
                    BinaryOp::Assign => " = ",
                    BinaryOp::PlusAssign => " += ",
                    BinaryOp::MinusAssign => " -= ",
                    BinaryOp::StarAssign => " *= ",
                    BinaryOp::SlashAssign => " /= ",
                    BinaryOp::CaretAssign => " ^= ",
                    BinaryOp::AmpAssign => " &= ",
                    BinaryOp::Eq => " == ",
                    BinaryOp::EqLoose => " = ",
                    BinaryOp::NotEq => " <> ",
                    BinaryOp::Lt => " < ",
                    BinaryOp::Le => " <= ",
                    BinaryOp::Gt => " > ",
                    BinaryOp::Ge => " >= ",
                    BinaryOp::Add => " + ",
                    BinaryOp::Sub => " - ",
                    BinaryOp::Mul => " * ",
                    BinaryOp::Div => " / ",
                    BinaryOp::Pow => " ^ ",
                    BinaryOp::Concat => " & ",
                    BinaryOp::BitAnd => " & ",
                    BinaryOp::And => " And ",
                    BinaryOp::Or => " Or ",
                };
                let is_assign = matches!(
                    op,
                    BinaryOp::Assign
                        | BinaryOp::PlusAssign
                        | BinaryOp::MinusAssign
                        | BinaryOp::StarAssign
                        | BinaryOp::SlashAssign
                        | BinaryOp::CaretAssign
                        | BinaryOp::AmpAssign
                );
                if !is_assign {
                    let _ = write!(self.out, "(");
                }
                self.print_expr(a);
                let _ = write!(self.out, "{sym}");
                self.print_expr(b);
                if !is_assign {
                    let _ = write!(self.out, ")");
                }
            }
            ExprKind::Unary(op, a) => {
                let sym = match op {
                    UnaryOp::Not => "Not ",
                    UnaryOp::Neg => "-",
                    UnaryOp::Plus => "+",
                };
                let _ = write!(self.out, "{sym}");
                self.print_expr(a);
            }
            ExprKind::ArrayLit(items) => {
                // A deobfuscated table can run to thousands of entries; one
                // line per few entries keeps that readable (and AutoIt's `_`
                // continuation keeps it valid).
                let wrap = items.len() > WRAP_ARRAY_AFTER;
                let _ = write!(self.out, "[");
                for (i, it) in items.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(self.out, ",");
                        if wrap && i % ARRAY_ITEMS_PER_LINE == 0 {
                            let _ = write!(self.out, " _");
                            self.nl();
                            self.pad();
                            let _ = write!(self.out, "    ");
                        } else {
                            let _ = write!(self.out, " ");
                        }
                    }
                    self.print_expr(it);
                }
                let _ = write!(self.out, "]");
            }
            ExprKind::IndexCall(v, args) => {
                let _ = write!(self.out, "{}", v.name.name);
                for idx in &v.indices {
                    let _ = write!(self.out, "[");
                    self.print_expr(idx);
                    let _ = write!(self.out, "]");
                }
                let _ = write!(self.out, "(");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(self.out, ", ");
                    }
                    self.print_expr(a);
                }
                let _ = write!(self.out, ")");
            }
            ExprKind::Subscript(base, indices) => {
                // A subscript binds tighter than every operator, so a compound
                // base needs parentheses to keep meaning what it said.
                let needs_parens = matches!(
                    base.kind,
                    ExprKind::Binary(..)
                        | ExprKind::Unary(..)
                        | ExprKind::Ternary(..)
                );
                if needs_parens {
                    let _ = write!(self.out, "(");
                }
                self.print_expr(base);
                if needs_parens {
                    let _ = write!(self.out, ")");
                }
                for idx in indices {
                    let _ = write!(self.out, "[");
                    self.print_expr(idx);
                    let _ = write!(self.out, "]");
                }
            }
            ExprKind::Member(recv, name) => {
                // The implicit `With` subject prints as nothing, so a leading
                // `.Value` comes out exactly as written.
                self.print_expr(recv);
                let _ = write!(self.out, ".{}", name.name);
            }
            ExprKind::MethodCall(recv, name, args) => {
                self.print_expr(recv);
                let _ = write!(self.out, ".{}(", name.name);
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(self.out, ", ");
                    }
                    self.print_expr(a);
                }
                let _ = write!(self.out, ")");
            }
            ExprKind::WithSubject => {
                // Printed by the enclosing `Member`/`MethodCall`.
            }
            ExprKind::Ternary(c, a, b) => {
                self.print_expr(c);
                let _ = write!(self.out, " ? ");
                self.print_expr(a);
                let _ = write!(self.out, " : ");
                self.print_expr(b);
            }
            ExprKind::Paren(p) => {
                let _ = write!(self.out, "(");
                self.print_expr(p);
                let _ = write!(self.out, ")");
            }
        }
    }
}

/// Render `value` as an AutoIt string literal.
///
/// AutoIt accepts either delimiter and escapes the active one by doubling it, so
/// this picks the delimiter the value does **not** contain. Deobfuscation
/// inlines a lot of JSON and regexes, and those are mostly double quotes:
/// `'{"a": 1}'` says the same thing as `"{""a"": 1}"` while staying readable.
///
/// A value holding both kinds of quote has no choice and keeps the double-quoted
/// form, where only `"` is doubled. Both forms round-trip to the same value —
/// that is asserted in the tests.
fn string_literal(value: &str) -> String {
    if value.contains('"') && !value.contains('\'') {
        format!("'{value}'")
    } else {
        format!("\"{}\"", value.replace('"', "\"\""))
    }
}

/// True for the `[]` dimension the parser records as a `Null` placeholder.
fn is_empty_dim(e: &Expr) -> bool {
    matches!(e.kind, ExprKind::Lit(Lit { kind: LitKind::Null, .. }))
}

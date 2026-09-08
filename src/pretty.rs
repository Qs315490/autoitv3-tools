//! Pretty-printer that renders an AST back to AutoIt source text.
//!
//! This is the foundation for deobfuscation: a *normalized* re-print that
//! strips comments, normalizes whitespace, and (in later passes) substitutes
//! constants. Structure is preserved faithfully so the output remains valid.

use std::fmt::Write;
use crate::ast::*;

pub struct PrettyPrinter {
    out: String,
    indent: usize,
}

impl PrettyPrinter {
    pub fn new() -> Self {
        Self {
            out: String::new(),
            indent: 0,
        }
    }

    pub fn print_program(&mut self, prog: &Program) -> String {
        self.out.clear();
        self.indent = 0;
        for item in &prog.items {
            self.print_item(item);
        }
        self.out.clone()
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
        let _ = write!(self.out, "Func {}", f.name.name);
        if !f.params.is_empty() {
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
        }
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
            StmtKind::VarDecl(v) => {
                self.pad();
                let kw = match v.kind {
                    VarKind::Local => "Local",
                    VarKind::Global => "Global",
                    VarKind::Dim => "Dim",
                    VarKind::Static => "Static",
                };
                let _ = write!(self.out, "{kw}");
                if v.is_const {
                    let _ = write!(self.out, " Const");
                }
                for (i, item) in v.vars.iter().enumerate() {
                    if i > 0 {
                        let _ = write!(self.out, ",");
                    }
                    let _ = write!(self.out, " {}", item.name.name);
                    for d in &item.dims {
                        let _ = write!(self.out, "[");
                        self.print_expr(d);
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
            StmtKind::If(if_) => {
                self.pad();
                let _ = write!(self.out, "If ");
                self.print_expr(&if_.cond);
                let _ = write!(self.out, " Then");
                if let Some(ts) = &if_.then_stmt {
                    let _ = write!(self.out, " ");
                    self.print_stmt_inline(ts);
                }
                self.nl();
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
                self.indent += 1;
                for b in &if_.else_block {
                    self.print_stmt(b);
                    self.nl();
                }
                self.indent -= 1;
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
                let _ = write!(self.out, "For {} = ", f.var.name);
                self.print_expr(&f.from);
                let _ = write!(self.out, " To ");
                self.print_expr(&f.to);
                if let Some(st) = &f.step {
                    let _ = write!(self.out, " Step ");
                    self.print_expr(st);
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
                    if let Some(init) = &item.init {
                        let _ = write!(tmp.out, " = ");
                        tmp.print_expr(init);
                    }
                }
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
                LitKind::Str(s) => {
                    let _ = write!(self.out, "\"");
                    let escaped = s.replace('"', "\"\"");
                    let _ = write!(self.out, "{escaped}");
                    let _ = write!(self.out, "\"");
                }
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
                    BinaryOp::Eq => " == ",
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
                let _ = write!(self.out, "(");
                self.print_expr(a);
                let _ = write!(self.out, "{sym}");
                self.print_expr(b);
                let _ = write!(self.out, ")");
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
            ExprKind::Paren(p) => {
                let _ = write!(self.out, "(");
                self.print_expr(p);
                let _ = write!(self.out, ")");
            }
        }
    }
}
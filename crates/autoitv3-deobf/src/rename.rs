//! Identifier renaming pass.
//!
//! Maps every obfuscated variable/function/param/macro name to a short,
//! deterministic, readable alias (e.g. `$v000`, `$v001`, `Func$f000`). The
//! mapping is stable across runs (sorted by first appearance), which makes
//! repeated deobfuscation runs reproducible.
//!
//! This does not change behavior — it only substitutes names consistently
//! everywhere they appear, including string contents is *not* touched.

use std::collections::HashMap;
use autoitv3_ast::ast::*;

/// Deterministically rename identifiers throughout a whole program, in place.
pub fn rename_program(prog: &mut Program) -> RenameReport {
    let mut ctx = RenameCtx {
        vars: HashMap::new(),
        funcs: HashMap::new(),
        macros: HashMap::new(),
        next_var: 0,
        next_func: 0,
        next_macro: 0,
    };
    ctx.visit_items(&mut prog.items);
    RenameReport {
        vars: ctx.vars.len(),
        funcs: ctx.funcs.len(),
        macros: ctx.macros.len(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct RenameReport {
    pub vars: usize,
    pub funcs: usize,
    pub macros: usize,
}

struct RenameCtx {
    vars: HashMap<String, String>,
    funcs: HashMap<String, String>,
    macros: HashMap<String, String>,
    next_var: usize,
    next_func: usize,
    next_macro: usize,
}

impl RenameCtx {
    fn var_name(&mut self, name: &str) -> String {
        if let Some(v) = self.vars.get(name) {
            return v.clone();
        }
        let alias = format!("$v{:03}", self.next_var);
        self.next_var += 1;
        self.vars.insert(name.to_string(), alias.clone());
        alias
    }

    fn func_name(&mut self, name: &str) -> String {
        if let Some(v) = self.funcs.get(name) {
            return v.clone();
        }
        let alias = format!("f{:03}", self.next_func);
        self.next_func += 1;
        self.funcs.insert(name.to_string(), alias.clone());
        alias
    }

    fn macro_name(&mut self, name: &str) -> String {
        if let Some(v) = self.macros.get(name) {
            return v.clone();
        }
        let alias = format!("@m{:03}", self.next_macro);
        self.next_macro += 1;
        self.macros.insert(name.to_string(), alias.clone());
        alias
    }

    fn visit_items(&mut self, items: &mut Vec<Item>) {
        for item in items {
            match &mut item.kind {
                ItemKind::Func(f) => {
                    f.name.name = self.func_name(&f.name.name.clone());
                    for p in &mut f.params {
                        p.name.name = self.var_name(&p.name.name.clone());
                        if let Some(d) = &mut p.default {
                            self.visit_expr(d);
                        }
                    }
                    self.visit_stmts(&mut f.body);
                }
                ItemKind::Stmt(s) => self.visit_stmt(s),
                ItemKind::Region(r) => self.visit_items(&mut r.items),
                ItemKind::Directive(_) => {}
            }
        }
    }

    fn visit_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        for s in stmts {
            self.visit_stmt(s);
        }
    }

    fn visit_stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            StmtKind::VarDecl(v) => {
                for item in &mut v.vars {
                    item.name.name = self.var_name(&item.name.name.clone());
                    for d in &mut item.dims {
                        self.visit_expr(d);
                    }
                    if let Some(init) = &mut item.init {
                        self.visit_expr(init);
                    }
                }
            }
            StmtKind::Expr(e) => self.visit_expr(e),
            StmtKind::Return(Some(e)) | StmtKind::Exit(Some(e)) | StmtKind::ExitLoop(Some(e))
            | StmtKind::ContinueLoop(Some(e)) => {
                self.visit_expr(e);
            }
            StmtKind::If(if_) => {
                self.visit_expr(&mut if_.cond);
                if let Some(ts) = &mut if_.then_stmt {
                    self.visit_stmt(ts);
                }
                self.visit_stmts(&mut if_.then_block);
                for (c, body) in &mut if_.else_ifs {
                    self.visit_expr(c);
                    self.visit_stmts(body);
                }
                self.visit_stmts(&mut if_.else_block);
            }
            StmtKind::While(w) => {
                self.visit_expr(&mut w.cond);
                self.visit_stmts(&mut w.body);
            }
            StmtKind::DoUntil(d) => {
                self.visit_stmts(&mut d.body);
                self.visit_expr(&mut d.cond);
            }
            StmtKind::For(f) => {
                f.var.name = self.var_name(&f.var.name.clone());
                if let Some(it) = &mut f.iter {
                    self.visit_expr(it);
                }
                self.visit_expr(&mut f.from);
                self.visit_expr(&mut f.to);
                if let Some(st) = &mut f.step {
                    self.visit_expr(st);
                }
                self.visit_stmts(&mut f.body);
            }
            StmtKind::Select(cases) => {
                for c in cases {
                    self.visit_case(c);
                }
            }
            StmtKind::Switch(sw) => {
                self.visit_expr(&mut sw.expr);
                for c in &mut sw.cases {
                    self.visit_case(c);
                }
            }
            StmtKind::With(w) => {
                self.visit_expr(&mut w.expr);
                self.visit_stmts(&mut w.body);
            }
            StmtKind::Directive(_) => {}
            StmtKind::Return(None) | StmtKind::Exit(None) | StmtKind::ExitLoop(None)
            | StmtKind::ContinueLoop(None) => {}
        }
    }

    fn visit_case(&mut self, c: &mut CaseClause) {
        for v in &mut c.values {
            self.visit_expr(v);
        }
        self.visit_stmts(&mut c.body);
    }

    fn visit_expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::Lit(_) => {}
            ExprKind::Var(v) => {
                v.name.name = self.var_name(&v.name.name.clone());
                for i in &mut v.indices {
                    self.visit_expr(i);
                }
            }
            ExprKind::Macro(m) => {
                *m = self.macro_name(m);
            }
            ExprKind::Ident(id) => {
                id.name = self.func_name(&id.name.clone());
            }
            ExprKind::Call(c) => {
                c.callee.name = self.func_name(&c.callee.name.clone());
                for a in &mut c.args {
                    self.visit_expr(a);
                }
            }
            ExprKind::IndexCall(v, args) => {
                v.name.name = self.var_name(&v.name.name.clone());
                for i in &mut v.indices {
                    self.visit_expr(i);
                }
                for a in args {
                    self.visit_expr(a);
                }
            }
            ExprKind::Unary(_, a) => self.visit_expr(a),
            ExprKind::Binary(_, a, b) => {
                self.visit_expr(a);
                self.visit_expr(b);
            }
            ExprKind::Paren(p) => self.visit_expr(p),
            ExprKind::Ternary(c, a, b) => {
                self.visit_expr(c);
                self.visit_expr(a);
                self.visit_expr(b);
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    self.visit_expr(it);
                }
            }
            // COM member names are not AutoIt variables or functions, so they
            // are left exactly as written; only the receiver is visited.
            ExprKind::Member(recv, _) => self.visit_expr(recv),
            ExprKind::MethodCall(recv, _, args) => {
                self.visit_expr(recv);
                for a in args {
                    self.visit_expr(a);
                }
            }
            ExprKind::WithSubject => {}
        }
    }
}
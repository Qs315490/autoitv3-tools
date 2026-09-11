//! Constant folding pass.
//!
//! Walks the AST and replaces expressions that can be fully evaluated at
//! analysis time with their constant value. This is the core of undoing the
//! arithmetic/string obfuscation seen in obfuscated AutoIt scripts (e.g. building
//! strings by concatenating many indexed constants).
//!
//! Only *pure* constant subexpressions are folded; anything involving a
//! variable, macro, or call is left untouched (so behavior is preserved).
//!
//! The actual *evaluation* is delegated to `autoitv3-runtime`: this module only
//! walks the tree and decides where a constant can be inlined. That keeps one
//! single implementation of AutoIt's operator semantics (coercion, string
//! concatenation, integer/float promotion) instead of two that could drift.

use autoitv3_ast::ast::*;
use autoitv3_runtime::{is_constant_expr, ExecutionProfile, Runtime};

/// Fold constant expressions throughout a whole program, in place.
pub fn fold_program(prog: &mut Program) -> usize {
    let mut rt = Runtime::new();
    // Constant folding only evaluates pure expressions, but state the profile
    // anyway so the pass is explicit about wanting reproducibility.
    rt.set_profile(ExecutionProfile::deterministic());
    let mut ctx = FoldCtx { folds: 0, rt: &mut rt };
    for item in &mut prog.items {
        ctx.fold_item(item);
    }
    ctx.folds
}

/// Try to evaluate an expression to a constant literal.
///
/// Returns `Some(Expr)` (a `Lit`) when the expression is a *pure constant*
/// (see [`is_constant_expr`]) and the runtime can represent its value as a
/// literal. Everything else returns `None` and is left untouched.
pub fn try_fold(rt: &mut Runtime, e: &Expr) -> Option<Expr> {
    if !is_constant_expr(e) {
        return None;
    }
    let value = rt.eval_expr(e).ok()?;
    let kind: LitKind = value.to_lit_kind()?;
    Some(Expr {
        kind: ExprKind::Lit(Lit { kind, span: e.span }),
        span: e.span,
    })
}

struct FoldCtx<'a> {
    folds: usize,
    rt: &'a mut Runtime,
}

impl FoldCtx<'_> {
    /// Fold a single expression in place, returning true if it changed.
    fn fold_expr(&mut self, e: &mut Expr) -> bool {
        // Recurse into children first (bottom-up).
        let mut changed = false;
        match &mut e.kind {
            ExprKind::Paren(inner) => changed |= self.fold_expr(inner),
            ExprKind::Unary(_, a) => changed |= self.fold_expr(a),
            ExprKind::Binary(_, a, b) => {
                changed |= self.fold_expr(a);
                changed |= self.fold_expr(b);
            }
            ExprKind::Ternary(c, a, b) => {
                changed |= self.fold_expr(c);
                changed |= self.fold_expr(a);
                changed |= self.fold_expr(b);
            }
            ExprKind::Var(v) => {
                for i in &mut v.indices {
                    changed |= self.fold_expr(i);
                }
            }
            ExprKind::Call(c) => {
                for a in &mut c.args {
                    changed |= self.fold_expr(a);
                }
            }
            ExprKind::IndexCall(_, args) => {
                for a in args {
                    changed |= self.fold_expr(a);
                }
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    changed |= self.fold_expr(it);
                }
            }
            ExprKind::Member(recv, _) => changed |= self.fold_expr(recv),
            ExprKind::MethodCall(recv, _, args) => {
                changed |= self.fold_expr(recv);
                for a in args {
                    changed |= self.fold_expr(a);
                }
            }
            _ => {}
        }

        // Now try to collapse this node to a constant.
        if let Some(folded) = try_fold(self.rt, e) {
            *e = folded;
            changed = true;
        }
        changed
    }
}

impl FoldCtx<'_> {
    fn fold_item(&mut self, item: &mut Item) {
        match &mut item.kind {
            ItemKind::Func(f) => {
                self.fold_params(&mut f.params);
                self.fold_stmts(&mut f.body);
            }
            ItemKind::Stmt(s) => self.fold_stmt(s),
            ItemKind::Region(r) => {
                for it in &mut r.items {
                    self.fold_item(it);
                }
            }
            ItemKind::Directive(_) => {}
        }
    }

    fn fold_params(&mut self, params: &mut [Param]) {
        for p in params {
            if let Some(d) = &mut p.default {
                self.fold_expr_ctx(d);
            }
        }
    }

    fn fold_stmts(&mut self, stmts: &mut Vec<Stmt>) {
        for s in stmts {
            self.fold_stmt(s);
        }
    }

    fn fold_stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            StmtKind::VarDecl(v) => {
                for item in &mut v.vars {
                    for d in &mut item.dims {
                        self.fold_expr_ctx(d);
                    }
                    if let Some(init) = &mut item.init {
                        self.fold_expr_ctx(init);
                    }
                }
            }
            StmtKind::Expr(e) => self.fold_expr_ctx(e),
            StmtKind::Return(Some(e)) | StmtKind::Exit(Some(e)) | StmtKind::ExitLoop(Some(e))
            | StmtKind::ContinueLoop(Some(e)) => {
                self.fold_expr_ctx(e);
            }
            StmtKind::If(if_) => {
                self.fold_expr_ctx(&mut if_.cond);
                if let Some(ts) = &mut if_.then_stmt {
                    self.fold_stmt(ts);
                }
                self.fold_stmts(&mut if_.then_block);
                for (c, body) in &mut if_.else_ifs {
                    self.fold_expr_ctx(c);
                    self.fold_stmts(body);
                }
                self.fold_stmts(&mut if_.else_block);
            }
            StmtKind::While(w) => {
                self.fold_expr_ctx(&mut w.cond);
                self.fold_stmts(&mut w.body);
            }
            StmtKind::DoUntil(d) => {
                self.fold_stmts(&mut d.body);
                self.fold_expr_ctx(&mut d.cond);
            }
            StmtKind::For(f) => {
                if let Some(it) = &mut f.iter {
                    self.fold_expr_ctx(it);
                }
                self.fold_expr_ctx(&mut f.from);
                self.fold_expr_ctx(&mut f.to);
                if let Some(st) = &mut f.step {
                    self.fold_expr_ctx(st);
                }
                self.fold_stmts(&mut f.body);
            }
            StmtKind::Select(cases) => {
                for c in cases {
                    self.fold_case(c);
                }
            }
            StmtKind::Switch(sw) => {
                self.fold_expr_ctx(&mut sw.expr);
                for c in &mut sw.cases {
                    self.fold_case(c);
                }
            }
            StmtKind::With(w) => {
                self.fold_expr_ctx(&mut w.expr);
                self.fold_stmts(&mut w.body);
            }
            StmtKind::Directive(_) => {}
            StmtKind::Return(None) | StmtKind::Exit(None) | StmtKind::ExitLoop(None)
            | StmtKind::ContinueLoop(None) => {}
        }
    }

    fn fold_case(&mut self, c: &mut CaseClause) {
        for v in &mut c.values {
            self.fold_expr_ctx(v);
        }
        self.fold_stmts(&mut c.body);
    }

    fn fold_expr_ctx(&mut self, e: &mut Expr) {
        if self.fold_expr(e) {
            self.folds += 1;
        }
    }
}

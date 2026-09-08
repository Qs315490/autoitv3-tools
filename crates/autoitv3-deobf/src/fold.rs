//! Constant folding pass.
//!
//! Walks the AST and replaces expressions that can be fully evaluated at
//! analysis time with their constant value. This is the core of undoing the
//! arithmetic/string obfuscation seen in real AutoIt scripts (e.g. building
//! strings by concatenating many indexed constants).
//!
//! Only *pure* constant subexpressions are folded; anything involving a
//! variable, macro, or call is left untouched (so behavior is preserved).

use autoitv3_ast::ast::*;
use autoitv3_ast::span::Span;

/// Fold constant expressions throughout a whole program, in place.
pub fn fold_program(prog: &mut Program) -> usize {
    let mut ctx = FoldCtx { folds: 0 };
    for item in &mut prog.items {
        ctx.fold_item(item);
    }
    ctx.folds
}

/// Try to evaluate an expression to a constant literal.
///
/// Returns `Some(Expr)` (a `Lit`) when the expression is a pure constant, and
/// `None` otherwise. Any folding already performed by `fold_expr` is reflected
/// here, so nested constant subtrees collapse bottom-up.
pub fn try_fold(e: &Expr) -> Option<Expr> {
    match &e.kind {
        ExprKind::Lit(_) => Some(e.clone()),
        ExprKind::Paren(inner) => try_fold(inner),
        ExprKind::Unary(op, a) => {
            let a = try_fold(a)?;
            let k = lit_of(&a)?;
            let out = match op {
                UnaryOp::Neg => neg(k),
                UnaryOp::Plus => Some(k.clone()),
                UnaryOp::Not => not(k),
            }?;
            Some(lit_expr(out, e.span))
        }
        ExprKind::Binary(op, l, r) => {
            let l = try_fold(l)?;
            let r = try_fold(r)?;
            let lk = lit_of(&l)?;
            let rk = lit_of(&r)?;
            let out = apply_binary(op, lk, rk)?;
            Some(lit_expr(out, e.span))
        }
        ExprKind::Ternary(c, a, b) => {
            let c = try_fold(c)?;
            let ck = lit_of(&c)?;
            let truthy = match ck {
                LitKind::Bool(b) => *b,
                LitKind::Int(v) => *v != 0,
                LitKind::Float(f) => *f != 0.0,
                _ => return None,
            };
            try_fold(if truthy { a } else { b })
        }
        _ => None,
    }
}

/// Extract the `LitKind` from a constant `Expr` that is already a literal.
fn lit_of(e: &Expr) -> Option<&LitKind> {
    match &e.kind {
        ExprKind::Lit(l) => Some(&l.kind),
        _ => None,
    }
}

/// Fold a single expression in place, returning true if it changed.
fn fold_expr(e: &mut Expr) -> bool {
    // Recurse into children first (bottom-up).
    let mut changed = false;
    match &mut e.kind {
        ExprKind::Paren(inner) => changed |= fold_expr(inner),
        ExprKind::Unary(_, a) => changed |= fold_expr(a),
        ExprKind::Binary(_, a, b) => {
            changed |= fold_expr(a);
            changed |= fold_expr(b);
        }
        ExprKind::Ternary(c, a, b) => {
            changed |= fold_expr(c);
            changed |= fold_expr(a);
            changed |= fold_expr(b);
        }
        ExprKind::Var(v) => {
            for i in &mut v.indices {
                changed |= fold_expr(i);
            }
        }
        ExprKind::Call(c) => {
            for a in &mut c.args {
                changed |= fold_expr(a);
            }
        }
        ExprKind::IndexCall(_, args) => {
            for a in args {
                changed |= fold_expr(a);
            }
        }
        ExprKind::ArrayLit(items) => {
            for it in items {
                changed |= fold_expr(it);
            }
        }
        _ => {}
    }

    // Now try to collapse this node to a constant.
    if let Some(folded) = try_fold(e) {
        *e = folded;
        changed = true;
    }
    changed
}

struct FoldCtx {
    folds: usize,
}

impl FoldCtx {
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
            StmtKind::Return(Some(e)) | StmtKind::Exit(Some(e)) | StmtKind::ExitLoop(Some(e)) => {
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
            StmtKind::Return(None) | StmtKind::Exit(None) | StmtKind::ExitLoop(None) => {}
        }
    }

    fn fold_case(&mut self, c: &mut CaseClause) {
        for v in &mut c.values {
            self.fold_expr_ctx(v);
        }
        self.fold_stmts(&mut c.body);
    }

    fn fold_expr_ctx(&mut self, e: &mut Expr) {
        if fold_expr(e) {
            self.folds += 1;
        }
    }
}

fn lit_expr(kind: LitKind, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Lit(Lit { kind, span }),
        span,
    }
}

fn neg(k: &LitKind) -> Option<LitKind> {
    match k {
        LitKind::Int(v) => Some(LitKind::Int(-v)),
        LitKind::Float(f) => Some(LitKind::Float(-f)),
        _ => None,
    }
}

fn not(k: &LitKind) -> Option<LitKind> {
    match k {
        LitKind::Bool(b) => Some(LitKind::Bool(!b)),
        LitKind::Int(v) => Some(LitKind::Bool(*v == 0)),
        LitKind::Float(f) => Some(LitKind::Bool(*f == 0.0)),
        _ => None,
    }
}

fn apply_binary(op: &BinaryOp, l: &LitKind, r: &LitKind) -> Option<LitKind> {
    use LitKind::*;
    match (op, l, r) {
        (BinaryOp::Add, Int(a), Int(b)) => Some(Int(a + b)),
        (BinaryOp::Sub, Int(a), Int(b)) => Some(Int(a - b)),
        (BinaryOp::Mul, Int(a), Int(b)) => Some(Int(a * b)),
        (BinaryOp::Div, Int(a), Int(b)) if *b != 0 => Some(Int(a / b)),
        (BinaryOp::Pow, Int(a), Int(b)) if *b >= 0 => Some(Int(a.pow(*b as u32))),

        (BinaryOp::Add, Float(a), Float(b)) => Some(Float(a + b)),
        (BinaryOp::Sub, Float(a), Float(b)) => Some(Float(a - b)),
        (BinaryOp::Mul, Float(a), Float(b)) => Some(Float(a * b)),
        (BinaryOp::Div, Float(a), Float(b)) if *b != 0.0 => Some(Float(a / b)),

        (BinaryOp::Concat, Str(a), Str(b)) => Some(Str(format!("{a}{b}"))),
        (BinaryOp::Concat, Str(a), Int(b)) => Some(Str(format!("{a}{b}"))),
        (BinaryOp::Concat, Str(a), Float(b)) => Some(Str(format!("{a}{b}"))),
        (BinaryOp::Concat, Int(a), Str(b)) => Some(Str(format!("{a}{b}"))),
        (BinaryOp::Concat, Float(a), Str(b)) => Some(Str(format!("{a}{b}"))),

        (BinaryOp::Eq, a, b) => Some(Bool(a == b)),
        (BinaryOp::NotEq, a, b) => Some(Bool(a != b)),
        (BinaryOp::Lt, Int(a), Int(b)) => Some(Bool(a < b)),
        (BinaryOp::Le, Int(a), Int(b)) => Some(Bool(a <= b)),
        (BinaryOp::Gt, Int(a), Int(b)) => Some(Bool(a > b)),
        (BinaryOp::Ge, Int(a), Int(b)) => Some(Bool(a >= b)),
        (BinaryOp::And, Bool(a), Bool(b)) => Some(Bool(*a && *b)),
        (BinaryOp::Or, Bool(a), Bool(b)) => Some(Bool(*a || *b)),

        _ => None,
    }
}
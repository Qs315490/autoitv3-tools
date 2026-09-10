//! Turn indirect, name-in-a-string calls into direct ones.
//!
//! AutoIt can reach a function at run time by naming it in a string:
//!
//! ```autoit
//! Call("Foo", 1)      ; calls Foo(1)
//! Execute("Foo(1)")   ; evaluates the string, calling Foo(1)
//! ```
//!
//! Obfuscators lean on this heavily — the target is hidden behind a string, so
//! neither the call graph nor a reader can tell what actually runs. When the
//! string is a literal naming a function **the script itself defines**, the
//! indirection buys nothing and the call can simply be written out:
//!
//! ```text
//! Call("Foo", 1)     ->  Foo(1)
//! Call("Foo")        ->  Foo()
//! Execute("Foo(1)")  ->  Foo(1)
//! Execute("Foo")     ->  Foo()
//! ```
//!
//! The rewritten call is an ordinary one, so the later `rename` pass gives the
//! target the same alias as its definition.
//!
//! # What is deliberately *not* simplified
//!
//! * `Call($name)` / `Execute($code)` whose name is computed — there is nothing
//!   to read statically. Running the *evaluate* pass first can turn those into
//!   literals, after which this pass sees them on the next run.
//! * Names the script does not define. Without a built-in table a direct call
//!   cannot be validated, so `Call("MsgBox", ...)` is left exactly as written
//!   (it is also perfectly readable already).
//! * An `Execute` argument that is not a single call: an assignment, several
//!   statements, or an expression with no call. `Execute` evaluates its code in
//!   a fresh scope, and only a plain call is equivalent once inlined.
//! * Anything where the function name is not the *whole* string literal —
//!   `Call("Foo" & $suffix)` is resolved at run time.

use std::collections::HashMap;

use autoitv3_ast::ast::*;
use autoitv3_ast::span::Span;

/// Rewrite `Call("Foo", ...)` and `Execute("Foo(...)")` into direct calls.
pub fn simplify_program(prog: &mut Program) -> SimplifyReport {
    let mut ctx = Simplify {
        funcs: defined_functions(&prog.items),
        calls: 0,
        executes: 0,
    };
    ctx.items(&mut prog.items);
    SimplifyReport {
        calls: ctx.calls,
        executes: ctx.executes,
    }
}

/// What one [`simplify_program`] run rewrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SimplifyReport {
    /// `Call("Foo", ...)` → `Foo(...)`.
    pub calls: usize,
    /// `Execute("Foo(...)")` → `Foo(...)`.
    pub executes: usize,
}

impl SimplifyReport {
    /// Total number of indirect calls rewritten.
    pub fn total(&self) -> usize {
        self.calls + self.executes
    }
}

/// Lower-cased name → the spelling the script defines it with.
fn defined_functions(items: &[Item]) -> HashMap<String, String> {
    fn walk(items: &[Item], out: &mut HashMap<String, String>) {
        for item in items {
            match &item.kind {
                ItemKind::Func(f) => {
                    out.entry(f.name.name.to_ascii_lowercase())
                        .or_insert_with(|| f.name.name.clone());
                }
                ItemKind::Region(r) => walk(&r.items, out),
                ItemKind::Stmt(_) | ItemKind::Directive(_) => {}
            }
        }
    }
    let mut out = HashMap::new();
    walk(items, &mut out);
    out
}

struct Simplify {
    /// Script-defined functions: lower-cased name → declared spelling.
    funcs: HashMap<String, String>,
    calls: usize,
    executes: usize,
}

impl Simplify {
    fn items(&mut self, items: &mut [Item]) {
        for item in items {
            match &mut item.kind {
                ItemKind::Func(f) => self.stmts(&mut f.body),
                ItemKind::Stmt(s) => self.stmt(s),
                ItemKind::Region(r) => self.items(&mut r.items),
                ItemKind::Directive(_) => {}
            }
        }
    }

    fn stmts(&mut self, stmts: &mut [Stmt]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &mut Stmt) {
        match &mut s.kind {
            StmtKind::VarDecl(v) => {
                for item in &mut v.vars {
                    for d in &mut item.dims {
                        self.expr(d);
                    }
                    if let Some(init) = &mut item.init {
                        self.expr(init);
                    }
                }
            }
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Return(Some(e))
            | StmtKind::Exit(Some(e))
            | StmtKind::ExitLoop(Some(e))
            | StmtKind::ContinueLoop(Some(e)) => self.expr(e),
            StmtKind::If(if_) => {
                self.expr(&mut if_.cond);
                if let Some(ts) = &mut if_.then_stmt {
                    self.stmt(ts);
                }
                self.stmts(&mut if_.then_block);
                for (c, body) in &mut if_.else_ifs {
                    self.expr(c);
                    self.stmts(body);
                }
                self.stmts(&mut if_.else_block);
            }
            StmtKind::While(w) => {
                self.expr(&mut w.cond);
                self.stmts(&mut w.body);
            }
            StmtKind::DoUntil(d) => {
                self.stmts(&mut d.body);
                self.expr(&mut d.cond);
            }
            StmtKind::For(f) => {
                if let Some(it) = &mut f.iter {
                    self.expr(it);
                }
                self.expr(&mut f.from);
                self.expr(&mut f.to);
                if let Some(st) = &mut f.step {
                    self.expr(st);
                }
                self.stmts(&mut f.body);
            }
            StmtKind::Select(cases) => {
                for c in cases {
                    self.case(c);
                }
            }
            StmtKind::Switch(sw) => {
                self.expr(&mut sw.expr);
                for c in &mut sw.cases {
                    self.case(c);
                }
            }
            StmtKind::With(w) => {
                self.expr(&mut w.expr);
                self.stmts(&mut w.body);
            }
            StmtKind::Directive(_)
            | StmtKind::Return(None)
            | StmtKind::Exit(None)
            | StmtKind::ExitLoop(None)
            | StmtKind::ContinueLoop(None) => {}
        }
    }

    fn case(&mut self, c: &mut CaseClause) {
        for v in &mut c.values {
            self.expr(v);
        }
        self.stmts(&mut c.body);
    }

    /// Walk an expression bottom-up, then try to simplify the node itself, so
    /// the arguments are folded before the call that contains them.
    fn expr(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::Unary(_, a) => self.expr(a),
            ExprKind::Binary(_, a, b) => {
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Paren(p) => self.expr(p),
            ExprKind::Ternary(c, a, b) => {
                self.expr(c);
                self.expr(a);
                self.expr(b);
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    self.expr(it);
                }
            }
            ExprKind::Call(c) => {
                for a in &mut c.args {
                    self.expr(a);
                }
            }
            ExprKind::IndexCall(v, args) => {
                for i in &mut v.indices {
                    self.expr(i);
                }
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Member(recv, _) => self.expr(recv),
            ExprKind::MethodCall(recv, _, args) => {
                self.expr(recv);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Var(v) => {
                for i in &mut v.indices {
                    self.expr(i);
                }
            }
            ExprKind::Lit(_)
            | ExprKind::Macro(_)
            | ExprKind::Ident(_)
            | ExprKind::WithSubject => {}
        }
        self.simplify(e);
    }

    fn simplify(&mut self, e: &mut Expr) {
        let ExprKind::Call(call) = &e.kind else {
            return;
        };
        match call.callee.name.to_ascii_lowercase().as_str() {
            "call" => self.simplify_call(e),
            "execute" => self.simplify_execute(e),
            _ => {}
        }
    }

    /// `Call("Foo" [, arg...])` → `Foo([arg...])`.
    fn simplify_call(&mut self, e: &mut Expr) {
        let ExprKind::Call(call) = &e.kind else {
            return;
        };
        let Some(ExprKind::Lit(Lit {
            kind: LitKind::Str(name),
            ..
        })) = call.args.first().map(|a| &a.kind)
        else {
            return;
        };
        let Some(target) = self.funcs.get(&name.trim().to_ascii_lowercase()) else {
            return;
        };
        let target = target.clone();
        let args = call.args[1..].to_vec();
        let span = e.span;
        e.kind = ExprKind::Call(CallExpr {
            callee: Ident { name: target, span },
            args,
        });
        self.calls += 1;
    }

    /// `Execute("Foo(...)")` → `Foo(...)`, for a string that is exactly one
    /// call to a script-defined function.
    fn simplify_execute(&mut self, e: &mut Expr) {
        let ExprKind::Call(call) = &e.kind else {
            return;
        };
        if call.args.len() != 1 {
            return;
        }
        let ExprKind::Lit(Lit {
            kind: LitKind::Str(code),
            ..
        }) = &call.args[0].kind
        else {
            return;
        };
        let Some(mut replacement) = self.parse_execute(code, e.span) else {
            return;
        };
        std::mem::swap(&mut e.kind, &mut replacement);
        self.executes += 1;
    }

    /// Parse the code inside `Execute` and, when it is a lone call to a
    /// script-defined function, return it as a direct call expression.
    fn parse_execute(&self, code: &str, span: Span) -> Option<ExprKind> {
        let inner = autoitv3_ast::parse(code).ok()?;
        if inner.items.len() != 1 {
            return None;
        }
        let ItemKind::Stmt(stmt) = &inner.items[0].kind else {
            return None;
        };
        let StmtKind::Expr(expr) = &stmt.kind else {
            return None;
        };
        let mut call = match &expr.kind {
            // `Execute("Foo(1)")`
            ExprKind::Call(c) => {
                let target = self.funcs.get(&c.callee.name.to_ascii_lowercase())?;
                CallExpr {
                    callee: Ident {
                        name: target.clone(),
                        span,
                    },
                    args: c.args.clone(),
                }
            }
            // `Execute("Foo")` — a bare identifier in statement position is a
            // call in AutoIt.
            ExprKind::Ident(id) => {
                let target = self.funcs.get(&id.name.to_ascii_lowercase())?;
                CallExpr {
                    callee: Ident {
                        name: target.clone(),
                        span,
                    },
                    args: Vec::new(),
                }
            }
            _ => return None,
        };
        // The arguments were parsed out of the string, so their spans point at
        // text that no longer exists once the call is spliced into the program.
        rebase_spans(&mut call, span);
        Some(ExprKind::Call(call))
    }
}

/// Point every span inside `call` at `span`.
fn rebase_spans(call: &mut CallExpr, span: Span) {
    call.callee.span = span;
    for arg in &mut call.args {
        rebase_expr(arg, span);
    }
}

fn rebase_expr(e: &mut Expr, span: Span) {
    e.span = span;
    match &mut e.kind {
        ExprKind::Lit(lit) => lit.span = span,
        ExprKind::Var(v) => {
            v.name.span = span;
            for i in &mut v.indices {
                rebase_expr(i, span);
            }
        }
        ExprKind::Ident(id) => id.span = span,
        ExprKind::Call(c) => rebase_spans(c, span),
        ExprKind::IndexCall(v, args) => {
            v.name.span = span;
            for i in &mut v.indices {
                rebase_expr(i, span);
            }
            for a in args {
                rebase_expr(a, span);
            }
        }
        ExprKind::Binary(_, a, b) => {
            rebase_expr(a, span);
            rebase_expr(b, span);
        }
        ExprKind::Unary(_, a) => rebase_expr(a, span),
        ExprKind::Paren(p) => rebase_expr(p, span),
        ExprKind::Ternary(c, a, b) => {
            rebase_expr(c, span);
            rebase_expr(a, span);
            rebase_expr(b, span);
        }
        ExprKind::ArrayLit(items) => {
            for it in items {
                rebase_expr(it, span);
            }
        }
        ExprKind::Member(recv, id) => {
            rebase_expr(recv, span);
            id.span = span;
        }
        ExprKind::MethodCall(recv, id, args) => {
            rebase_expr(recv, span);
            id.span = span;
            for a in args {
                rebase_expr(a, span);
            }
        }
        ExprKind::Macro(_) | ExprKind::WithSubject => {}
    }
}

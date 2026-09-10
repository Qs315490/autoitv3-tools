//! Function-table resolver pass.
//!
//! This pass undoes the *function indirection* obfuscation layer. In the
//! script payload, calls to user functions / AutoIt builtins are hidden
//! behind an indirection table: a `Global Const $fn_table = BuildFunctionTable()`
//! builds an array whose element 0 is the count and elements `1..count` are
//! function names (some real AutoIt builtins like `String`, `BitAnd`, plus
//! obfuscated user-defined functions). Calls then go through
//! `$fn_table[0x33d]()` / references through `$fn_table[0x22]`.
//!
//! `BuildFunctionTable()` is a *pure* array-construction function: it declares
//! `Local $x[] = [count, name, ...]` array literals merged with `MergeArrays`,
//! and finally returns the merged array. That is exactly what
//! [`autoitv3_runtime::Runtime`] executes, so this pass simply runs the builder
//! on the interpreter and reads the resulting array — no hand-written
//! evaluator to keep in sync with AutoIt's semantics.
//!
//! After the pass, every `$fn_table[0x..](args)` becomes `FuncName(args)` and
//! every `$fn_table[0x..]` becomes `FuncName` (an `Ident`), so the several thousand
//! obfuscated references become directly readable calls to the real function
//! names. This makes the whole downstream body deobfuscated and greppable.
//!
//! The string table (`$string_table = $fn_table[0x33d]()`, built via `Execute`,
//! maps and binary ops) is NOT resolved by this pass — it requires a full
//! runtime interpreter and is tracked as a follow-up TODO (see README).

use std::collections::HashMap;
use autoitv3_ast::ast::*;
use autoitv3_runtime::{ExecutionProfile, Runtime, Value};

/// Result of resolving the function table.
#[derive(Debug, Default)]
pub struct TableReport {
    /// Number of `$fn_table[...](...)` indexed calls rewritten to plain calls.
    pub calls_rewritten: usize,
    /// Number of `$fn_table[...]` indexed references rewritten to identifiers.
    pub refs_rewritten: usize,
    /// Number of table entries resolved (function names in the table).
    pub entries: usize,
}

/// Resolve the `$fn_table` function table and rewrite all its usages in place.
///
/// `table_var` is the variable name (without `$`) of the function table,
/// defaulting to `fn_table`. `builder_func` is the name (without `$`) of the
/// pure builder that constructs the table, defaulting to `BuildFunctionTable`.
pub fn resolve_function_table(
    prog: &mut Program,
    table_var: &str,
    builder_func: &str,
) -> TableReport {
    // Normalize: AST stores variable names with the leading '$'.
    let tv = if table_var.starts_with('$') {
        table_var.to_string()
    } else {
        format!("${table_var}")
    };
    let mut ctx = ResolveCtx {
        table: None,
        index: HashMap::new(),
        report: TableReport::default(),
    };

    // Pass 1: evaluate the builder function on the runtime to obtain the table.
    ctx.table = eval_builder(prog, builder_func);

    // If we got a table, build the index -> name map.
    if let Some(table) = &ctx.table {
        ctx.report.entries = table.len().saturating_sub(1);
        for (i, name) in table.iter().enumerate() {
            if i == 0 {
                continue; // element 0 is the count
            }
            ctx.index.insert(i as i64, name.clone());
        }
    }

    // Pass 2: rewrite all `$table_var[...]` references throughout the program.
    for item in &mut prog.items {
        ctx.rewrite_item(item, &tv);
    }

    ctx.report
}

struct ResolveCtx {
    /// The statically evaluated table (element 0 = count).
    table: Option<Vec<String>>,
    /// Maps array index -> function name.
    index: HashMap<i64, String>,
    report: TableReport,
}

/// Evaluate the table builder by *running* it on the interpreter.
///
/// `BuildFunctionTable()` is pure array construction (`Local $x[] = [...]`
/// literals merged with the `MergeArrays` helper), so the runtime can execute
/// it directly. Delegating to the interpreter means the pass also copes with
/// builders that use loops, `ReDim`, string work or `Execute` — the shapes the
/// obfuscator's *string* table needs — instead of only the literal pattern this
/// pass used to special-case.
///
/// Returns the resolved element list (element 0 is the count), or `None` when
/// the builder is missing or cannot be evaluated.
fn eval_builder(prog: &Program, builder_func: &str) -> Option<Vec<String>> {
    let mut rt = Runtime::with_program(prog);
    // Deobfuscation must be reproducible and must not touch the machine, so it
    // uses the deterministic profile rather than AutoIt's own semantics.
    rt.set_profile(ExecutionProfile::deterministic());
    // Table builders are finite, but keep a generous guard against a builder
    // that loops forever on an unsupported construct.
    rt.set_max_steps(20_000_000);

    let value = rt.call_function(builder_func, Vec::new()).ok()?;
    let Value::Array(items) = value else { return None };
    let items = items.borrow();
    let mut out = Vec::with_capacity(items.len());
    for v in items.iter() {
        out.push(v.to_autoit_string());
    }
    Some(out)
}

impl ResolveCtx {
    fn rewrite_item(&mut self, item: &mut Item, tv: &str) {
        match &mut item.kind {
            ItemKind::Func(f) => {
                self.rewrite_params(&mut f.params, tv);
                self.rewrite_stmts(&mut f.body, tv);
            }
            ItemKind::Stmt(s) => self.rewrite_stmt(s, tv),
            ItemKind::Region(r) => {
                for it in &mut r.items {
                    self.rewrite_item(it, tv);
                }
            }
            ItemKind::Directive(_) => {}
        }
    }

    fn rewrite_params(&mut self, params: &mut [Param], tv: &str) {
        for p in params {
            if let Some(d) = &mut p.default {
                self.rewrite_expr(d, tv);
            }
        }
    }

    fn rewrite_stmts(&mut self, stmts: &mut Vec<Stmt>, tv: &str) {
        for s in stmts {
            self.rewrite_stmt(s, tv);
        }
    }

    fn rewrite_stmt(&mut self, s: &mut Stmt, tv: &str) {
        match &mut s.kind {
            StmtKind::VarDecl(v) => {
                for vi in &mut v.vars {
                    for d in &mut vi.dims {
                        self.rewrite_expr(d, tv);
                    }
                    if let Some(init) = &mut vi.init {
                        self.rewrite_expr(init, tv);
                    }
                }
            }
            StmtKind::Expr(e) => self.rewrite_expr(e, tv),
            StmtKind::Return(Some(e)) | StmtKind::Exit(Some(e)) | StmtKind::ExitLoop(Some(e))
            | StmtKind::ContinueLoop(Some(e)) => {
                self.rewrite_expr(e, tv);
            }
            StmtKind::If(if_) => {
                self.rewrite_expr(&mut if_.cond, tv);
                if let Some(ts) = &mut if_.then_stmt {
                    self.rewrite_stmt(ts, tv);
                }
                self.rewrite_stmts(&mut if_.then_block, tv);
                for (c, body) in &mut if_.else_ifs {
                    self.rewrite_expr(c, tv);
                    self.rewrite_stmts(body, tv);
                }
                self.rewrite_stmts(&mut if_.else_block, tv);
            }
            StmtKind::While(w) => {
                self.rewrite_expr(&mut w.cond, tv);
                self.rewrite_stmts(&mut w.body, tv);
            }
            StmtKind::DoUntil(d) => {
                self.rewrite_stmts(&mut d.body, tv);
                self.rewrite_expr(&mut d.cond, tv);
            }
            StmtKind::For(f) => {
                if let Some(it) = &mut f.iter {
                    self.rewrite_expr(it, tv);
                }
                self.rewrite_expr(&mut f.from, tv);
                self.rewrite_expr(&mut f.to, tv);
                if let Some(st) = &mut f.step {
                    self.rewrite_expr(st, tv);
                }
                self.rewrite_stmts(&mut f.body, tv);
            }
            StmtKind::Select(cases) => {
                for c in cases {
                    self.rewrite_case(c, tv);
                }
            }
            StmtKind::Switch(sw) => {
                self.rewrite_expr(&mut sw.expr, tv);
                for c in &mut sw.cases {
                    self.rewrite_case(c, tv);
                }
            }
            StmtKind::With(w) => {
                self.rewrite_expr(&mut w.expr, tv);
                self.rewrite_stmts(&mut w.body, tv);
            }
            StmtKind::Directive(_) => {}
            StmtKind::Return(None) | StmtKind::Exit(None) | StmtKind::ExitLoop(None)
            | StmtKind::ContinueLoop(None) => {}
        }
    }

    fn rewrite_case(&mut self, c: &mut CaseClause, tv: &str) {
        for v in &mut c.values {
            self.rewrite_expr(v, tv);
        }
        self.rewrite_stmts(&mut c.body, tv);
    }

    /// Rewrite `$tv[index]` references and `$tv[index](args)` calls.
    fn rewrite_expr(&mut self, e: &mut Expr, tv: &str) {
        match &mut e.kind {
            ExprKind::Var(v) => {
                // AutoIt variable names are case-insensitive, and the
                // obfuscator is inconsistent about it: the table is `$fn_table`
                // in code but `$FN_TABLE` inside the `Execute` strings.
                if v.name.name.eq_ignore_ascii_case(tv) && v.indices.len() == 1 {
                    // `$tv[expr]` where expr is a constant index -> Ident(name).
                    let idx = match &v.indices[0].kind {
                        ExprKind::Lit(Lit { kind: LitKind::Int(n), .. }) => Some(*n),
                        _ => None,
                    };
                    if let Some(idx) = idx {
                        if let Some(name) = self.index.get(&idx) {
                            self.report.refs_rewritten += 1;
                            e.kind = ExprKind::Ident(Ident {
                                name: name.clone(),
                                span: e.span,
                            });
                            return;
                        }
                    }
                }
                // Recurse into subscripts regardless.
                for idx in &mut v.indices {
                    self.rewrite_expr(idx, tv);
                }
            }
            ExprKind::IndexCall(v, args) => {
                if v.name.name.eq_ignore_ascii_case(tv) && v.indices.len() == 1 {
                    let idx = match &v.indices[0].kind {
                        ExprKind::Lit(Lit { kind: LitKind::Int(n), .. }) => Some(*n),
                        _ => None,
                    };
                    if let Some(idx) = idx {
                        let name = self.index.get(&idx).cloned();
                        if let Some(name) = name {
                            // `$tv[idx](args...)` -> `Name(args...)`.
                            let mut call_args = Vec::new();
                            for a in args.drain(..) {
                                call_args.push(a);
                            }
                            for a in &mut call_args {
                                self.rewrite_expr(a, tv);
                            }
                            self.report.calls_rewritten += 1;
                            e.kind = ExprKind::Call(CallExpr {
                                callee: Ident {
                                    name: name.clone(),
                                    span: e.span,
                                },
                                args: call_args,
                            });
                            return;
                        }
                    }
                }
                for idx in &mut v.indices {
                    self.rewrite_expr(idx, tv);
                }
                for a in args {
                    self.rewrite_expr(a, tv);
                }
            }
            ExprKind::Call(c) => {
                for a in &mut c.args {
                    self.rewrite_expr(a, tv);
                }
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    self.rewrite_expr(it, tv);
                }
            }
            ExprKind::Unary(_, a) => self.rewrite_expr(a, tv),
            ExprKind::Binary(_, a, b) => {
                self.rewrite_expr(a, tv);
                self.rewrite_expr(b, tv);
            }
            ExprKind::Paren(p) => self.rewrite_expr(p, tv),
            ExprKind::Ternary(c, a, b) => {
                self.rewrite_expr(c, tv);
                self.rewrite_expr(a, tv);
                self.rewrite_expr(b, tv);
            }
            ExprKind::Member(recv, _) => self.rewrite_expr(recv, tv),
            ExprKind::MethodCall(recv, _, args) => {
                self.rewrite_expr(recv, tv);
                for a in args {
                    self.rewrite_expr(a, tv);
                }
            }
            ExprKind::WithSubject => {}
            ExprKind::Lit(_) | ExprKind::Macro(_) | ExprKind::Ident(_) => {}
        }
    }
}
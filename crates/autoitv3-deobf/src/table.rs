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
//! `Local $x[] = [count, name, ...]` array literals locked with `MergeArrays`,
//! and finally returns the merged array. None of that needs runtime state
//! beyond local arrays and a ReDim/append helper, so it can be evaluated
//! statically over the AST.
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

    // Pass 1: statically evaluate the builder function to obtain the table.
    if let Some(func) = find_func(prog, builder_func) {
        ctx.table = eval_pure_builder(func);
    }

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

fn find_func<'a>(prog: &'a Program, name: &str) -> Option<&'a FuncDef> {
    prog.items.iter().find_map(|it| match &it.kind {
        ItemKind::Func(f) if f.name.name == name => Some(f),
        _ => None,
    })
}

/// Statically evaluate a pure builder function whose body is a sequence of
/// `Local $x[] = [count, ...]` array literals, `MergeArrays(target, src)`
/// calls Scribble, and a final `Return $target`. Returns the built array
/// (element 0 = total count, then names), or `None` if the pattern does not
/// match (in which case the pass leaves things unchanged).
fn eval_pure_builder(func: &FuncDef) -> Option<Vec<String>> {
    // Map of local variable name -> array contents.
    let mut locals: HashMap<String, Vec<String>> = HashMap::new();

    for st in &func.body {
        match &st.kind {
            StmtKind::VarDecl(v) => {
                // Only handle `Local $x[] = [count, name, ...]` (single var,
                // array-literal init). Ignore scalar declarations and other
                // kinds of VarDecl.
                for vi in &v.vars {
                    if vi.dims.is_empty() {
                        continue;
                    }
                    let Some(init) = &vi.init else { continue };
                    let ExprKind::ArrayLit(items) = &init.kind else { continue };
                    // Parse each array element: Int literal (count / index) or
                    // Ident (function name). Skip if any element is not a
                    // plain Int/Ident literal.
                    let mut vals: Vec<String> = Vec::with_capacity(items.len());
                    let mut ok = true;
                    for e in items {
                        match &e.kind {
                            ExprKind::Lit(Lit { kind: LitKind::Int(n), .. }) => {
                                vals.push(n.to_string());
                            }
                            ExprKind::Ident(id) => {
                                vals.push(id.name.clone());
                            }
                            ExprKind::Macro(m) => {
                                vals.push(m.clone());
                            }
                            ExprKind::Lit(Lit { kind: LitKind::Str(s), .. }) => {
                                vals.push(s.clone());
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        continue; // bail out of this declaration
                    }
                    locals.insert(vi.name.name.clone(), vals);
                }
            }
            StmtKind::Expr(e) => {
                let ExprKind::Call(c) = &e.kind else { continue };
                if c.callee.name != "MergeArrays" || c.args.len() != 2 {
                    continue;
                }
                // Resolve target and source variable names from the arguments.
                let tgt = match &c.args[0].kind {
                    ExprKind::Var(v) => v.name.name.clone(),
                    _ => continue,
                };
                let src = match &c.args[1].kind {
                    ExprKind::Var(v) => v.name.name.clone(),
                    _ => continue,
                };
                let (Some(t), Some(s)) = (locals.get(&tgt).cloned(), locals.get(&src).cloned())
                else {
                    continue;
                };
                // MergeArrays(target, source):
                //   ReDim target[target[0] + source[0] + 1]
                //   For i = 1 To source[0]: target[target[0]+i] = source[i]
                //   target[0] += source[0]
                let tcnt = t.first().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                let scnt = s.first().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
                let mut newt = vec![(tcnt + scnt).to_string()];
                // Copy target values 1..tcnt
                for i in 1..=tcnt as usize {
                    if let Some(v) = t.get(i) {
                        newt.push(v.clone());
                    }
                }
                // Copy source values 1..scnt
                for i in 1..=scnt as usize {
                    if let Some(v) = s.get(i) {
                        newt.push(v.clone());
                    }
                }
                locals.insert(tgt.clone(), newt);
            }
            StmtKind::Return(Some(e)) => {
                let ExprKind::Var(v) = &e.kind else { continue };
                return locals.get(&v.name.name).cloned();
            }
            _ => {}
        }
    }
    None
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
            StmtKind::Return(Some(e)) | StmtKind::Exit(Some(e)) | StmtKind::ExitLoop(Some(e)) => {
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
            StmtKind::Return(None) | StmtKind::Exit(None) | StmtKind::ExitLoop(None) => {}
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
                if v.name.name == tv && v.indices.len() == 1 {
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
                if v.name.name == tv && v.indices.len() == 1 {
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
            ExprKind::Lit(_) | ExprKind::Macro(_) | ExprKind::Ident(_) => {}
        }
    }
}
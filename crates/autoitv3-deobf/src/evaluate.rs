//! Runtime evaluation: run the script body, then inline what it computed.
//!
//! The earlier passes work purely on syntax. [`crate::table`] can resolve the
//! obfuscator's *function* table because that is built from array literals
//! alone, but its *string* table is produced by running generated code
//! (`Execute`, maps, `Binary`, string surgery) — nothing static can recover it.
//!
//! So this pass simply **runs** the script's top-level body on
//! [`autoitv3_runtime::Runtime`] and then replaces every constant-indexed
//! reference into a global table with the value that actually came out:
//!
//! ```text
//! $string_table[0xc03]   ->   "Windows\\System32"     (a string the script built)
//! $fn_table[0x33d]() ->   ResolvedFunc()        (a function name from the table)
//! ```
//!
//! # Partial evaluation is the normal case
//!
//! A real script's start-up code quickly reaches the operating system —
//! `DllCall`, the registry, GUI — which is exactly the boundary the
//! `autoitv3-platform` crate marks with an undefined-function error. The
//! obfuscator, however, builds its tables *early*, so an aborted run still
//! leaves them in the globals. This pass therefore keeps whatever the run
//! managed to produce and reports where it stopped, instead of discarding
//! everything because the last line failed.

use std::collections::{HashMap, HashSet};

use autoitv3_ast::ast::*;
use autoitv3_ast::span::Span;
use autoitv3_runtime::profile::ExecutionProfile;
use autoitv3_runtime::{Runtime, Value};

/// What an [`evaluate`] run achieved.
#[derive(Debug, Default, Clone)]
pub struct EvaluateReport {
    /// True when the script body ran to completion.
    pub completed: bool,
    /// Where the run stopped, when it did not complete.
    pub stopped: Option<String>,
    /// Number of globals the runtime produced.
    pub globals: usize,
    /// Number of those that are arrays or maps (i.e. candidate tables).
    pub tables: usize,
    /// Number of constant-indexed references replaced with runtime values.
    pub substitutions: usize,
    /// Number of indexed calls resolved to a real function name.
    pub calls_resolved: usize,
    /// Number of `Global` table declarations replaced by the value itself.
    pub declarations_resolved: usize,
    /// The globals the run produced, kept so the substitution can be repeated
    /// once the later passes have spliced new code into the tree.
    pub values: Tables,
}

/// The globals a run produced, keyed by lower-cased name without the `$`.
///
/// A snapshot is kept because substitution is not a one-shot: the `Simplify`
/// pass turns `Execute("$FN_TABLE[1094]($name_table[175])")` strings into real code,
/// and that code reads the same tables. Running [`Tables::substitute`] again
/// after the simplifier is what finishes the job.
#[derive(Debug, Default, Clone)]
pub struct Tables {
    values: HashMap<String, Value>,
}

/// What a substitution sweep is allowed to rewrite.
#[derive(Debug, Default, Clone, Copy)]
pub struct SubstituteOptions {
    /// Rewrite `Global Const $t = Build()` into the table's literal value.
    ///
    /// **Off by default.** The declaration is the only record of *how* the
    /// script built the table, and for a runtime table the literal is a lot of
    /// output — the reference sample's string table alone is several thousand entries.
    /// Turn it on when the goal is to read the data rather than the code.
    pub inline_declarations: bool,
}

/// What one substitution sweep replaced.
#[derive(Debug, Default, Clone, Copy)]
pub struct SubstitutionCount {
    /// Constant-indexed references replaced with runtime values.
    pub substitutions: usize,
    /// Indexed calls resolved to a real function name.
    pub calls_resolved: usize,
    /// `Global` table declarations replaced by the value itself.
    pub declarations_resolved: usize,
}

impl SubstitutionCount {
    /// Total number of rewrites.
    pub fn total(&self) -> usize {
        self.substitutions + self.calls_resolved + self.declarations_resolved
    }
}

impl Tables {
    /// Build a snapshot from `name -> value` pairs. A leading `$` in the name
    /// is optional; lookups are case-insensitive, like AutoIt itself.
    pub fn new<I: IntoIterator<Item = (String, Value)>>(values: I) -> Self {
        Self {
            values: values
                .into_iter()
                .map(|(name, value)| (name.trim_start_matches('$').to_ascii_lowercase(), value))
                .collect(),
        }
    }

    /// Replace constant-indexed table reads in `prog` with the values the run
    /// produced. Safe to call repeatedly; later sweeps pick up code an earlier
    /// pass spliced in.
    pub fn substitute(&self, prog: &mut Program) -> SubstitutionCount {
        self.substitute_with(prog, SubstituteOptions::default())
    }

    /// As [`Tables::substitute`], with explicit [`SubstituteOptions`].
    pub fn substitute_with(
        &self,
        prog: &mut Program,
        options: SubstituteOptions,
    ) -> SubstitutionCount {
        // Bare variable reads are only safe to inline when the script itself
        // promises the value never changes.
        let consts = const_globals(prog);
        let mut ctx = SubstituteCtx {
            tables: &self.values,
            consts: &consts,
            inline_declarations: options.inline_declarations,
            substitutions: 0,
            calls_resolved: 0,
            declarations_resolved: 0,
        };
        for item in &mut prog.items {
            ctx.item(item);
        }
        SubstitutionCount {
            substitutions: ctx.substitutions,
            calls_resolved: ctx.calls_resolved,
            declarations_resolved: ctx.declarations_resolved,
        }
    }
}

/// Run `prog`'s script body and inline the resulting table values.
///
/// `prog` is modified in place. The script body itself stays in the tree — the
/// caller decides whether to keep it (see `EvaluateReport::completed`).
///
/// Uses [`autoitv3_platform::host_platform`], i.e. the Windows emulation layer
/// configured from the environment. Use [`evaluate_with_platform`] to pass an
/// explicit stack (the CLI's `--win-version`, a test with a custom registry).
pub fn evaluate(prog: &mut Program, profile: ExecutionProfile) -> EvaluateReport {
    evaluate_with_platform(prog, profile, autoitv3_platform::host_platform())
}

/// [`evaluate`] with an explicit platform stack.
///
/// This is how `au3 evaluate --win-version win11` reaches the run: the caller
/// builds the [`Platform`](autoitv3_runtime::platform::Platform) it wants and
/// hands it over.
pub fn evaluate_with_platform(
    prog: &mut Program,
    profile: ExecutionProfile,
    platform: Box<dyn autoitv3_runtime::platform::Platform>,
) -> EvaluateReport {
    evaluate_with_options(prog, profile, platform, SubstituteOptions::default())
}

/// [`evaluate_with_platform`] with explicit [`SubstituteOptions`].
pub fn evaluate_with_options(
    prog: &mut Program,
    profile: ExecutionProfile,
    platform: Box<dyn autoitv3_runtime::platform::Platform>,
    options: SubstituteOptions,
) -> EvaluateReport {
    let mut report = EvaluateReport::default();

    // Run the script body. A failure part-way through is expected and useful:
    // keep the globals it managed to build.
    let mut rt = Runtime::with_program(prog);
    rt.set_platform(platform);
    rt.set_profile(profile);
    rt.set_max_steps(20_000_000);
    match rt.run_script() {
        Ok(flow) => {
            report.completed = flow.is_normal();
        }
        Err(e) => {
            report.stopped = Some(e.to_string());
        }
    }

    let globals = rt.globals_snapshot();
    report.globals = globals.len();

    let mut values: HashMap<String, Value> = HashMap::new();
    for (name, value) in globals {
        if matches!(value, Value::Array(_) | Value::Map(_)) {
            report.tables += 1;
        }
        values.insert(name, value);
    }
    report.values = Tables { values };

    let first = report.values.substitute_with(prog, options);
    report.substitutions = first.substitutions;
    report.calls_resolved = first.calls_resolved;
    report.declarations_resolved = first.declarations_resolved;
    report
}

/// Walks the tree replacing constant-indexed table reads.
///
/// Parameter defaults count: they are evaluated at call time and read the same
/// tables as the body.
struct SubstituteCtx<'a> {
    tables: &'a HashMap<String, Value>,
    /// Names declared `Global Const`, which a bare read may be replaced by.
    consts: &'a HashSet<String>,
    /// Whether a `Global Const` table may be replaced by its value.
    inline_declarations: bool,
    substitutions: usize,
    calls_resolved: usize,
    declarations_resolved: usize,
}

impl SubstituteCtx<'_> {
    fn item(&mut self, item: &mut Item) {
        match &mut item.kind {
            ItemKind::Func(f) => {
                // Parameter defaults are evaluated at call time and may read
                // the tables just like the body does.
                for p in &mut f.params {
                    if let Some(default) = &mut p.default {
                        self.expr(default);
                    }
                }
                self.stmts(&mut f.body);
            }
            ItemKind::Stmt(s) => {
                self.stmt(s);
                self.inline_global_table(s);
            }
            ItemKind::Region(r) => {
                for it in &mut r.items {
                    self.item(it);
                }
            }
            ItemKind::Directive(_) => {}
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
            // A bare control transfer with nothing to walk.
            StmtKind::ContinueCase => {}
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

    fn expr(&mut self, e: &mut Expr) {
        // Recurse first so inner tables collapse before outer ones.
        match &mut e.kind {
            ExprKind::Unary(_, a) => self.expr(a),
            ExprKind::Binary(op, a, b) => {
                if is_assign(op) {
                    // `$x = ...`: the target has to stay a variable, or the
                    // statement becomes `"value" = ...`. Its *subscripts* may
                    // still be substituted (`$a[$string_table[1]] = ...`).
                    self.target(a);
                } else {
                    self.expr(a);
                }
                self.expr(b);
            }
            ExprKind::Paren(p) => self.expr(p),
            ExprKind::Ternary(c, a, b) => {
                self.expr(c);
                self.expr(a);
                self.expr(b);
            }
            ExprKind::Call(c) => {
                for a in &mut c.args {
                    self.expr(a);
                }
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    self.expr(it);
                }
            }
            ExprKind::Member(recv, _) => self.expr(recv),
            ExprKind::MethodCall(recv, _, args) => {
                self.expr(recv);
                for a in args {
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
                self.index_call(e);
            }
            ExprKind::Subscript(base, indices) => {
                // The base is often itself a table call (`DllCall(...)[0]`), so
                // it has to go through the same substitution path.
                self.expr(base);
                for i in indices {
                    self.expr(i);
                }
            }
            ExprKind::Var(v) => {
                for i in &mut v.indices {
                    self.expr(i);
                }
                self.var(e);
            }
            _ => {}
        }
    }

    /// Visit an assignment target: subscripts are fair game, the target
    /// variable itself is not.
    fn target(&mut self, e: &mut Expr) {
        match &mut e.kind {
            ExprKind::Var(v) => {
                for i in &mut v.indices {
                    self.expr(i);
                }
            }
            ExprKind::Member(recv, _) => self.expr(recv),
            _ => self.expr(e),
        }
    }

    /// Replace `$table[i][j]...` with the value the runtime produced.
    ///
    /// A bare `$name` is only substituted when it is `Global Const`: a mutable
    /// global could be reassigned later, and inlining its value would then be
    /// wrong. Indexed reads assume the table is immutable once built, which is
    /// how the obfuscator uses them.
    fn var(&mut self, e: &mut Expr) {
        let ExprKind::Var(v) = &e.kind else { return };
        if v.indices.is_empty() {
            let key = v.name.name.trim_start_matches('$').to_ascii_lowercase();
            if !self.consts.contains(&key) {
                return;
            }
        }
        let Some(value) = self.resolve(&v.name.name, &v.indices) else {
            return;
        };
        if let Some(lit) = literal_of(&value) {
            self.substitutions += 1;
            *e = lit;
        }
    }

    /// Replace `$table[i](args)` with a call to the function the table holds.
    fn index_call(&mut self, e: &mut Expr) {
        let ExprKind::IndexCall(v, _) = &e.kind else { return };
        let Some(value) = self.resolve(&v.name.name, &v.indices) else {
            return;
        };
        let Value::FuncRef(name) = value else { return };
        let ExprKind::IndexCall(_, args) = std::mem::replace(
            &mut e.kind,
            ExprKind::Lit(Lit { kind: LitKind::Null, span: e.span }),
        ) else {
            return;
        };
        self.calls_resolved += 1;
        e.kind = ExprKind::Call(CallExpr {
            callee: Ident { name, span: e.span },
            args,
        });
    }

    /// Replace `Global $t = Build()` with the value the run produced.
    ///
    /// Only arrays: those are the obfuscator's data tables, and once every read
    /// of one has been substituted, the call in its declaration is all that is
    /// left of it. Leaving `$t = SomeBuilder()` there would hide the table's
    /// content behind a function the reader has to trace.
    fn inline_global_table(&mut self, s: &mut Stmt) {
        if !self.inline_declarations {
            return;
        }
        let StmtKind::VarDecl(v) = &mut s.kind else {
            return;
        };
        if !matches!(v.kind, VarKind::Global) {
            return;
        }
        for item in &mut v.vars {
            if !item.dims.is_empty() {
                continue;
            }
            if !matches!(item.init.as_ref().map(|e| &e.kind), Some(ExprKind::Call(_))) {
                continue;
            }
            let key = item.name.name.trim_start_matches('$').to_ascii_lowercase();
            // A plain (non-`Const`) global may be assigned later, so its value
            // now says nothing about its value when the declaration runs.
            let Some(value) = self.tables.get(&key).filter(|_| v.is_const).cloned() else {
                continue;
            };
            if !matches!(value, Value::Array(_)) {
                continue;
            }
            if let Some(literal) = value_literal(&value, 0) {
                item.init = Some(literal);
                self.declarations_resolved += 1;
            }
        }
    }

    /// Walk constant subscripts through the runtime value.
    fn resolve(&self, name: &str, indices: &[Expr]) -> Option<Value> {
        let key = name.trim_start_matches('$').to_ascii_lowercase();
        let mut cur = self.tables.get(&key)?.clone();
        for idx in indices {
            let ExprKind::Lit(lit) = &idx.kind else { return None };
            cur = match (&cur, &lit.kind) {
                (Value::Array(a), LitKind::Int(i)) => {
                    let a = a.borrow();
                    if *i < 0 || *i as usize >= a.len() {
                        return None;
                    }
                    a[*i as usize].clone()
                }
                // AutoIt coerces the subscript, so `$a["2"]` is `$a[2]`.
                (Value::Array(a), LitKind::Str(s)) => {
                    let i = Value::Str(s.clone()).to_int();
                    let a = a.borrow();
                    if i < 0 || i as usize >= a.len() {
                        return None;
                    }
                    a[i as usize].clone()
                }
                // Map keys are strings, so the obfuscator's `$table[0x75]` is the
                // key `"117"` — the same coercion the interpreter applies.
                (Value::Map(m), other) => {
                    let key_value = match other {
                        LitKind::Str(s) => Value::Str(s.clone()),
                        LitKind::Int(i) => Value::Int(*i),
                        LitKind::Float(f) => Value::Float(*f),
                        LitKind::Bool(b) => Value::Bool(*b),
                        LitKind::Null | LitKind::Default => return None,
                    };
                    m.borrow().get(&key_value.to_autoit_string()).cloned()?
                }
                _ => return None,
            };
        }
        Some(cur)
    }
}

/// A scalar value as an AST literal; arrays and maps have no literal form.
///
/// Strings that cannot appear inside an AutoIt `"..."` literal — AutoIt has no
/// escape for a line break, and a literal newline would end the statement — are
/// left alone rather than producing source that does not parse.
/// The literal form of a whole table: an array literal, nested arrays
/// included, with binaries written as `Binary("0x…")`.
///
/// `None` when any element has no literal form (a map, a string with a newline),
/// because a half-written table would be worse than none.
fn value_literal(v: &Value, depth: usize) -> Option<Expr> {
    if depth > 8 {
        return None;
    }
    match v {
        Value::Array(a) => {
            let items = a.borrow();
            let mut out = Vec::with_capacity(items.len());
            for e in items.iter() {
                out.push(value_literal(e, depth + 1)?);
            }
            Some(Expr {
                kind: ExprKind::ArrayLit(out),
                span: Span::default(),
            })
        }
        Value::Binary(b) => Some(Expr {
            kind: ExprKind::Call(CallExpr {
                callee: Ident {
                    name: "Binary".to_string(),
                    span: Span::default(),
                },
                args: vec![Expr {
                    kind: ExprKind::Lit(Lit {
                        kind: LitKind::Str(autoitv3_runtime::value::binary_to_hex(b)),
                        span: Span::default(),
                    }),
                    span: Span::default(),
                }],
            }),
            span: Span::default(),
        }),
        other => literal_of(other),
    }
}

fn literal_of(v: &Value) -> Option<Expr> {
    if let Value::Str(s) = v {
        if s.contains(['\r', '\n', '\0']) {
            return None;
        }
    }
    let kind = v.to_lit_kind()?;
    Some(Expr {
        kind: ExprKind::Lit(Lit { kind, span: Span::default() }),
        span: Span::default(),
    })
}

/// Names declared `Global Const`, lower-cased without the `$`.
fn const_globals(prog: &Program) -> HashSet<String> {
    let mut out = HashSet::new();
    for item in &prog.items {
        collect_consts(item, &mut out);
    }
    out
}

fn collect_consts(item: &Item, out: &mut HashSet<String>) {
    let mut visit = |s: &Stmt| {
        if let StmtKind::VarDecl(v) = &s.kind {
            if v.is_const && matches!(v.kind, VarKind::Global) {
                for vi in &v.vars {
                    out.insert(vi.name.name.trim_start_matches('$').to_ascii_lowercase());
                }
            }
        }
    };
    match &item.kind {
        ItemKind::Stmt(s) => visit(s),
        ItemKind::Func(f) => {
            for s in &f.body {
                visit(s);
            }
        }
        ItemKind::Region(r) => {
            for it in &r.items {
                collect_consts(it, out);
            }
        }
        ItemKind::Directive(_) => {}
    }
}

/// True for the assignment operators, whose left operand is an lvalue.
fn is_assign(op: &BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Assign
            | BinaryOp::PlusAssign
            | BinaryOp::MinusAssign
            | BinaryOp::StarAssign
            | BinaryOp::SlashAssign
            | BinaryOp::CaretAssign
            | BinaryOp::AmpAssign
    )
}
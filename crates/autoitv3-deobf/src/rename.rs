//! Identifier renaming pass.
//!
//! Maps every obfuscated variable to a short, deterministic, readable alias
//! that says **where the variable lives** and **what it holds**:
//!
//! ```text
//! $g_int_000     script-level (Global, or first assigned at the top level)
//! $l_str_003     local to a function (Local/Dim/Static, or first assigned there)
//! $arg_arr_001   a function parameter
//! ```
//!
//! The first field is the scope (`g` / `l` / `arg`), the second the inferred
//! type — `int`, `float`, `str`, `bool`, `arr`, `map`, or `var` when nothing in
//! the source pins it down. Long type names are abbreviated (`integer` → `int`,
//! `string` → `str`, `boolean` → `bool`, `array` → `arr`, `variant` → `var`).
//! The trailing three-digit number keeps aliases unique; each scope has its own
//! counter.
//!
//! Functions the **script itself defines** become `f000`; built-in functions
//! (`MsgBox`, `UBound`, ...) and macros (`@error`, ...) are left exactly as
//! written, because renaming something the runtime resolves by name would
//! break the script. String literals are never rewritten — turning an indirect
//! call into a direct one is the `simplify` pass's job (see
//! [`crate::simplify`]), and it runs before this one.
//!
//! # Optional
//!
//! Renaming is opt-in per category through [`RenameOptions`]:
//! [`rename_program`] renames everything, [`rename_program_with`] takes a
//! selection, and [`RenameOptions::none`] makes the pass a no-op. The
//! orchestrator exposes the same switch as
//! [`Deobfuscator::without_rename`](crate::Deobfuscator::without_rename)
//! and the CLI as `--no-rename`.
//!
//! # Scope analysis
//!
//! AutoIt has no declarations for everything, so the scope is recovered from
//! how a name is introduced:
//!
//! * a `Global` declaration, or a declaration/assignment at script level, makes
//!   the name global *everywhere* — a global read inside a function must keep
//!   the same alias, or renaming would change what the function sees;
//! * an explicit `Local`/`Dim`/`Static` (or a `For` loop variable, or a `ByRef`
//!   parameter) inside a function is local to that function;
//! * a bare assignment inside a function makes a local, unless the name is
//!   global — again so the alias stays stable across scopes;
//! * a function parameter is `arg` and wins inside its own function.
//!
//! Names are matched case-insensitively, because AutoIt variable names are:
//! `$Foo` and `$foo` are the same variable and must get the same alias.
//!
//! # Types
//!
//! The type is inferred statically from the declaration or first assignment —
//! the initializer literal, array dimensions, an array literal, a `Map()`
//! call, or string concatenation. It is a readability hint only: anything the
//! source does not pin down is `var`, and the pass never executes the script.
//!
//! This does not change behavior — it only substitutes names consistently
//! everywhere they appear; string contents are *not* touched.

use std::collections::{HashMap, HashSet};

use autoitv3_ast::ast::*;

/// Deterministically rename identifiers throughout a whole program, in place.
///
/// Shorthand for [`rename_program_with`] with [`RenameOptions::all`].
pub fn rename_program(prog: &mut Program) -> RenameReport {
    rename_program_with(prog, RenameOptions::all())
}

/// Rename the categories `options` enables.
pub fn rename_program_with(prog: &mut Program, options: RenameOptions) -> RenameReport {
    if !options.vars && !options.funcs {
        return RenameReport::default();
    }
    // First learn what every variable is (scope + type) and which functions
    // the script defines; the aliases are then assigned in a second walk.
    let mut analyzer = Analyzer::new();
    analyzer.items(&prog.items);
    let analysis = analyzer.finish();

    let mut ctx = RenameCtx {
        options,
        vars: analysis.vars,
        defined_funcs: analysis.funcs,
        aliases: HashMap::new(),
        funcs: HashMap::new(),
        next_func: 1,
        next_var: [0; 3],
        next_func_alias: 0,
    };
    ctx.visit_items(&mut prog.items);
    RenameReport {
        vars: ctx.aliases.len(),
        funcs: ctx.funcs.len(),
    }
}

/// Which identifier categories the rename pass may touch.
///
/// This is how renaming is made optional: a caller can keep the original names
/// entirely (`RenameOptions::none()`), rename only one category, or rename
/// everything (`RenameOptions::all()`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameOptions {
    /// Variables, to `$<scope>_<type>_<n>` aliases.
    pub vars: bool,
    /// Functions the script itself defines (`Func ... EndFunc`), to `fNNN`.
    /// Built-in functions are never touched.
    pub funcs: bool,
}

impl RenameOptions {
    /// Rename everything the pass understands.
    pub const fn all() -> Self {
        Self {
            vars: true,
            funcs: true,
        }
    }

    /// Rename nothing — the pass becomes a no-op.
    pub const fn none() -> Self {
        Self {
            vars: false,
            funcs: false,
        }
    }
}

impl Default for RenameOptions {
    fn default() -> Self {
        Self::all()
    }
}

#[derive(Debug, Clone, Default)]
pub struct RenameReport {
    pub vars: usize,
    pub funcs: usize,
}

// ---------------------------------------------------------------------------
// Scope and type model
// ---------------------------------------------------------------------------

/// Where a variable lives — the first field of its alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Scope {
    /// Script level: a `Global` declaration, or a top-level declaration or
    /// assignment.
    Global,
    /// Inside one function: `Local`/`Dim`/`Static`, a `For` variable, or a
    /// bare assignment.
    Local,
    /// A function parameter.
    Arg,
}

impl Scope {
    fn token(self) -> &'static str {
        match self {
            Scope::Global => "g",
            Scope::Local => "l",
            Scope::Arg => "arg",
        }
    }

    fn index(self) -> usize {
        match self {
            Scope::Global => 0,
            Scope::Local => 1,
            Scope::Arg => 2,
        }
    }
}

/// The data type inferred for a variable — the second field of its alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    Int,
    Float,
    Str,
    Bool,
    Arr,
    Map,
    /// Nothing in the source pins the type down.
    Var,
}

impl ValueType {
    fn token(self) -> &'static str {
        match self {
            ValueType::Int => "int",
            ValueType::Float => "float",
            ValueType::Str => "str",
            ValueType::Bool => "bool",
            ValueType::Arr => "arr",
            ValueType::Map => "map",
            ValueType::Var => "var",
        }
    }

    /// Combine two hints: a concrete type beats `Var`, the first concrete type
    /// wins over a later, different one.
    fn merge(self, other: ValueType) -> ValueType {
        match (self, other) {
            (ValueType::Var, t) => t,
            (t, _) => t,
        }
    }
}

/// One variable: scope, owning function (`0` = script level) and case-folded
/// name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct VarKey {
    scope: Scope,
    func: u32,
    name: String,
}

impl VarKey {
    fn new(scope: Scope, func: u32, name: &str) -> Self {
        Self {
            scope,
            // Globals are not owned by a function, so they get one shared key.
            func: if scope == Scope::Global { 0 } else { func },
            name: name.trim_start_matches('$').to_ascii_lowercase(),
        }
    }
}

/// What the analysis learned about one variable.
#[derive(Debug, Clone, Copy)]
struct VarInfo {
    ty: ValueType,
    /// True when a declaration/parameter introduced the name, as opposed to a
    /// bare assignment.
    explicit: bool,
}

// ---------------------------------------------------------------------------
// Analysis: which variables exist, where, and holding what
// ---------------------------------------------------------------------------

/// What the analysis learned: every variable, and the function names the
/// script defines — the only call targets the rename pass may touch.
struct Analysis {
    vars: HashMap<VarKey, VarInfo>,
    funcs: HashSet<String>,
}

struct Analyzer {
    vars: HashMap<VarKey, VarInfo>,
    funcs: HashSet<String>,
    next_func: u32,
}

impl Analyzer {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
            funcs: HashSet::new(),
            next_func: 1,
        }
    }

    /// Drop implicit locals whose name is also a script-level variable: those
    /// references resolve to the global, so they must share its alias.
    fn finish(mut self) -> Analysis {
        let globals: HashSet<String> = self
            .vars
            .keys()
            .filter(|k| k.scope == Scope::Global)
            .map(|k| k.name.clone())
            .collect();
        self.vars.retain(|key, info| {
            if key.scope == Scope::Global || info.explicit {
                return true;
            }
            !globals.contains(&key.name)
        });
        Analysis {
            vars: self.vars,
            funcs: self.funcs,
        }
    }

    fn record(&mut self, key: VarKey, ty: ValueType, explicit: bool) {
        self.vars
            .entry(key)
            .and_modify(|info| {
                info.ty = info.ty.merge(ty);
                info.explicit |= explicit;
            })
            .or_insert(VarInfo { ty, explicit });
    }

    /// Where a declaration at `func` lands: script level or a function local.
    fn decl_scope(&self, kind: &VarKind, func: u32) -> Scope {
        // `Local` at script level still names a global, and a `Global`
        // declaration inside a function declares a global.
        if func == 0 || *kind == VarKind::Global {
            Scope::Global
        } else {
            Scope::Local
        }
    }

    fn items(&mut self, items: &[Item]) {
        self.items_in(items, 0);
    }

    fn items_in(&mut self, items: &[Item], func: u32) {
        for item in items {
            match &item.kind {
                ItemKind::Func(f) => {
                    let id = self.next_func;
                    self.next_func += 1;
                    // Remember the definition: only these names may be renamed
                    // at their call sites.
                    self.funcs.insert(f.name.name.to_ascii_lowercase());
                    for p in &f.params {
                        let ty = p
                            .default
                            .as_ref()
                            .map(infer_type)
                            .unwrap_or(ValueType::Var);
                        self.record(VarKey::new(Scope::Arg, id, &p.name.name), ty, true);
                    }
                    for p in &f.params {
                        if let Some(d) = &p.default {
                            self.expr(d, id);
                        }
                    }
                    self.stmts(&f.body, id);
                }
                ItemKind::Stmt(s) => self.stmt(s, func),
                ItemKind::Region(r) => self.items_in(&r.items, func),
                ItemKind::Directive(_) => {}
            }
        }
    }

    fn stmts(&mut self, stmts: &[Stmt], func: u32) {
        for s in stmts {
            self.stmt(s, func);
        }
    }

    fn stmt(&mut self, s: &Stmt, func: u32) {
        match &s.kind {
            StmtKind::VarDecl(v) => {
                let scope = self.decl_scope(&v.kind, func);
                for item in &v.vars {
                    let ty = if item.dims.is_empty() {
                        item.init
                            .as_ref()
                            .map(infer_type)
                            .unwrap_or(ValueType::Var)
                    } else {
                        // `Dim $a[3]` declares an array even without `[...]`.
                        ValueType::Arr
                    };
                    self.record(VarKey::new(scope, func, &item.name.name), ty, true);
                }
                for item in &v.vars {
                    for d in &item.dims {
                        self.expr(d, func);
                    }
                    if let Some(init) = &item.init {
                        self.expr(init, func);
                    }
                }
            }
            StmtKind::Expr(e) => self.expr(e, func),
            StmtKind::Return(Some(e))
            | StmtKind::Exit(Some(e))
            | StmtKind::ExitLoop(Some(e))
            | StmtKind::ContinueLoop(Some(e)) => self.expr(e, func),
            StmtKind::If(if_) => {
                self.expr(&if_.cond, func);
                if let Some(ts) = &if_.then_stmt {
                    self.stmt(ts, func);
                }
                self.stmts(&if_.then_block, func);
                for (c, body) in &if_.else_ifs {
                    self.expr(c, func);
                    self.stmts(body, func);
                }
                self.stmts(&if_.else_block, func);
            }
            StmtKind::While(w) => {
                self.expr(&w.cond, func);
                self.stmts(&w.body, func);
            }
            StmtKind::DoUntil(d) => {
                self.stmts(&d.body, func);
                self.expr(&d.cond, func);
            }
            StmtKind::For(f) => {
                let scope = if func == 0 { Scope::Global } else { Scope::Local };
                let ty = if f.iter.is_some() {
                    ValueType::Var
                } else {
                    infer_type(&f.from).merge(infer_type(&f.to))
                };
                self.record(VarKey::new(scope, func, &f.var.name), ty, true);
                if let Some(it) = &f.iter {
                    self.expr(it, func);
                }
                self.expr(&f.from, func);
                self.expr(&f.to, func);
                if let Some(st) = &f.step {
                    self.expr(st, func);
                }
                self.stmts(&f.body, func);
            }
            StmtKind::Select(cases) => {
                for c in cases {
                    self.case(c, func);
                }
            }
            StmtKind::Switch(sw) => {
                self.expr(&sw.expr, func);
                for c in &sw.cases {
                    self.case(c, func);
                }
            }
            StmtKind::With(w) => {
                self.expr(&w.expr, func);
                self.stmts(&w.body, func);
            }
            StmtKind::Directive(_)
            | StmtKind::Return(None)
            | StmtKind::Exit(None)
            | StmtKind::ExitLoop(None)
            | StmtKind::ContinueLoop(None) => {}
        }
    }

    fn case(&mut self, c: &CaseClause, func: u32) {
        for v in &c.values {
            self.expr(v, func);
        }
        self.stmts(&c.body, func);
    }

    fn expr(&mut self, e: &Expr, func: u32) {
        match &e.kind {
            ExprKind::Binary(op, a, b) => {
                if is_assign(op) {
                    // An assignment to a name nothing declared introduces it:
                    // a global at script level, a local inside a function.
                    if let ExprKind::Var(v) = &a.kind {
                        let scope = if func == 0 { Scope::Global } else { Scope::Local };
                        let ty = if matches!(op, BinaryOp::Assign) {
                            infer_type(b)
                        } else {
                            ValueType::Var
                        };
                        self.record(VarKey::new(scope, func, &v.name.name), ty, false);
                    }
                    self.expr(a, func);
                } else {
                    self.expr(a, func);
                }
                self.expr(b, func);
            }
            ExprKind::Unary(_, a) => self.expr(a, func),
            ExprKind::Paren(p) => self.expr(p, func),
            ExprKind::Ternary(c, a, b) => {
                self.expr(c, func);
                self.expr(a, func);
                self.expr(b, func);
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    self.expr(it, func);
                }
            }
            ExprKind::Call(c) => {
                for a in &c.args {
                    self.expr(a, func);
                }
            }
            ExprKind::IndexCall(v, args) => {
                for i in &v.indices {
                    self.expr(i, func);
                }
                for a in args {
                    self.expr(a, func);
                }
            }
            ExprKind::Subscript(base, indices) => {
                self.expr(base, func);
                for i in indices {
                    self.expr(i, func);
                }
            }
            ExprKind::Member(recv, _) => self.expr(recv, func),
            ExprKind::MethodCall(recv, _, args) => {
                self.expr(recv, func);
                for a in args {
                    self.expr(a, func);
                }
            }
            ExprKind::Lit(_)
            | ExprKind::Var(_)
            | ExprKind::Macro(_)
            | ExprKind::Ident(_)
            | ExprKind::WithSubject => {}
        }
    }
}

/// Static type hint from an initializer expression.
fn infer_type(e: &Expr) -> ValueType {
    match &e.kind {
        ExprKind::Lit(lit) => match lit.kind {
            LitKind::Int(_) => ValueType::Int,
            LitKind::Float(_) => ValueType::Float,
            LitKind::Str(_) => ValueType::Str,
            LitKind::Bool(_) => ValueType::Bool,
            LitKind::Null | LitKind::Default => ValueType::Var,
        },
        ExprKind::ArrayLit(_) => ValueType::Arr,
        ExprKind::Call(c) if c.callee.name.eq_ignore_ascii_case("Map") => ValueType::Map,
        // `"a" & $x` is a string whatever the right-hand side is.
        ExprKind::Binary(BinaryOp::Concat, _, _) => ValueType::Str,
        ExprKind::Unary(UnaryOp::Not, _) => ValueType::Bool,
        ExprKind::Paren(p) => infer_type(p),
        _ => ValueType::Var,
    }
}

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

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

struct RenameCtx {
    /// Which categories may be renamed.
    options: RenameOptions,
    /// Scope and type of every variable, from [`Analyzer`].
    vars: HashMap<VarKey, VarInfo>,
    /// Names the script defines with `Func ... EndFunc`.
    defined_funcs: HashSet<String>,
    aliases: HashMap<VarKey, String>,
    funcs: HashMap<String, String>,
    /// Function counter, kept in step with the analyzer's traversal.
    next_func: u32,
    /// One alias counter per [`Scope`].
    next_var: [usize; 3],
    next_func_alias: usize,
}

impl RenameCtx {
    /// The scope a use of `name` inside `func` resolves to.
    ///
    /// Explicit declarations win over implicit ones: a parameter, then a
    /// function-local declaration, then a script-level name, and only then the
    /// implicit "assignment creates a local" rule.
    fn scope_of(&self, func: u32, name: &str) -> (Scope, u32) {
        if func != 0 {
            if self
                .vars
                .contains_key(&VarKey::new(Scope::Arg, func, name))
            {
                return (Scope::Arg, func);
            }
            if self
                .vars
                .contains_key(&VarKey::new(Scope::Local, func, name))
            {
                return (Scope::Local, func);
            }
        }
        if self
            .vars
            .contains_key(&VarKey::new(Scope::Global, 0, name))
        {
            return (Scope::Global, 0);
        }
        if func == 0 {
            (Scope::Global, 0)
        } else {
            (Scope::Local, func)
        }
    }

    fn var_alias(&mut self, func: u32, name: &str) -> String {
        if !self.options.vars {
            return name.to_string();
        }
        let (scope, owner) = self.scope_of(func, name);
        let key = VarKey::new(scope, owner, name);
        if let Some(alias) = self.aliases.get(&key) {
            return alias.clone();
        }
        let ty = self
            .vars
            .get(&key)
            .map(|info| info.ty)
            .unwrap_or(ValueType::Var);
        let next = &mut self.next_var[scope.index()];
        let alias = format!("${}_{}_{:03}", scope.token(), ty.token(), next);
        *next += 1;
        self.aliases.insert(key, alias.clone());
        alias
    }

    /// The alias for a call target.
    ///
    /// Only names the script defines are renamed: a built-in (`UBound`,
    /// `MsgBox`, ...) or an unknown name is a runtime lookup, so it is left
    /// exactly as written.
    fn func_alias(&mut self, name: &str) -> String {
        let key = name.to_ascii_lowercase();
        if !self.options.funcs || !self.defined_funcs.contains(&key) {
            return name.to_string();
        }
        if let Some(v) = self.funcs.get(&key) {
            return v.clone();
        }
        let alias = format!("f{:03}", self.next_func_alias);
        self.next_func_alias += 1;
        self.funcs.insert(key, alias.clone());
        alias
    }

    fn visit_items(&mut self, items: &mut Vec<Item>) {
        self.visit_items_in(items, 0);
    }

    fn visit_items_in(&mut self, items: &mut Vec<Item>, func: u32) {
        for item in items {
            match &mut item.kind {
                ItemKind::Func(f) => {
                    let id = self.next_func;
                    self.next_func += 1;
                    f.name.name = self.func_alias(&f.name.name.clone());
                    for p in &mut f.params {
                        p.name.name = self.var_alias(id, &p.name.name.clone());
                        if let Some(d) = &mut p.default {
                            self.visit_expr(d, id);
                        }
                    }
                    self.visit_stmts(&mut f.body, id);
                }
                ItemKind::Stmt(s) => self.visit_stmt(s, func),
                ItemKind::Region(r) => self.visit_items_in(&mut r.items, func),
                ItemKind::Directive(_) => {}
            }
        }
    }

    fn visit_stmts(&mut self, stmts: &mut Vec<Stmt>, func: u32) {
        for s in stmts {
            self.visit_stmt(s, func);
        }
    }

    fn visit_stmt(&mut self, s: &mut Stmt, func: u32) {
        match &mut s.kind {
            StmtKind::VarDecl(v) => {
                for item in &mut v.vars {
                    item.name.name = self.var_alias(func, &item.name.name.clone());
                    for d in &mut item.dims {
                        self.visit_expr(d, func);
                    }
                    if let Some(init) = &mut item.init {
                        self.visit_expr(init, func);
                    }
                }
            }
            StmtKind::Expr(e) => self.visit_expr(e, func),
            StmtKind::Return(Some(e)) | StmtKind::Exit(Some(e)) | StmtKind::ExitLoop(Some(e))
            | StmtKind::ContinueLoop(Some(e)) => {
                self.visit_expr(e, func);
            }
            StmtKind::If(if_) => {
                self.visit_expr(&mut if_.cond, func);
                if let Some(ts) = &mut if_.then_stmt {
                    self.visit_stmt(ts, func);
                }
                self.visit_stmts(&mut if_.then_block, func);
                for (c, body) in &mut if_.else_ifs {
                    self.visit_expr(c, func);
                    self.visit_stmts(body, func);
                }
                self.visit_stmts(&mut if_.else_block, func);
            }
            StmtKind::While(w) => {
                self.visit_expr(&mut w.cond, func);
                self.visit_stmts(&mut w.body, func);
            }
            StmtKind::DoUntil(d) => {
                self.visit_stmts(&mut d.body, func);
                self.visit_expr(&mut d.cond, func);
            }
            StmtKind::For(f) => {
                f.var.name = self.var_alias(func, &f.var.name.clone());
                if let Some(it) = &mut f.iter {
                    self.visit_expr(it, func);
                }
                self.visit_expr(&mut f.from, func);
                self.visit_expr(&mut f.to, func);
                if let Some(st) = &mut f.step {
                    self.visit_expr(st, func);
                }
                self.visit_stmts(&mut f.body, func);
            }
            StmtKind::Select(cases) => {
                for c in cases {
                    self.visit_case(c, func);
                }
            }
            StmtKind::Switch(sw) => {
                self.visit_expr(&mut sw.expr, func);
                for c in &mut sw.cases {
                    self.visit_case(c, func);
                }
            }
            StmtKind::With(w) => {
                self.visit_expr(&mut w.expr, func);
                self.visit_stmts(&mut w.body, func);
            }
            StmtKind::Directive(name) => {
                // `#forceref $a, $b` exists only to mark variables as used, but
                // the names in it are real references: leaving them behind
                // would point at a variable that no longer exists.
                if let Some(rewritten) =
                    rewrite_forceref(name, |v| self.var_alias(func, v))
                {
                    *name = rewritten;
                }
            }
            StmtKind::Return(None) | StmtKind::Exit(None) | StmtKind::ExitLoop(None)
            | StmtKind::ContinueLoop(None) => {}
        }
    }

    fn visit_case(&mut self, c: &mut CaseClause, func: u32) {
        for v in &mut c.values {
            self.visit_expr(v, func);
        }
        self.visit_stmts(&mut c.body, func);
    }

    fn visit_expr(&mut self, e: &mut Expr, func: u32) {
        match &mut e.kind {
            ExprKind::Lit(_) => {}
            ExprKind::Var(v) => {
                v.name.name = self.var_alias(func, &v.name.name.clone());
                for i in &mut v.indices {
                    self.visit_expr(i, func);
                }
            }
            // Macros are built-ins the runtime resolves by name (`@error`,
            // `@CRLF`, ...); there is nothing script-defined to rename.
            ExprKind::Macro(_) => {}
            ExprKind::Ident(id) => {
                id.name = self.func_alias(&id.name.clone());
            }
            ExprKind::Call(c) => {
                c.callee.name = self.func_alias(&c.callee.name.clone());
                for a in &mut c.args {
                    self.visit_expr(a, func);
                }
            }
            ExprKind::IndexCall(v, args) => {
                v.name.name = self.var_alias(func, &v.name.name.clone());
                for i in &mut v.indices {
                    self.visit_expr(i, func);
                }
                for a in args {
                    self.visit_expr(a, func);
                }
            }
            ExprKind::Subscript(base, indices) => {
                self.visit_expr(base, func);
                for i in indices {
                    self.visit_expr(i, func);
                }
            }
            ExprKind::Unary(_, a) => self.visit_expr(a, func),
            ExprKind::Binary(_, a, b) => {
                self.visit_expr(a, func);
                self.visit_expr(b, func);
            }
            ExprKind::Paren(p) => self.visit_expr(p, func),
            ExprKind::Ternary(c, a, b) => {
                self.visit_expr(c, func);
                self.visit_expr(a, func);
                self.visit_expr(b, func);
            }
            ExprKind::ArrayLit(items) => {
                for it in items {
                    self.visit_expr(it, func);
                }
            }
            // COM member names are not AutoIt variables or functions, so they
            // are left exactly as written; only the receiver is visited.
            ExprKind::Member(recv, _) => self.visit_expr(recv, func),
            ExprKind::MethodCall(recv, _, args) => {
                self.visit_expr(recv, func);
                for a in args {
                    self.visit_expr(a, func);
                }
            }
            ExprKind::WithSubject => {}
        }
    }
}

/// Rewrite the `$variables` inside a `#forceref` directive body, leaving the
/// keyword and the punctuation exactly as written. `None` for any other
/// directive, which carries no references to rename.
fn rewrite_forceref<F>(text: &str, mut alias: F) -> Option<String>
where
    F: FnMut(&str) -> String,
{
    let bytes = text.as_bytes();
    let keyword_end = bytes
        .iter()
        .position(|b| b.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    if !text[..keyword_end].eq_ignore_ascii_case("forceref") {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..keyword_end]);
    let mut i = keyword_end;
    while i < bytes.len() {
        let start = i;
        if bytes[i] == b'$' {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            if i > start + 1 {
                out.push_str(&alias(&text[start..i]));
                continue;
            }
        }
        while i < bytes.len() && bytes[i] != b'$' {
            i += 1;
        }
        out.push_str(&text[start..i]);
    }
    Some(out)
}

#[cfg(test)]
mod forceref_tests {
    use super::rewrite_forceref;

    #[test]
    fn only_forceref_is_rewritten() {
        assert!(rewrite_forceref("noinline", |v| v.to_string()).is_none());
    }

    #[test]
    fn a_forceref_list_keeps_its_shape() {
        let out = rewrite_forceref("forceref $unused, $other", |v| format!("<{v}>"));
        assert_eq!(out.as_deref(), Some("forceref <$unused>, <$other>"));
    }
}

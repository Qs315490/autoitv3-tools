//! The interpreter core.
//!
//! [`Runtime`] loads a parsed [`Program`](autoitv3_ast::ast::Program) and can
//! call into it. It is deliberately small but semantically careful about the
//! parts of AutoIt the obfuscator leans on:
//!
//! * `ByRef` parameters (copy-in/copy-out plus shared array storage)
//! * `ReDim` resizing an array in place
//! * `For ... To ... Step` and `For ... In`
//! * compound assignment (`+=`, `&=`, ...)
//! * `@error` / `@extended`
//!
//! It also exposes the seams a debugger and a full runtime need — see
//! [`crate::debug`] and [`crate::host`].

use std::collections::HashMap;

use autoitv3_ast::ast::*;
use autoitv3_ast::span::Span;

use crate::builtins;
use crate::debug::{Breakpoints, DebugAction, Debugger, FrameInfo, StopReason};
use crate::error::{Flow, RuntimeError};
use crate::host::{Host, HostContext};
use crate::platform::Platform;
use crate::value::Value;

/// Default runaway-loop guard.
pub const DEFAULT_MAX_STEPS: u64 = 5_000_000;
/// Default recursion guard.
pub const DEFAULT_MAX_DEPTH: usize = 256;

/// One activation record.
struct Frame {
    vars: HashMap<String, Value>,
    function: Option<String>,
    span: Option<Span>,
    /// How many arguments the caller passed, for `@NumParams`.
    arg_count: usize,
}

/// The AutoIt interpreter.
pub struct Runtime {
    globals: HashMap<String, Value>,
    frames: Vec<Frame>,
    /// Lower-cased function name -> definition.
    funcs: HashMap<String, FuncDef>,
    /// Lower-cased function name -> original spelling.
    func_names: HashMap<String, String>,
    /// Plugged-in provider of native functions (the full-runtime seam).
    host: Option<Box<dyn Host>>,
    /// OS integration; supplies builtins that cannot be portable.
    ///
    /// `None` until one is installed: the core deliberately names no concrete
    /// operating system (see `crate::platform`).
    platform: Option<Box<dyn Platform>>,
    /// Attached debugger (the debug-module seam).
    debugger: Option<Box<dyn Debugger>>,
    /// Breakpoints consulted before each statement.
    breakpoints: Breakpoints,
    error: i64,
    extended: i64,
    steps: u64,
    max_steps: u64,
    max_depth: usize,
    /// Set when a debugger asked to stop, read by [`Runtime::take_pause`].
    paused: Option<StopReason>,
    /// Set by `Exit [code]`.
    exit_code: Option<i32>,
    /// Top-level (script) statements, executed by [`Runtime::run_script`].
    script: Vec<Stmt>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new()
    }
}

impl Runtime {
    /// Create a runtime with no program loaded.
    pub fn new() -> Self {
        Self {
            globals: HashMap::new(),
            frames: Vec::new(),
            funcs: HashMap::new(),
            func_names: HashMap::new(),
            host: None,
            platform: None,
            debugger: None,
            breakpoints: Breakpoints::new(),
            error: 0,
            extended: 0,
            steps: 0,
            max_steps: DEFAULT_MAX_STEPS,
            max_depth: DEFAULT_MAX_DEPTH,
            paused: None,
            exit_code: None,
            script: Vec::new(),
        }
    }

    /// Create a runtime with a program already loaded.
    pub fn with_program(prog: &Program) -> Self {
        let mut rt = Self::new();
        rt.load_program(prog);
        rt
    }

    /// Register every `Func` in `prog` so it can be called, and remember the
    /// top-level statements for [`Runtime::run_script`].
    ///
    /// Re-loading replaces previously loaded definitions.
    pub fn load_program(&mut self, prog: &Program) {
        self.script.clear();
        for item in &prog.items {
            self.load_item(item);
        }
    }

    /// Execute the program's top-level statements (script body), stopping at
    /// `Exit` or on error.
    ///
    /// `Func` definitions are *not* executed; call them explicitly with
    /// [`Runtime::call_function`].
    pub fn run_script(&mut self) -> Result<Flow, RuntimeError> {
        let stmts = std::mem::take(&mut self.script);
        let mut flow = Flow::Normal;
        for s in &stmts {
            match self.exec_stmt(s) {
                Ok(Flow::Normal) => {}
                Ok(other) => {
                    flow = other;
                    break;
                }
                Err(e) => {
                    self.script = stmts;
                    return Err(e);
                }
            }
        }
        self.script = stmts;
        Ok(flow)
    }

    fn load_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Func(f) => {
                let key = f.name.name.to_ascii_lowercase();
                self.func_names.insert(key.clone(), f.name.name.clone());
                self.funcs.insert(key, f.clone());
            }
            ItemKind::Stmt(st) => self.script.push(st.clone()),
            ItemKind::Region(r) => {
                for it in &r.items {
                    self.load_item(it);
                }
            }
            _ => {}
        }
    }

    /// True when a user function with this name is loaded.
    pub fn has_function(&self, name: &str) -> bool {
        self.funcs.contains_key(&name.to_ascii_lowercase())
    }

    /// Names of all loaded user functions, in sorted order.
    pub fn function_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.func_names.values().cloned().collect();
        v.sort();
        v
    }

    /// Attach a host providing native functions.
    ///
    /// A host is consulted before the built-in [`Platform`], so an embedding
    /// application can override any platform-provided function.
    pub fn set_host(&mut self, host: Box<dyn Host>) {
        self.host = Some(host);
    }

    /// Install the platform layer.
    ///
    /// Nothing OS-specific can be reached until one is installed; use
    /// `autoitv3_platform::host_platform()` for the current OS.
    pub fn set_platform(&mut self, platform: Box<dyn Platform>) {
        self.platform = Some(platform);
    }

    /// The name of the installed platform, or `none` when there is not one.
    pub fn platform_name(&self) -> &'static str {
        self.platform.as_ref().map(|p| p.name()).unwrap_or("none")
    }

    /// Attach a debugger.
    pub fn set_debugger(&mut self, debugger: Box<dyn Debugger>) {
        self.debugger = Some(debugger);
    }

    /// Take the debugger back out (e.g. to read collected trace data).
    pub fn take_debugger(&mut self) -> Option<Box<dyn Debugger>> {
        self.debugger.take()
    }

    /// The breakpoint set the interpreter consults.
    pub fn breakpoints_mut(&mut self) -> &mut Breakpoints {
        &mut self.breakpoints
    }

    /// Take (and clear) a pending pause request.
    pub fn take_pause(&mut self) -> Option<StopReason> {
        self.paused.take()
    }

    /// Set the runaway-loop step budget (`0` disables the check).
    pub fn set_max_steps(&mut self, steps: u64) {
        self.max_steps = steps;
    }

    /// Set the recursion depth budget.
    pub fn set_max_depth(&mut self, depth: usize) {
        self.max_depth = depth;
    }

    /// The code passed to `Exit`, if the script exited.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// The current `@error` value.
    pub fn error(&self) -> i64 {
        self.error
    }

    /// The current `@extended` value.
    pub fn extended(&self) -> i64 {
        self.extended
    }

    /// Set `@error` / `@extended` (this is what `SetError()` does).
    pub fn set_error_value(&mut self, error: i64, extended: i64) {
        self.error = error;
        self.extended = extended;
    }

    /// Read a global variable by name (with or without the leading `$`).
    pub fn get_global(&self, name: &str) -> Option<&Value> {
        self.globals.get(&var_key(name))
    }

    /// Write a global variable.
    pub fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(var_key(name), value);
    }

    /// Snapshot every global variable (for inspection / debugging).
    pub fn globals_snapshot(&self) -> Vec<(String, Value)> {
        let mut v: Vec<(String, Value)> =
            self.globals.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Snapshot the active call frames, innermost last (for debugging).
    pub fn frames_snapshot(&self) -> Vec<FrameInfo> {
        self.frames
            .iter()
            .enumerate()
            .map(|(i, f)| FrameInfo {
                function: f.function.clone(),
                depth: i,
                span: f.span,
                locals: {
                    let mut v: Vec<(String, Value)> =
                        f.vars.iter().map(|(k, val)| (k.clone(), val.clone())).collect();
                    v.sort_by(|a, b| a.0.cmp(&b.0));
                    v
                },
            })
            .collect()
    }

    // ------------------------------------------------------------------
    // Calling into the program
    // ------------------------------------------------------------------

    /// Call a user function by name with the given arguments.
    ///
    /// This is the entry point the deobfuscator uses to evaluate the
    /// obfuscator's table-builder helpers.
    pub fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, RuntimeError> {
        self.call_user(name, args, None)
    }

    /// Call a function value (as produced by evaluating a bare identifier),
    /// falling back to a builtin when no user function matches.
    pub fn call_value(&mut self, callee: &Value, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        let Value::FuncRef(name) = callee else {
            return Err(RuntimeError::Type {
                expected: "function reference",
                got: callee.type_name().to_string(),
                span: Some(span),
            });
        };
        let name = name.clone();
        self.call_named(&name, args, span)
    }

    /// Call `name` — user function first, then builtin, then host.
    pub fn call_named(&mut self, name: &str, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        if self.has_function(name) {
            return self.call_user(name, args, Some(span));
        }
        self.call_external(name, args, span)
    }

    /// Call a builtin or host function.
    ///
    /// The host is reached through a [`HostContext`] built from *disjoint*
    /// fields of `self`, so no raw pointers or interior mutability are needed.
    fn call_external(&mut self, name: &str, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
        if let Some(v) = builtins::call(self, name, &args, span)? {
            return Ok(v);
        }
        // An explicit host wins over the platform default.
        if self.host.is_some() {
            let Runtime { globals, error, extended, host, .. } = self;
            let mut ctx = HostBridge { globals, error, extended };
            if let Some(host) = host.as_mut() {
                if let Some(v) = host.call(name, args.clone(), &mut ctx)? {
                    return Ok(v);
                }
            }
        }

        // Then the OS layer, when one is installed.
        if self.platform.is_some() {
            let Runtime { globals, error, extended, platform, .. } = self;
            let mut ctx = HostBridge { globals, error, extended };
            if let Some(p) = platform.as_mut() {
                if let Some(v) = p.call(name, args, &mut ctx)? {
                    return Ok(v);
                }
            }
        }

        Err(RuntimeError::UndefinedFunction { name: name.to_string(), span: Some(span) })
    }

    fn call_user(
        &mut self,
        name: &str,
        args: Vec<Value>,
        span: Option<Span>,
    ) -> Result<Value, RuntimeError> {
        let key = name.to_ascii_lowercase();
        let Some(def) = self.funcs.get(&key).cloned() else {
            return Err(RuntimeError::UndefinedFunction {
                name: name.to_string(),
                span,
            });
        };

        if self.frames.len() >= self.max_depth {
            return Err(RuntimeError::CallDepthExceeded { limit: self.max_depth });
        }

        // Set up the frame with parameters bound.
        let mut vars: HashMap<String, Value> = HashMap::new();
        let mut by_ref: Vec<(String, Value)> = Vec::new();
        for (i, p) in def.params.iter().enumerate() {
            let arg = args.get(i).cloned().unwrap_or_else(|| match &p.default {
                Some(d) => self.eval_const_default(d),
                None => Value::Null,
            });
            let k = var_key(&p.name.name);
            vars.insert(k.clone(), arg.clone());
            if p.by_ref {
                by_ref.push((k, arg));
            }
        }

        let display = self
            .func_names
            .get(&key)
            .cloned()
            .unwrap_or_else(|| name.to_string());
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_call_enter(&display, &args);
        }

        self.frames.push(Frame {
            vars,
            function: Some(display.clone()),
            span,
            arg_count: args.len(),
        });

        let mut result = Ok(Value::Null);
        for st in &def.body {
            match self.exec_stmt(st) {
                Ok(Flow::Normal) => {}
                Ok(Flow::Return(v)) => {
                    result = Ok(v);
                    break;
                }
                Ok(Flow::Exit(code)) => {
                    // `Exit` terminates the whole script: record the code and
                    // unwind this call with a null result.
                    self.exit_code = Some(code);
                    result = Ok(Value::Null);
                    break;
                }
                // A loop-control signal escaping a function is a script bug;
                // stop unwinding and report it.
                Ok(Flow::Break(_)) | Ok(Flow::Continue(_)) => {
                    result = Err(RuntimeError::Unsupported {
                        what: format!("loop control outside loop in {display}"),
                        span,
                    });
                    break;
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }

        let frame = self.frames.pop().expect("frame pushed above");
        // Copy-out `ByRef` parameters back into the caller's variable.
        for (k, _) in &by_ref {
            if let Some(v) = frame.vars.get(k) {
                let v = v.clone();
                self.write_back_by_ref(k, v);
            }
        }

        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_call_exit(&display, result.as_ref().ok());
        }
        result
    }

    /// Copy a `ByRef` parameter back to the caller's variable of the same name.
    fn write_back_by_ref(&mut self, key: &str, value: Value) {
        if let Some(frame) = self.frames.last_mut() {
            if frame.vars.contains_key(key) {
                frame.vars.insert(key.to_string(), value);
                return;
            }
        }
        // The caller passed a global.
        if self.globals.contains_key(key) {
            self.globals.insert(key.to_string(), value);
        }
    }

    /// Evaluate a parameter default (must not depend on frame state).
    fn eval_const_default(&mut self, e: &Expr) -> Value {
        self.eval_expr(e).unwrap_or(Value::Null)
    }

    // ------------------------------------------------------------------
    // Variables
    // ------------------------------------------------------------------

    fn current_vars(&mut self) -> &mut HashMap<String, Value> {
        if self.frames.is_empty() {
            &mut self.globals
        } else {
            &mut self.frames.last_mut().expect("non-empty").vars
        }
    }

    fn read_var(&self, name: &str, span: Span) -> Result<Value, RuntimeError> {
        let key = var_key(name);
        if let Some(f) = self.frames.last() {
            if let Some(v) = f.vars.get(&key) {
                return Ok(v.clone());
            }
        }
        if let Some(v) = self.globals.get(&key) {
            return Ok(v.clone());
        }
        // Unset variables read as "" in AutoIt when no `MustDeclareVars` is set.
        let _ = span;
        Ok(Value::Str(String::new()))
    }

    fn write_var(&mut self, name: &str, value: Value, scope: VarScope, span: Span) {
        let key = var_key(name);
        if let Some(dbg) = self.debugger.as_mut() {
            dbg.on_variable_write(&key, &value);
        }
        let _ = span;
        match scope {
            VarScope::Global => {
                self.globals.insert(key, value);
            }
            VarScope::Local => {
                self.current_vars().insert(key, value);
            }
            VarScope::Auto => {
                // Prefer an existing binding: local first, then global.
                if let Some(f) = self.frames.last_mut() {
                    if f.vars.contains_key(&key) {
                        f.vars.insert(key, value);
                        return;
                    }
                }
                if self.globals.contains_key(&key) {
                    self.globals.insert(key, value);
                    return;
                }
                self.current_vars().insert(key, value);
            }
        }
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    /// Evaluate an expression in the current frame.
    pub fn eval_expr(&mut self, e: &Expr) -> Result<Value, RuntimeError> {
        self.tick()?;
        match &e.kind {
            ExprKind::Lit(l) => Ok(match &l.kind {
                LitKind::Int(i) => Value::Int(*i),
                LitKind::Float(f) => Value::Float(*f),
                LitKind::Str(s) => Value::Str(s.clone()),
                LitKind::Bool(b) => Value::Bool(*b),
                LitKind::Default => Value::Default,
                LitKind::Null => Value::Null,
            }),
            ExprKind::Macro(name) => Ok(self.eval_macro(name)),
            ExprKind::Ident(id) => Ok(Value::FuncRef(id.name.clone())),
            ExprKind::Var(v) => {
                let base = self.read_var(&v.name.name, e.span)?;
                self.index_value(base, &v.indices, e.span)
            }
            ExprKind::Call(c) => {
                let mut args = Vec::with_capacity(c.args.len());
                for a in &c.args {
                    args.push(self.eval_expr(a)?);
                }
                self.call_named(&c.callee.name, args, e.span)
            }
            ExprKind::IndexCall(v, args) => {
                let callee = {
                    let base = self.read_var(&v.name.name, e.span)?;
                    self.index_value(base, &v.indices, e.span)?
                };
                let mut argv = Vec::with_capacity(args.len());
                for a in args {
                    argv.push(self.eval_expr(a)?);
                }
                self.call_value(&callee, argv, e.span)
            }
            ExprKind::Unary(op, a) => {
                let v = self.eval_expr(a)?;
                Ok(match op {
                    UnaryOp::Not => Value::Bool(!v.is_truthy()),
                    UnaryOp::Neg => match v {
                        Value::Int(i) => Value::Int(-i),
                        other => Value::Float(-other.to_f64()),
                    },
                    UnaryOp::Plus => match v {
                        Value::Int(i) => Value::Int(i),
                        other => Value::Float(other.to_f64()),
                    },
                })
            }
            ExprKind::Binary(op, a, b) => self.eval_binary(op, a, b, e.span),
            ExprKind::Paren(p) => self.eval_expr(p),
            // COM/object member access has no portable semantics: it needs a
            // platform host (see `crate::platform`) to resolve the member
            // against a real object.
            ExprKind::Member(_, name) => {
                return Err(RuntimeError::Unsupported {
                    what: format!("member access `.{}` (needs a platform host)", name.name),
                    span: Some(e.span),
                })
            }
            ExprKind::MethodCall(_, name, _) => {
                return Err(RuntimeError::Unsupported {
                    what: format!("method call `.{}()` (needs a platform host)", name.name),
                    span: Some(e.span),
                })
            }
            ExprKind::WithSubject => {
                return Err(RuntimeError::Unsupported {
                    what: "`With` subject outside a platform host".to_string(),
                    span: Some(e.span),
                })
            }
            ExprKind::Ternary(c, x, y) => {
                if self.eval_expr(c)?.is_truthy() {
                    self.eval_expr(x)
                } else {
                    self.eval_expr(y)
                }
            }
            ExprKind::ArrayLit(items) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items {
                    out.push(self.eval_expr(it)?);
                }
                Ok(Value::array(out))
            }
        }
    }

    fn eval_binary(
        &mut self,
        op: &BinaryOp,
        a: &Expr,
        b: &Expr,
        span: Span,
    ) -> Result<Value, RuntimeError> {
        use BinaryOp::*;
        // Assignment operators write through to an lvalue.
        if matches!(op, Assign | PlusAssign | MinusAssign | StarAssign | SlashAssign | CaretAssign | AmpAssign) {
            let rhs = self.eval_expr(b)?;
            let value = match op {
                Assign => rhs,
                _ => {
                    let cur = self.eval_expr(a)?;
                    let arith = match op {
                        PlusAssign => Add,
                        MinusAssign => Sub,
                        StarAssign => Mul,
                        SlashAssign => Div,
                        CaretAssign => Pow,
                        AmpAssign => Concat,
                        _ => unreachable!(),
                    };
                    apply_arith(&cur, &rhs, &arith)
                }
            };
            self.assign_to(a, value.clone(), span)?;
            return Ok(value);
        }

        // Short-circuit `And` / `Or` like AutoIt does.
        match op {
            And => {
                let l = self.eval_expr(a)?;
                if !l.is_truthy() {
                    return Ok(Value::Bool(false));
                }
                return Ok(Value::Bool(self.eval_expr(b)?.is_truthy()));
            }
            Or => {
                let l = self.eval_expr(a)?;
                if l.is_truthy() {
                    return Ok(Value::Bool(true));
                }
                return Ok(Value::Bool(self.eval_expr(b)?.is_truthy()));
            }
            _ => {}
        }

        let l = self.eval_expr(a)?;
        let r = self.eval_expr(b)?;
        Ok(match op {
            // `==` is case-sensitive for strings; `=` and `<>` are not.
            Eq => Value::Bool(l.eq_strict(&r)),
            EqLoose => Value::Bool(l.eq_loose(&r)),
            NotEq => Value::Bool(!l.eq_loose(&r)),
            Lt => Value::Bool(l.compare(&r) == std::cmp::Ordering::Less),
            Le => Value::Bool(l.compare(&r) != std::cmp::Ordering::Greater),
            Gt => Value::Bool(l.compare(&r) == std::cmp::Ordering::Greater),
            Ge => Value::Bool(l.compare(&r) != std::cmp::Ordering::Less),
            Add | Sub | Mul | Div | Pow | Concat | BitAnd => apply_arith(&l, &r, op),
            Assign | PlusAssign | MinusAssign | StarAssign | SlashAssign | CaretAssign | AmpAssign => {
                unreachable!("handled above")
            }
            And | Or => unreachable!("handled above"),
        })
    }

    /// Write `value` into the lvalue expression `target`.
    fn assign_to(&mut self, target: &Expr, value: Value, span: Span) -> Result<(), RuntimeError> {
        match &target.kind {
            ExprKind::Var(v) if v.indices.is_empty() => {
                self.write_var(&v.name.name, value, VarScope::Auto, span);
                Ok(())
            }
            ExprKind::Var(v) => {
                let base = self.read_var(&v.name.name, span)?;
                self.assign_index(base, &v.indices, value, span)
            }
            other => Err(RuntimeError::Unsupported {
                what: format!("assignment target {other:?}"),
                span: Some(span),
            }),
        }
    }

    /// Apply array/map subscripts to `base`.
    fn index_value(
        &mut self,
        base: Value,
        indices: &[Expr],
        span: Span,
    ) -> Result<Value, RuntimeError> {
        let mut cur = base;
        for idx in indices {
            let key = self.eval_expr(idx)?;
            cur = match &cur {
                Value::Array(a) => {
                    let i = key.to_int();
                    let arr = a.borrow();
                    if i < 0 || i as usize >= arr.len() {
                        return Err(RuntimeError::IndexOutOfBounds {
                            index: i,
                            len: arr.len(),
                            span: Some(span),
                        });
                    }
                    arr[i as usize].clone()
                }
                Value::Map(m) => {
                    let k = key.to_autoit_string();
                    m.borrow().get(&k).cloned().unwrap_or(Value::Null)
                }
                Value::Str(s) => {
                    // AutoIt strings are not indexable; return "".
                    let _ = s;
                    Value::Str(String::new())
                }
                Value::Null => Value::Null,
                other => {
                    return Err(RuntimeError::Type {
                        expected: "Array or Map",
                        got: other.type_name().to_string(),
                        span: Some(span),
                    })
                }
            };
        }
        Ok(cur)
    }

    /// Write into an array element or map key.
    fn assign_index(
        &mut self,
        base: Value,
        indices: &[Expr],
        value: Value,
        span: Span,
    ) -> Result<(), RuntimeError> {
        if indices.is_empty() {
            return Ok(());
        }
        // Evaluate the leading subscripts to find the container.
        let mut container = base;
        for idx in &indices[..indices.len() - 1] {
            let key = self.eval_expr(idx)?;
            container = match &container {
                Value::Array(a) => {
                    let i = key.to_int();
                    let arr = a.borrow();
                    if i < 0 || i as usize >= arr.len() {
                        return Err(RuntimeError::IndexOutOfBounds {
                            index: i,
                            len: arr.len(),
                            span: Some(span),
                        });
                    }
                    arr[i as usize].clone()
                }
                Value::Map(m) => {
                    let k = key.to_autoit_string();
                    m.borrow().get(&k).cloned().unwrap_or(Value::Null)
                }
                other => {
                    return Err(RuntimeError::Type {
                        expected: "Array or Map",
                        got: other.type_name().to_string(),
                        span: Some(span),
                    })
                }
            };
        }
        let last = &indices[indices.len() - 1];
        let key = self.eval_expr(last)?;
        match &container {
            Value::Array(a) => {
                let i = key.to_int();
                let mut arr = a.borrow_mut();
                if i < 0 || i as usize >= arr.len() {
                    return Err(RuntimeError::IndexOutOfBounds {
                        index: i,
                        len: arr.len(),
                        span: Some(span),
                    });
                }
                arr[i as usize] = value;
                Ok(())
            }
            Value::Map(m) => {
                m.borrow_mut().insert(key.to_autoit_string(), value);
                Ok(())
            }
            other => Err(RuntimeError::Type {
                expected: "Array or Map",
                got: other.type_name().to_string(),
                span: Some(span),
            }),
        }
    }

    /// Resolve an AutoIt macro.
    ///
    /// The interpreter answers the macros that are pure interpreter state and
    /// defers everything environment-dependent to the installed platform, so
    /// `@TempDir` and friends are real values rather than empty strings.
    fn eval_macro(&self, name: &str) -> Value {
        let key = name.trim_start_matches('@').to_ascii_lowercase();
        match key.as_str() {
            // Interpreter state.
            "error" => Value::Int(self.error),
            "extended" => Value::Int(self.extended),
            "scriptlinenumber" => Value::Int(
                self.frames
                    .last()
                    .and_then(|f| f.span)
                    .map(|s| s.start.line as i64)
                    .unwrap_or(0),
            ),
            "numparams" => Value::Int(
                self.frames.last().map(|f| f.arg_count as i64).unwrap_or(0),
            ),
            // Universal character constants.
            "crlf" => Value::Str("\r\n".into()),
            "cr" => Value::Str("\r".into()),
            "lf" => Value::Str("\n".into()),
            "tab" => Value::Str("\t".into()),
            // Everything else is environment-dependent.
            _ => match self.platform.as_ref().and_then(|p| p.macro_value(&key)) {
                Some(v) => v,
                None => Value::Null,
            },
        }
    }

    fn tick(&mut self) -> Result<(), RuntimeError> {
        self.steps += 1;
        if self.max_steps != 0 && self.steps > self.max_steps {
            return Err(RuntimeError::StepLimitExceeded { limit: self.max_steps });
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    /// Execute a statement, returning the control-flow signal it produced.
    pub fn exec_stmt(&mut self, s: &Stmt) -> Result<Flow, RuntimeError> {
        self.tick()?;

        // Debug hooks: breakpoints, then the debugger's per-statement callback.
        if self.debugger.is_some() || !self.breakpoints.items().is_empty() {
            let depth = self.frames.len();
            if let Some(bp) = self.breakpoints.hit(s.span) {
                let reason = StopReason::Breakpoint { id: bp.id, line: bp.line };
                if let Some(dbg) = self.debugger.as_mut() {
                    dbg.on_stop(&reason);
                }
                self.paused = Some(reason);
            }
            if let Some(dbg) = self.debugger.as_mut() {
                match dbg.on_statement(s.span, depth) {
                    DebugAction::Continue => {}
                    DebugAction::Pause => {
                        self.paused = Some(StopReason::Step);
                    }
                    DebugAction::Abort => {
                        return Err(RuntimeError::Unsupported {
                            what: "aborted by debugger".to_string(),
                            span: Some(s.span),
                        })
                    }
                }
            }
        }

        if let Some(f) = self.frames.last_mut() {
            f.span = Some(s.span);
        }

        match &s.kind {
            StmtKind::Directive(_) => Ok(Flow::Normal),
            StmtKind::Expr(e) => {
                self.eval_expr(e)?;
                Ok(Flow::Normal)
            }
            StmtKind::VarDecl(v) => {
                self.exec_var_decl(v, s.span)?;
                Ok(Flow::Normal)
            }
            StmtKind::Return(e) => {
                let v = match e {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Null,
                };
                Ok(Flow::Return(v))
            }
            StmtKind::Exit(e) => {
                let code = match e {
                    Some(e) => self.eval_expr(e)?.to_int() as i32,
                    None => 0,
                };
                Ok(Flow::Exit(code))
            }
            StmtKind::ExitLoop(e) => Ok(Flow::Break(loop_levels(self, e)?)),
            StmtKind::ContinueLoop(e) => Ok(Flow::Continue(loop_levels(self, e)?)),
            StmtKind::If(if_) => self.exec_if(if_),
            StmtKind::While(w) => self.exec_while(w),
            StmtKind::DoUntil(d) => self.exec_do_until(d),
            StmtKind::For(f) => self.exec_for(f),
            StmtKind::Select(cases) => self.exec_cases(cases),
            StmtKind::Switch(sw) => {
                let subject = self.eval_expr(&sw.expr)?;
                for c in &sw.cases {
                    let mut matched = false;
                    for v in &c.values {
                        let cv = self.eval_expr(v)?;
                        if c.is_else || subject.eq_loose(&cv) {
                            matched = true;
                            break;
                        }
                    }
                    if matched || c.is_else {
                        return self.exec_block(&c.body);
                    }
                }
                Ok(Flow::Normal)
            }
            StmtKind::With(w) => {
                self.eval_expr(&w.expr)?;
                self.exec_block(&w.body)
            }
        }
    }

    fn exec_var_decl(&mut self, v: &VarDecl, span: Span) -> Result<(), RuntimeError> {
        let scope = match v.kind {
            VarKind::Global => VarScope::Global,
            VarKind::Local | VarKind::Dim | VarKind::Static => VarScope::Local,
        };
        // Running state for `Enum` numbering (ignored for other declarations).
        let enum_step = match &v.enum_step {
            Some(e) => self.eval_expr(e)?.to_int(),
            None => 1,
        };
        let mut enum_next: Option<i64> = None;

        for item in &v.vars {
            let key = var_key(&item.name.name);
            if v.is_redim {
                // `ReDim $a[n]` — resize in place, preserving existing values.
                let size = self.dim_size(&item.dims, span)?;
                let cur = self.read_var(&item.name.name, span)?;
                if let Value::Array(a) = cur {
                    let mut arr = a.borrow_mut();
                    arr.resize(size, Value::Int(0));
                } else {
                    self.write_var(&item.name.name, Value::array_sized(size), scope, span);
                }
                continue;
            }

            let value = if let Some(init) = &item.init {
                self.eval_expr(init)?
            } else if !item.dims.is_empty() {
                if is_empty_brackets(&item.dims) {
                    // `Local $m[]` declares a Map (an array needs a size or an
                    // initializer).
                    Value::map()
                } else {
                    let size = self.dim_size(&item.dims, span)?;
                    Value::array_sized(size)
                }
            } else {
                Value::Str(String::new())
            };

            if v.is_enum {
                // Enum numbering: an explicit value resets the counter,
                // otherwise the member continues the sequence, advancing by
                // `Step n` (default 1).
                let value = match &item.init {
                    Some(init) => self.eval_expr(init)?.to_int(),
                    None => match enum_next {
                        Some(prev) => prev + enum_step,
                        None => 0,
                    },
                };
                enum_next = Some(value);
                self.write_var(&item.name.name, Value::Int(value), scope, span);
                continue;
            }
            let _ = key;
            self.write_var(&item.name.name, value, scope, span);
        }
        Ok(())
    }

    /// Size for `$a[n]`. Empty brackets (`$a[]`) are not a size — they are the
    /// "let the initializer decide" form, which yields an empty array.
    fn dim_size(&mut self, dims: &[Expr], span: Span) -> Result<usize, RuntimeError> {
        let Some(first) = dims.first() else { return Ok(0) };
        if matches!(first.kind, ExprKind::Lit(Lit { kind: LitKind::Null, .. })) {
            return Ok(0);
        }
        let n = self.eval_expr(first)?.to_int();
        if n < 0 {
            return Err(RuntimeError::IndexOutOfBounds {
                index: n,
                len: 0,
                span: Some(span),
            });
        }
        Ok(n as usize)
    }

    fn exec_if(&mut self, if_: &IfStmt) -> Result<Flow, RuntimeError> {
        if self.eval_expr(&if_.cond)?.is_truthy() {
            if let Some(ts) = &if_.then_stmt {
                return self.exec_stmt(ts);
            }
            return self.exec_block(&if_.then_block);
        }
        for (cond, body) in &if_.else_ifs {
            if self.eval_expr(cond)?.is_truthy() {
                return self.exec_block(body);
            }
        }
        self.exec_block(&if_.else_block)
    }

    fn exec_while(&mut self, w: &WhileStmt) -> Result<Flow, RuntimeError> {
        loop {
            if !self.eval_expr(&w.cond)?.is_truthy() {
                break;
            }
            match self.exec_block(&w.body)? {
                Flow::Normal => {}
                Flow::Break(n) if n <= 1 => break,
                Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                Flow::Continue(n) if n <= 1 => continue,
                Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_do_until(&mut self, d: &DoUntilStmt) -> Result<Flow, RuntimeError> {
        loop {
            match self.exec_block(&d.body)? {
                Flow::Normal => {}
                Flow::Break(n) if n <= 1 => break,
                Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                Flow::Continue(n) if n <= 1 => {}
                Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                other => return Ok(other),
            }
            if self.eval_expr(&d.cond)?.is_truthy() {
                break;
            }
        }
        Ok(Flow::Normal)
    }

    fn exec_for(&mut self, f: &ForStmt) -> Result<Flow, RuntimeError> {
        if let Some(iter) = &f.iter {
            // `For $x In $collection`
            let subject = self.eval_expr(iter)?;
            let items: Vec<Value> = match &subject {
                Value::Array(a) => a.borrow().clone(),
                Value::Map(m) => m.borrow().values().cloned().collect(),
                Value::Null => Vec::new(),
                other => vec![other.clone()],
            };
            for item in items {
                self.write_var(&f.var.name, item, VarScope::Auto, f.var.span);
                match self.exec_block(&f.body)? {
                    Flow::Normal => {}
                    Flow::Break(n) if n <= 1 => break,
                    Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                    Flow::Continue(n) if n <= 1 => continue,
                    Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                    other => return Ok(other),
                }
            }
            return Ok(Flow::Normal);
        }

        let from = self.eval_expr(&f.from)?;
        let to = self.eval_expr(&f.to)?;
        let step = match &f.step {
            Some(s) => self.eval_expr(s)?,
            None => Value::Int(1),
        };
        let stepf = step.to_f64();
        if stepf == 0.0 {
            return Ok(Flow::Normal);
        }
        let mut cur = from.to_f64();
        let end = to.to_f64();
        // Integer loop when everything is integral, so indices stay exact.
        let integral = matches!(from, Value::Int(_))
            && matches!(to, Value::Int(_))
            && matches!(step, Value::Int(_));
        loop {
            if stepf > 0.0 && cur > end {
                break;
            }
            if stepf < 0.0 && cur < end {
                break;
            }
            let v = if integral { Value::Int(cur as i64) } else { Value::Float(cur) };
            self.write_var(&f.var.name, v, VarScope::Auto, f.var.span);
            match self.exec_block(&f.body)? {
                Flow::Normal => {}
                Flow::Break(n) if n <= 1 => break,
                Flow::Break(n) => return Ok(Flow::Break(n - 1)),
                Flow::Continue(n) if n <= 1 => {}
                Flow::Continue(n) => return Ok(Flow::Continue(n - 1)),
                other => return Ok(other),
            }
            cur += stepf;
        }
        Ok(Flow::Normal)
    }

    fn exec_cases(&mut self, cases: &[CaseClause]) -> Result<Flow, RuntimeError> {
        for c in cases {
            if c.is_else {
                return self.exec_block(&c.body);
            }
            for v in &c.values {
                let cv = self.eval_expr(v)?;
                if cv.is_truthy() {
                    return self.exec_block(&c.body);
                }
            }
        }
        Ok(Flow::Normal)
    }

    /// Execute a list of statements, propagating the first non-`Normal` flow.
    pub fn exec_block(&mut self, body: &[Stmt]) -> Result<Flow, RuntimeError> {
        for s in body {
            let flow = self.exec_stmt(s)?;
            if !flow.is_normal() {
                return Ok(flow);
            }
        }
        Ok(Flow::Normal)
    }

    /// Execute a string as AutoIt source (`Execute()`), in the current frame.
    pub fn execute_source(&mut self, src: &str, span: Span) -> Result<Value, RuntimeError> {
        let prog = autoitv3_ast::parse(src).map_err(|e| RuntimeError::Unsupported {
            what: format!("Execute() parse error: {}", e.msg),
            span: Some(span),
        })?;
        // Only expression statements contribute a value, matching AutoIt's
        // `Execute` returning the value of the last expression.
        let mut last = Value::Null;
        for item in &prog.items {
            match &item.kind {
                ItemKind::Func(f) => {
                    let key = f.name.name.to_ascii_lowercase();
                    self.func_names.insert(key.clone(), f.name.name.clone());
                    self.funcs.insert(key, f.clone());
                }
                ItemKind::Stmt(s) => {
                    if let StmtKind::Expr(e) = &s.kind {
                        last = self.eval_expr(e)?;
                    } else {
                        match self.exec_stmt(s)? {
                            Flow::Normal => last = Value::Null,
                            Flow::Return(v) => return Ok(v),
                            other => {
                                return Err(RuntimeError::Unsupported {
                                    what: format!("Execute() control flow {other:?}"),
                                    span: Some(span),
                                })
                            }
                        }
                    }
                }
                ItemKind::Region(r) => {
                    for it in &r.items {
                        self.load_item(it);
                    }
                }
                ItemKind::Directive(_) => {}
            }
        }
        Ok(last)
    }
}

/// True when `e` is a *pure constant* expression: literals combined with
/// unary/binary/ternary operators, with no variables, macros, identifiers or
/// calls.
///
/// Deobfuscation uses this as the safety gate for inlining an evaluated
/// expression: a pure constant is guaranteed to have the same value wherever
/// it appears, which is exactly what constant folding needs.
pub fn is_constant_expr(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Lit(_) => true,
        ExprKind::Paren(inner) => is_constant_expr(inner),
        ExprKind::Unary(_, a) => is_constant_expr(a),
        ExprKind::Binary(_, a, b) => is_constant_expr(a) && is_constant_expr(b),
        ExprKind::Ternary(c, a, b) => {
            is_constant_expr(c) && is_constant_expr(a) && is_constant_expr(b)
        }
        // Variables, macros, idents (function references) and calls all depend
        // on runtime state, so they are not constant.
        _ => false,
    }
}

/// True for the `$x[]` form — empty brackets, which the parser records as a
/// `Null` literal dimension.
fn is_empty_brackets(dims: &[Expr]) -> bool {
    dims.len() == 1
        && matches!(
            dims[0].kind,
            ExprKind::Lit(Lit { kind: LitKind::Null, .. })
        )
}

/// Variable names are case-insensitive in AutoIt and stored without the `$`.
fn var_key(name: &str) -> String {
    name.trim_start_matches('$').to_ascii_lowercase()
}

/// Where a variable write should land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VarScope {
    /// Force the global scope (`Global`).
    Global,
    /// Force the current frame (`Local` / `Dim` / `Static`).
    Local,
    /// Existing binding wins, else the current frame.
    Auto,
}

fn loop_levels(rt: &mut Runtime, e: &Option<Expr>) -> Result<usize, RuntimeError> {
    match e {
        Some(e) => {
            let n = rt.eval_expr(e)?.to_int();
            Ok(if n < 1 { 1 } else { n as usize })
        }
        None => Ok(1),
    }
}

/// Numeric / string arithmetic shared by binary operators and compound assigns.
fn apply_arith(l: &Value, r: &Value, op: &BinaryOp) -> Value {
    use BinaryOp::*;
    match op {
        Concat => Value::Str(format!("{}{}", l.to_autoit_string(), r.to_autoit_string())),
        BitAnd => {
            // `&` between non-strings is bitwise AND in AutoIt.
            if matches!(l, Value::Str(_)) || matches!(r, Value::Str(_)) {
                Value::Str(format!("{}{}", l.to_autoit_string(), r.to_autoit_string()))
            } else {
                Value::Int(l.to_int() & r.to_int())
            }
        }
        _ => {
            let both_int = matches!(l, Value::Int(_)) && matches!(r, Value::Int(_));
            let (a, b) = (l.to_f64(), r.to_f64());
            match op {
                Add => int_or_float(both_int, a + b),
                Sub => int_or_float(both_int, a - b),
                Mul => int_or_float(both_int, a * b),
                Div => {
                    if b == 0.0 {
                        // AutoIt divides by zero -> 0 with @error set; we keep
                        // it simple and return 0.
                        Value::Int(0)
                    } else if both_int && (l.to_int() % r.to_int()) == 0 {
                        Value::Int(l.to_int() / r.to_int())
                    } else {
                        Value::Float(a / b)
                    }
                }
                Pow => int_or_float(both_int, a.powf(b)),
                _ => Value::Null,
            }
        }
    }
}

fn int_or_float(both_int: bool, v: f64) -> Value {
    if both_int && v.fract() == 0.0 && v.abs() < 9.0e15 {
        Value::Int(v as i64)
    } else {
        Value::Float(v)
    }
}

/// Adapts the interpreter's variable/error state to the [`HostContext`] seam
/// without borrowing the whole [`Runtime`].
struct HostBridge<'a> {
    globals: &'a mut HashMap<String, Value>,
    error: &'a mut i64,
    extended: &'a mut i64,
}

impl HostContext for HostBridge<'_> {
    fn get_global(&self, name: &str) -> Option<Value> {
        self.globals.get(&var_key(name)).cloned()
    }

    fn set_global(&mut self, name: &str, value: Value) {
        self.globals.insert(var_key(name), value);
    }

    fn error(&self) -> i64 {
        *self.error
    }

    fn set_error(&mut self, error: i64, extended: i64) {
        *self.error = error;
        *self.extended = extended;
    }
}
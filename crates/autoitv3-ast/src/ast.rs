//! AutoIt v3 Abstract Syntax Tree (AST).
//!
//! Every node carries a `Span` so that later passes (deobfuscation, source
//! mapping, and breakpoint debugging) can locate any node precisely in the
//! original source.

use crate::span::Span;

/// A complete parsed AutoIt v3 program.
#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
    /// All `;` comments collected from the source, in token order.
    /// Preserved so the pretty-printer can re-emit them by default.
    pub comments: Vec<Comment>,
}

/// A comment from the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    /// For a `;` line comment: the text after the `;`. For a `#cs ... #ce`
    /// block: the whole block verbatim, `#cs`/`#ce` lines included.
    pub text: String,
    /// True for a `#cs ... #ce` block comment, false for a `;` line comment.
    pub block: bool,
    pub span: Span,
}

/// A top-level program item.
#[derive(Debug, Clone)]
pub struct Item {
    pub kind: ItemKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    /// A preprocessor directive such as `#include`, `#NoTrayIcon`.
    Directive(String),
    /// A `Func ... EndFunc` definition.
    Func(FuncDef),
    /// A top-level (non-func) statement.
    Stmt(Stmt),
    /// A `#region ... #endregion` style block (kept as a container).
    Region(Region),
}

/// A `Func name(params) ... EndFunc` definition.
#[derive(Debug, Clone)]
pub struct FuncDef {
    pub name: Ident,
    pub params: Vec<Param>,
    pub body: Vec<Stmt>,
    /// True for `Volatile Func Foo()` — the optimiser may not reorder or
    /// inline calls to it.
    pub is_volatile: bool,
    /// The span of the whole `Func ... EndFunc`.
    pub span: Span,
}

/// A single function parameter.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: Ident,
    pub by_ref: bool,
    pub default: Option<Expr>,
    pub span: Span,
}

/// A statement. `Stmt` is the unit executed by the future interpreter /
/// debugger, so it must be easy to instrument (each variant is a node).
#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    /// A variable declaration: `Local $x`, `Global Const $y = 1`, etc.
    VarDecl(VarDecl),
    /// A preprocessor directive inside a function body (e.g. `#forceref`).
    Directive(String),
    /// A plain expression statement (function call, assignment, etc.).
    Expr(Expr),
    /// `Return [expr]`
    Return(Option<Expr>),
    /// `Exit [code]`
    Exit(Option<Expr>),
    /// `ExitLoop [n]` — break out of `n` loops.
    ExitLoop(Option<Expr>),
    /// `ContinueLoop [n]` — continue the `n`-th enclosing loop.
    ///
    /// Kept distinct from `ExitLoop` because an interpreter must know which
    /// control transfer to perform.
    ContinueLoop(Option<Expr>),
    /// `ContinueCase` — fall through to the next `Case` of the enclosing
    /// `Select`/`Switch` without testing it.
    ///
    /// Kept as its own statement for the same reason as `ContinueLoop`: it is
    /// a control transfer, and one that is only meaningful inside a `Case`.
    ContinueCase,
    /// `If cond Then stmt` (single-line).
    If(IfStmt),
    /// `While cond ... WEnd`
    While(WhileStmt),
    /// `Do ... Until cond`
    DoUntil(DoUntilStmt),
    /// `For $i = a To b [Step c] ... Next`
    For(ForStmt),
    /// `Select ... Case ... EndSelect`
    Select(Vec<CaseClause>),
    /// `Switch expr ... Case ... EndSwitch`
    Switch(SwitchStmt),
    /// `With expr ... EndWith`
    With(WithStmt),
}

/// A variable declaration (Local/Global/Const/Dim/Static).
#[derive(Debug, Clone)]
pub struct VarDecl {
    pub kind: VarKind,
    pub is_const: bool,
    /// True for `Enum` / `Global Enum` blocks (members are constants).
    pub is_enum: bool,
    /// True for `ReDim $a[...]` — resizes an existing array in place rather
    /// than declaring a new variable. Kept separate from `Dim Const` because
    /// an interpreter must treat the two differently.
    pub is_redim: bool,
    /// The `Step n` of `Enum Step n $A, $B`, which controls how members
    /// without an explicit value are numbered.
    pub enum_step: Option<Expr>,
    pub vars: Vec<VarDeclItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VarKind {
    Local,
    Global,
    Dim,
    Static,
}

/// One variable in a declaration: `$x`, `$x[2]`, `$x = expr`, or `$x[2] = expr`.
#[derive(Debug, Clone)]
pub struct VarDeclItem {
    pub name: Ident,
    /// Bracket dimensions for `Dim`/`Local` arrays, e.g. `[3]` or `[2][4]`.
    pub dims: Vec<Expr>,
    pub init: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct IfStmt {
    pub cond: Expr,
    /// The single statement after `Then` on the same line (optional).
    pub then_stmt: Option<Box<Stmt>>,
    /// The multi-line `Then` body (empty for single-line form).
    pub then_block: Vec<Stmt>,
    /// The `ElseIf cond Then ...` clauses.
    pub else_ifs: Vec<(Expr, Vec<Stmt>)>,
    /// The `Else` block statements.
    pub else_block: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct WhileStmt {
    pub cond: Expr,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct DoUntilStmt {
    pub body: Vec<Stmt>,
    pub cond: Expr,
}

#[derive(Debug, Clone)]
pub struct ForStmt {
    pub var: Ident,
    /// For-In form: the iterated expression (`For $x In $arr`).
    pub iter: Option<Expr>,
    pub from: Expr,
    pub to: Expr,
    pub step: Option<Expr>,
    pub body: Vec<Stmt>,
}

/// A `Case` clause: either a value list or `Else`.
#[derive(Debug, Clone)]
pub struct CaseClause {
    pub values: Vec<Expr>,
    pub is_else: bool,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct SwitchStmt {
    pub expr: Expr,
    pub cases: Vec<CaseClause>,
}

#[derive(Debug, Clone)]
pub struct WithStmt {
    pub expr: Expr,
    pub body: Vec<Stmt>,
}

/// An expression.
#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    /// A literal value.
    Lit(Lit),
    /// A variable, possibly with array subscripts: `$a`, `$a[0]`, `$a[1][2]`.
    Var(VarExpr),
    /// A macro: `@error`.
    Macro(String),
    /// An identifier that is a call target or a constant name.
    Ident(Ident),
    /// A function call: `MsgBox(0, "x", $y)`.
    Call(CallExpr),
    /// A user-defined (non-keyword) binary operation.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// A unary operation.
    Unary(UnaryOp, Box<Expr>),
    /// Parenthesized expression (kept for source fidelity).
    Paren(Box<Expr>),
    /// Ternary conditional: `cond ? a : b`.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Array literal initializer: `[$a, $b, ...]`.
    ArrayLit(Vec<Expr>),
    /// Call of a function reference stored in an array element:
    /// `$arr[i](args...)`.
    IndexCall(VarExpr, Vec<Expr>),
    /// A subscript on something that is not a plain variable, e.g.
    /// `DllCall(...)[0]` or `$obj.Items[2]`. A subscript on a variable lives in
    /// [`VarExpr::indices`] instead, so this holds only the other bases.
    Subscript(Box<Expr>, Vec<Expr>),
    /// Member access on a COM/object value: `$obj.Property`,
    /// `$chart.Series(1)`, `.Value` inside `With ... EndWith`.
    Member(Box<Expr>, Ident),
    /// Method call on a COM/object value: `$obj.Method(args...)`.
    MethodCall(Box<Expr>, Ident, Vec<Expr>),
    /// The implicit subject of a `With ... EndWith` block, i.e. the receiver
    /// of a leading `.Member`. Never appears on its own.
    WithSubject,
}

/// A literal value.
#[derive(Debug, Clone)]
pub struct Lit {
    pub kind: LitKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LitKind {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Default,
    Null,
}

/// A variable reference, possibly with subscripts.
#[derive(Debug, Clone)]
pub struct VarExpr {
    pub name: Ident,
    /// Array subscripts, e.g. `[0]`, `[1][2]`.
    pub indices: Vec<Expr>,
}

#[derive(Debug, Clone)]
pub struct Ident {
    pub name: String,
    pub span: Span,
    /// `name` lower-cased and without a leading `$`: the key of every
    /// case-insensitive lookup, variables and functions alike.
    ///
    /// Filled on first use and then never written again — a run does not edit
    /// the AST, so this is a cache, not state. It exists because the interpreter
    /// resolves a name millions of times and `to_ascii_lowercase()` allocates
    /// on each of them.
    key: std::cell::OnceCell<String>,
}

impl Ident {
    /// An identifier for `name`. Use this rather than a struct literal: the
    /// lookup cache is bookkeeping only the AST owns.
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
            key: std::cell::OnceCell::new(),
        }
    }

    /// The lower-cased lookup key for this name, computed once.
    ///
    /// A leading `$` (how the lexer spells a variable) is dropped, so the same
    /// key serves variable and function lookups.
    pub fn key(&self) -> &str {
        self.key
            .get_or_init(|| self.name.trim_start_matches('$').to_ascii_lowercase())
    }

    /// Rename the identifier, dropping the cached lookup key.
    ///
    /// Use this rather than assigning to [`Ident::name`] directly: a cached key
    /// describes the old spelling, and a later lookup would silently use it.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
        self.key = std::cell::OnceCell::new();
    }
}

#[derive(Debug, Clone)]
pub struct CallExpr {
    pub callee: Ident,
    pub args: Vec<Expr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinaryOp {
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    CaretAssign,
    AmpAssign,
    /// `==` — equality, case-sensitive for strings.
    Eq,
    /// `=` — equality in *expression* context, case-insensitive for strings.
    ///
    /// AutoIt overloads `=`: at statement level it assigns, inside an
    /// expression it compares. Both roles therefore need their own variant so
    /// the interpreter can apply the right semantics (and the pretty-printer
    /// can reproduce the original spelling).
    EqLoose,
    /// `<>` — inequality, case-insensitive for strings.
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    BitAnd,
    And,
    Or,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnaryOp {
    Not,
    Neg,
    Plus,
}

/// A `#region` / `#endregion` block.
#[derive(Debug, Clone)]
pub struct Region {
    pub items: Vec<Item>,
    pub span: Span,
}
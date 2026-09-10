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

/// A `;` comment from the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    /// The comment text after the `;` (leading `;` not included).
    pub text: String,
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
    Eq,
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
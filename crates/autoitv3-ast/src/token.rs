//! Lexical tokens consumed by the parser.
//!
//! The lexer strips whitespace but preserves `;` comments as `Comment` tokens,
//! so the parser can carry them into the AST and the pretty-printer can
//! re-emit them. Every token carries its `Span` in the source.

use crate::span::Span;

/// A lexical token produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// A human readable name used in error messages.
    pub fn describe(&self) -> String {
        match &self.kind {
            TokenKind::IdentTok(s) | TokenKind::Macro(s) | TokenKind::Var(s) => s.clone(),
            other => format!("{other:?}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ----- Identifiers / literals -----
    /// An ordinary identifier: `Foo`, `MsgBox`, `func1`.
    IdentTok(String),
    /// A macro: `@ScriptDir`, `@error`.
    Macro(String),
    /// A variable: `$Foo`, `$__x`.
    Var(String),
    /// A numeric literal (raw text, e.g. `0x1A`, `1.5`, `0x2f`).
    Number(String),
    /// A quoted string literal (quotes stripped, doubled `""` decoded).
    Str(String),
    /// `True` / `False` literals.
    True,
    False,
    /// The `Default` keyword.
    Default,
    /// The `Null` keyword.
    Null,
    /// The `Volatile` function modifier (`Volatile Func Foo()`).
    Volatile,

    // ----- Comments -----
    /// A comment. For a `;` line comment `text` holds what follows the `;`
    /// (trailing `\r` trimmed) and `block` is false; for a `#cs ... #ce`
    /// block `text` holds the whole block verbatim, `#cs`/`#ce` lines
    /// included, and `block` is true. The newline after a line comment is a
    /// separate token.
    Comment { text: String, block: bool },

    // ----- Statement separators -----
    Newline,
    Colon,

    // ----- Preprocessor -----
    /// `#directive` e.g. `#include`, `#NoTrayIcon`, `#RequireAdmin`.
    Preproc(String),

    // ----- Punctuation -----
    LParen,
    RParen,
    LBracket,
    RBracket,
    /// `.` — member access on a COM/object value (`$obj.Prop`, `$obj.Method()`),
    /// and the implicit `With` subject when it starts an expression.
    Dot,
    Comma,

    // ----- Keywords -----
    Func,
    EndFunc,
    Local,
    Global,
    Const,
    Dim,
    ReDim,
    Static,
    ByRef,
    If,
    ElseIf,
    Else,
    EndIf,
    Then,
    For,
    To,
    Step,
    Next,
    While,
    WEnd,
    Do,
    Until,
    Select,
    Case,
    EndSelect,
    Switch,
    EndSwitch,
    Return,
    ExitLoop,
    ContinueLoop,
    Exit,
    ContinueCase,
    With,
    EndWith,
    And,
    Or,
    Not,
    In,
    Enum,

    // ----- Operators -----
    Assign,    // =
    PlusAssign, // +=
    MinusAssign, // -=
    StarAssign, // *=
    SlashAssign, // /=
    CaretAssign, // ^=
    AmpAssign, // &=
    Eq,     // ==
    NotEq,  // <>
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Caret,    // ^
    Amp,      // &  (used as both concat and bitwise-and)
    Question, // ? (ternary)

    // ----- End of input -----
    Eof,
}
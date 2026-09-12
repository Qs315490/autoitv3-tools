//! autoitv3-ast: an AutoIt v3 lexer + parser producing a span-aware AST.
//!
//! This crate is the foundation for AutoIt deobfuscation. Its design keeps
//! each stage (lexing, parsing, printing) as a separate module so future
//! stages (constant folding, breakpoint debugging, an evaluator) can be
//! layered on top without touching the core.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;
pub mod vocab;

pub use ast::{Program, Stmt, StmtKind, Expr, ExprKind};
pub use parser::{parse, ParseError, Parser};

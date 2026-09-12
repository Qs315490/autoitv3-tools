//! The AutoIt name vocabulary a token stream needs, re-exported.
//!
//! A compiled script names things *by position*: a keyword can be a small
//! index into AutoIt's keyword table, a built-in function is an index into its
//! function table, and the rest arrive as upper-cased names (`STRINGLEN`,
//! `CRLF`) that have to be turned back into AutoIt's spelling before the source
//! can be printed. Decoding therefore needs the tables — but they are the
//! *language's*, not this container format's, so they live with the crates that
//! own the concepts:
//!
//! * [`autoitv3_ast::vocab`] owns the keyword table (the lexer is what decides
//!   which words are keywords);
//! * [`autoitv3_runtime::vocab`] owns the function and macro tables (the
//!   runtime is what implements and evaluates them).
//!
//! This module is only the seam: the deassembler imports one place, and the
//! project keeps one copy of each table, in the crate whose job it describes.

pub use autoitv3_ast::vocab::{canonical_keyword, KEYWORDS};
pub use autoitv3_runtime::vocab::{canonical_function, canonical_macro, FUNCTIONS, MACROS};

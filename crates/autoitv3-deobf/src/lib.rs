//! autoitv3-deobf: source-level deobfuscation passes over the AutoIt v3 AST.
//!
//! These passes transform an already-parsed `autoitv3_ast::Program` in place
//! and, combined with the pretty-printer, produce deobfuscated AutoIt source.
//!
//! Current passes:
//! - `fold`    : constant folding — evaluate pure arithmetic/string/concat
//!   expressions on literals and inline them.
//! - `rename`  : deterministic renaming of generated/obfuscated identifiers
//!   and macros, so output is greppable and reproducible.
//! - `table`   : resolve the `$fn_table` function table and rewrite indexed
//!   calls/references to real function names.
//! - `evaluate`: run the script body and inline the values it computed —
//!   the only way to recover the obfuscator's *string* table.
//! - `orchestrator`: runs a pipeline of passes over a program.

pub mod evaluate;
pub mod fold;
pub mod rename;
pub mod table;
pub mod orchestrator;

pub use evaluate::{evaluate, EvaluateReport};
pub use orchestrator::{deobfuscate, Deobfuscator};

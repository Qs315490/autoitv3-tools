//! autoitv3-deobf: source-level deobfuscation passes over the AutoIt v3 AST.
//!
//! These passes transform an already-parsed `autoitv3_ast::Program` in place
//! and, combined with the pretty-printer, produce deobfuscated AutoIt source.
//!
//! Current passes:
//! - `fold`    : constant folding — evaluate pure arithmetic/string/concat
//!   expressions on literals and inline them.
//! - `table`   : resolve the function-indirection table and rewrite indexed
//!   calls/references to real function names.
//! - `simplify`: turn `Call("Foo", ...)` / `Execute("Foo(...)")` into direct
//!   calls, so a function named in a string stops hiding the call graph.
//! - `rename`  : deterministic renaming of variables to
//!   `$<scope>_<type>_<n>` aliases (`$g_int_000`, `$l_str_003`,
//!   `$arg_arr_001`) and of the functions the script itself defines (`fNNN`),
//!   so output is greppable and reproducible. Built-in functions and macros are
//!   left alone, and the whole pass is optional ([`RenameOptions`]).
//! - `evaluate`: run the script body and inline the values it computed —
//!   the only way to recover the obfuscator's *string* table.
//! - `orchestrator`: runs a pipeline of passes over a program.

pub mod evaluate;
pub mod fold;
pub mod orchestrator;
pub mod rename;
pub mod simplify;
pub mod table;

pub use evaluate::{
    evaluate, evaluate_with_debugger, evaluate_with_options, evaluate_with_platform,
    EvaluateReport, SubstituteOptions, SubstitutionCount, Tables,
};
// The build facts an evaluation runs under (`@Compiled`, `@Unicode`,
// `@AutoItX64`) belong to the runtime; they are re-exported so a caller of
// [`evaluate_with_options`] can name them without a second dependency.
pub use autoitv3_runtime::BuildFacts;
pub use orchestrator::{deobfuscate, DeobfReport, Deobfuscator, Pass};
pub use rename::{rename_program, rename_program_with, RenameOptions, RenameReport};
pub use simplify::{simplify_program, SimplifyReport};
pub use table::TableOptions;

//! Subcommand modules and the dispatch table.
//!
//! Each subcommand owns its arguments and its implementation in its own file:
//!
//! | command | module |
//! |---|---|
//! | `parse` | [`parse`] |
//! | `pretty` | [`pretty`] |
//! | `deobfuscate` | [`deobfuscate`] |
//! | `evaluate` | [`evaluate`] |
//! | `run` | [`run`] |
//! | `debug` | [`debug`] |
//!
//! The registry itself (names, aliases, help) is declared in
//! [`crate::cli`]; adding a command means adding a module here and a variant
//! there.

pub mod debug;
pub mod deobfuscate;
pub mod evaluate;
pub mod parse;
pub mod pretty;
pub mod run;

use crate::args::CliResult;
use crate::cli::{Cli, Command};

/// Run the subcommand `cli` selected.
pub fn dispatch(cli: &Cli) -> CliResult<()> {
    match &cli.command {
        Command::Parse(args) => parse::run(args),
        Command::Pretty(args) => pretty::run(args),
        Command::Deobfuscate(args) => deobfuscate::run(args),
        Command::Evaluate(args) => evaluate::run(args),
        Command::Run(args) => run::run(args),
        Command::Debug(args) => debug::run(args),
    }
}
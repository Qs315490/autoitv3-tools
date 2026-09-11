//! Top-level command line definition.
//!
//! Parsing is delegated to `clap`, which gives us — for free — `--help`,
//! `--version`, usage errors with exit code 2, **unambiguous subcommand
//! abbreviations** (`au3 deob` → `deobfuscate`) and **aliases**
//! (`au3 fmt` → `pretty`).
//!
//! Each subcommand's own arguments live next to its implementation in
//! [`crate::commands`]; this module only wires them into the registry.

use clap::{Parser, Subcommand};

use crate::commands;

/// AutoIt v3 analysis toolkit.
#[derive(Parser, Debug)]
#[command(
    name = "au3",
    version,
    about = "AutoIt v3 analysis toolkit: parse, format, deobfuscate and run scripts",
    // `au3 deob`, `au3 pars`, ... resolve as long as the prefix is unambiguous.
    infer_subcommands = true,
    subcommand_required = true,
    arg_required_else_help = true,
    propagate_version = true,
)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The available subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Parse a script and report top-level item / function counts
    #[command(visible_aliases = ["p", "check"])]
    Parse(commands::parse::ParseArgs),

    /// Re-print normalised source, preserving comments
    #[command(visible_aliases = ["fmt", "format"])]
    Pretty(commands::pretty::PrettyArgs),

    /// Constant-fold, resolve the function table, rename identifiers
    #[command(visible_aliases = ["deobf", "deob"])]
    Deobfuscate(commands::deobfuscate::DeobfuscateArgs),

    /// Run the script body and inline the table values it computed
    #[command(visible_aliases = ["eval", "e"])]
    Evaluate(commands::evaluate::EvaluateArgs),

    /// Interpret one function (probes the obfuscator's builders)
    #[command(visible_aliases = ["r", "exec"])]
    Run(commands::run::RunArgs),

    /// Load a script and debug it: breakpoints, stepping, a prompt
    #[command(visible_aliases = ["dbg"])]
    Debug(commands::debug::DebugArgs),

    /// Decode a resource-packed payload out of a directory or a PE image
    #[command(visible_aliases = ["unp"])]
    Unpack(commands::unpack::UnpackArgs),
}
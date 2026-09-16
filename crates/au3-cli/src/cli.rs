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
    /// Language for messages, help and diagnostics
    ///
    /// `auto` follows the environment and then the host: `AU3_LANG`, then
    /// `LC_ALL` / `LC_MESSAGES` / `LANG` (`zh*` is Chinese, anything else
    /// English), and on Windows the user's UI language when none of those is
    /// set. `en` and `zh-CN` force one instead. Defaults to `auto`.
    #[arg(long, global = true, value_name = "LANG", default_value = "auto",
          value_parser = clap::builder::PossibleValuesParser::new(["auto", "en", "zh-CN"]))]
    pub lang: String,

    /// Report the `#AutoIt3Wrapper_*` settings the script carries
    ///
    /// The wrapper consumed them at build time — they are where a build's
    /// resources, version info, x64 stub and UPX packing came from — so nothing
    /// acts on them, but they are the build's fingerprint: which packer knobs
    /// produced the thing being analysed. Off by default.
    #[arg(long = "wrapper-notes", global = true)]
    pub wrapper_notes: bool,

    /// (internal) Attach to the console of the process that started us
    ///
    /// The elevated copy an `#RequireAdmin` run starts is given a console of
    /// its own by the shell, so the script's output would appear in a second
    /// window. The launcher passes its own process id so the copy can attach
    /// to the console the command was typed in. Not meant to be used by hand:
    /// see `autoitv3-platform`'s `elevate` module.
    #[arg(long = "attach-console", value_name = "PID", hide = true)]
    pub attach_console: Option<u32>,

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

    /// Run a script, or call one function from it (probes the obfuscator's
    /// builders)
    #[command(visible_aliases = ["r", "exec"])]
    Run(commands::run::RunArgs),

    /// Load a script and debug it: breakpoints, stepping, a prompt
    #[command(visible_aliases = ["dbg"])]
    Debug(commands::debug::DebugArgs),

    /// Recover what a build carries: its resource files (the default), its
    /// compiled script (`--script`) or a resource-packed payload (`--payload`)
    #[command(visible_aliases = ["unp"])]
    Unpack(commands::unpack::UnpackArgs),
}
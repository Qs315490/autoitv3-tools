//! Subcommand registry and dispatch.
//!
//! Subcommands are plain first-level words (no `--` prefix), one module each:
//!
//! | command | module | purpose |
//! |---|---|---|
//! | `parse` | [`parse`] | validate a script and report its shape |
//! | `pretty` | [`pretty`] | re-print normalised source (keeps comments) |
//! | `deobfuscate` | [`deobfuscate`] | run the deobfuscation pipeline |
//! | `run` | [`run`] | interpret a function |

pub mod deobfuscate;
pub mod parse;
pub mod pretty;
pub mod run;

use crate::args::{CliError, CliResult};

/// Help text shown for `au3 help`, `au3 --help`, or a bare `au3`.
pub const USAGE: &str = "\
au3 — AutoIt v3 analysis toolkit

USAGE:
    au3 <COMMAND> [OPTIONS] <FILE.au3>

COMMANDS:
    parse        Parse a script and report top-level item / function counts
    pretty       Re-print normalised source, preserving comments
    deobfuscate  Constant-fold, resolve the function table, rename identifiers
    run          Interpret one function (probes the obfuscator's builders)
    help         Show this help

OUTPUT:
    -o FILE      Write the result to FILE instead of stdout
    -o -         Write to stdout explicitly
    The input file is never modified.

EXAMPLES:
    au3 parse      obfuscated.au3
    au3 pretty     obfuscated.au3 -o clean.au3
    au3 deobfuscate obfuscated.au3 -o clean.au3
    au3 run BuildFunctionTable --init obfuscated.au3
    au3 run Add --arg 2 --arg 3 script.au3";

/// Run the subcommand named by `argv[0]`.
///
/// `argv` excludes the program name. An empty invocation, `help`, `-h` and
/// `--help` all print [`USAGE`] and succeed; a `-h`/`--help` inside a
/// subcommand prints the same text rather than running it.
pub fn dispatch(argv: &[String]) -> CliResult<()> {
    let Some(command) = argv.first() else {
        println!("{USAGE}");
        return Ok(());
    };

    let rest = &argv[1..];

    if matches!(command.as_str(), "help" | "-h" | "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    if rest.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }

    match command.as_str() {
        "parse" => parse::run(rest),
        "pretty" => pretty::run(rest),
        "deobfuscate" => deobfuscate::run(rest),
        "run" => run::run(rest),
        other => Err(CliError::usage(format!(
            "unknown command `{other}` (try `au3 help`)"
        ))),
    }
}
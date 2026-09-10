//! `au3 evaluate <FILE> [-o FILE] [--faithful]` — run the script, inline results.
//!
//! The syntactic passes cannot recover the obfuscator's *string* table: it is
//! produced by running generated code. This command executes the script's
//! top-level body on the interpreter and replaces every constant-indexed table
//! reference with the value that actually came out, which turns
//! `$string_table[0xc03]` into the string the script built.
//!
//! A real script reaches the operating system (`DllCall`, registry, GUI)
//! long before it finishes, and that boundary is reported rather than hidden —
//! but the tables are built early, so a partial run is still useful. Off
//! Windows the Windows emulation layer (`--win-version`, default `win10`) moves
//! that boundary back: version queries and registry reads are answered from the
//! emulated machine instead of stopping the run.
//!
//! ```text
//! au3 evaluate sample.au3 --win-version win11 -o resolved.au3
//! au3 evaluate sample.au3 --no-win-emu     # stop at the first Windows call
//! ```

use autoitv3_deobf::evaluate_with_platform;
use autoitv3_format::PrettyPrinter;
use autoitv3_runtime::ExecutionProfile;
use clap::Args;

use crate::args::{load_program, CliResult, OutputArgs, WinEmuArgs};
use crate::output::write_output;

/// Arguments for `au3 evaluate`.
#[derive(Args, Debug)]
pub struct EvaluateArgs {
    /// Input AutoIt v3 script
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Run with AutoIt semantics (real delays, entropy, side effects) instead
    /// of the deterministic analysis profile
    #[arg(long)]
    pub faithful: bool,

    #[command(flatten)]
    pub win: WinEmuArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// The profile `evaluate` uses unless `--faithful` is given.
pub fn profile(faithful: bool) -> ExecutionProfile {
    if faithful {
        ExecutionProfile::faithful()
    } else {
        ExecutionProfile::deterministic()
    }
}

/// Report an evaluation outcome to stderr.
pub fn report(outcome: &autoitv3_deobf::EvaluateReport) {
    eprintln!(
        "evaluated: {} globals, {} tables, {} values inlined, {} calls resolved",
        outcome.globals, outcome.tables, outcome.substitutions, outcome.calls_resolved
    );
    match (&outcome.stopped, outcome.completed) {
        (Some(why), _) => {
            eprintln!("script body did not finish: {why}");
            if why.contains("undefined function") {
                // The usual reason: the script reached the operating system,
                // which is exactly where a Windows platform layer would go.
                eprintln!(
                    "  (that is the platform boundary: this function is not implemented for the current OS)"
                );
            }
            eprintln!("  values produced before that point were still inlined");
        }
        (None, false) => eprintln!("script body stopped early (Exit)"),
        (None, true) => eprintln!("script body ran to completion"),
    }
}

/// Entry point for the `evaluate` subcommand.
pub fn run(args: &EvaluateArgs) -> CliResult<()> {
    let mut prog = load_program(&args.input)?;

    let outcome = evaluate_with_platform(&mut prog, profile(args.faithful), args.win.platform()?);
    report(&outcome);

    let mut printer = PrettyPrinter::new().strip_comments(true);
    let rendered = printer.print_program(&prog);

    write_output(args.output.output.as_deref(), &rendered)
}
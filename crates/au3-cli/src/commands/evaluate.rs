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

use autoitv3_deobf::{evaluate_with_debugger, evaluate_with_options, SubstituteOptions};
use autoitv3_format::PrettyPrinter;
use clap::Args;

use crate::args::{
    load_input, CliResult, CompiledArgs, EffectArgs, OutputArgs, ProfileArgs, ProgressArgs,
    StepArgs, SubstituteArgs, WinEmuArgs,
};
use crate::output::write_output;
use crate::progress::reporter;
use std::path::Path;

/// Arguments for `au3 evaluate`.
#[derive(Args, Debug)]
pub struct EvaluateArgs {
    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Execution semantics (see `ProfileArgs`).
    #[command(flatten)]
    pub profile: ProfileArgs,

    /// Per-effect allow/deny overrides (see `EffectArgs`).
    #[command(flatten)]
    pub effects: EffectArgs,

    /// Interpreter step budget (see `StepArgs`).
    #[command(flatten)]
    pub steps: StepArgs,

    /// Substitution knobs (see `SubstituteArgs`).
    #[command(flatten)]
    pub substitute: SubstituteArgs,

    /// `@Compiled` selection (see `CompiledArgs`); the input decides by
    /// default, so a build is evaluated on its compiled side.
    #[command(flatten)]
    pub compiled: CompiledArgs,

    /// Progress-heartbeat control (see `ProgressArgs`).
    #[command(flatten)]
    pub progress: ProgressArgs,

    #[command(flatten)]
    pub win: WinEmuArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Report an evaluation outcome to stderr.
pub fn report(outcome: &autoitv3_deobf::EvaluateReport) {
    eprintln!(
        "evaluated: {} globals, {} tables, {} values inlined, {} calls resolved",
        outcome.globals, outcome.tables, outcome.substitutions, outcome.calls_resolved
    );
    if outcome.declarations_resolved > 0 {
        eprintln!(
            "  {} table declaration(s) rewritten as literal values",
            outcome.declarations_resolved
        );
    }
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
    let input = load_input(&args.input)?;
    let mut prog = input.program;

    let profile = args.effects.apply(args.profile.profile())?;
    let platform = args
        .win
        .platform(Some(Path::new(&args.input)), input.resource_module.as_deref())?;
    let options = SubstituteOptions {
        inline_declarations: args.substitute.inline_tables,
    };
    // A build's script saw `@Compiled = 1`; evaluating it as a source script
    // would take the wrong branch wherever the macro is tested.
    let compiled = args.compiled.resolve(input.resource_module.is_some());
    let outcome = match reporter(args.progress.no_progress) {
        Some(debugger) => evaluate_with_debugger(
            &mut prog,
            profile,
            platform,
            options,
            args.steps.max_steps,
            compiled,
            debugger,
        ),
        None => evaluate_with_options(
            &mut prog,
            profile,
            platform,
            options,
            args.steps.max_steps,
            compiled,
        ),
    };
    report(&outcome);

    let mut printer = PrettyPrinter::new().strip_comments(true);
    let rendered = printer.print_program(&prog);

    write_output(args.output.output.as_deref(), &rendered)
}
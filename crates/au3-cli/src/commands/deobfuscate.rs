//! `au3 deobfuscate <FILE> [-o FILE] [--rename]` — run the deobfuscation
//! pipeline.
//!
//! Pipeline (see `autoitv3-deobf`): constant folding, function-table
//! resolution (`$table[0x..](...)` → `FuncName(...)`) and indirect-call
//! simplification (`Call("Foo", ...)` → `Foo(...)`). Comments are stripped,
//! since they are noise once the code has been rewritten. A summary goes to
//! stderr so stdout stays a clean AutoIt program.
//!
//! Renaming is opt-in (`--rename`). By default every original variable and
//! function name is preserved, so the output still lines up with the input —
//! and with anything else that refers to it. `--rename` adds the deterministic
//! `$l_str_003` / `f042` aliases on top.

use autoitv3_deobf::{
    evaluate_with_debugger, evaluate_with_options, DeobfReport, Deobfuscator, SubstituteOptions,
};
use autoitv3_format::PrettyPrinter;
use autoitv3_i18n::{msg, tr};
use clap::Args;

use crate::args::{
    CliResult, CompiledArgs, EffectArgs, IncludeArgs, OutputArgs, ProfileArgs, ProgressArgs,
    StepArgs, SubstituteArgs, WinEmuArgs, load_input_included,
};

use crate::output::write_output;
use crate::progress::reporter;
use std::path::Path;

/// Arguments for `au3 deobfuscate`.
#[derive(Args, Debug)]
pub struct DeobfuscateArgs {
    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Run the script body first and inline the table values it computed,
    /// then apply the syntactic passes
    #[arg(long)]
    pub evaluate: bool,

    /// Execution semantics (see `ProfileArgs`); only used with --evaluate.
    #[command(flatten)]
    pub profile: ProfileArgs,

    /// Per-effect allow/deny overrides (see `EffectArgs`); only used with
    /// --evaluate.
    #[command(flatten)]
    pub effects: EffectArgs,

    /// Rename identifiers: give every script-defined variable and function a
    /// deterministic `$l_str_003` / `f042` alias. Off by default, so the output
    /// keeps the names the script was written with
    #[arg(long)]
    pub rename: bool,

    /// Deprecated: renaming is off by default now, so this does nothing
    #[arg(long, hide = true)]
    pub no_rename: bool,

    /// Name of the function-table variable (with or without `$`). Detected
    /// from the program's shape when omitted
    #[arg(long, value_name = "NAME")]
    pub table_var: Option<String>,

    /// Name of the function that builds the function table. Detected from the
    /// program's shape when omitted
    #[arg(long, value_name = "NAME")]
    pub table_builder: Option<String>,

    /// Substitution knobs (see `SubstituteArgs`); only used with --evaluate.
    #[command(flatten)]
    pub substitute: SubstituteArgs,

    /// Interpreter step budget (see `StepArgs`), for both --evaluate and the
    /// function-table pass.
    #[command(flatten)]
    pub steps: StepArgs,

    /// Progress-heartbeat control (see `ProgressArgs`); only used with
    /// --evaluate.
    #[command(flatten)]
    pub progress: ProgressArgs,

    /// `@Compiled` selection (see `CompiledArgs`); the input decides by
    /// default, so a build is evaluated on its compiled side.
    #[command(flatten)]
    pub compiled: CompiledArgs,

    /// `#include` search path (see `IncludeArgs`).
    #[command(flatten)]
    pub includes: IncludeArgs,

    #[command(flatten)]
    pub win: WinEmuArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `deobfuscate` subcommand.
pub fn run(args: &DeobfuscateArgs) -> CliResult<()> {
    let input = load_input_included(&args.input, &args.includes)?;
    let mut prog = input.program;

    // Runtime evaluation first: it recovers values the syntactic passes cannot
    // see (the obfuscator's string table), and the later passes then fold and
    // rename the result.
    let options = SubstituteOptions {
        inline_declarations: args.substitute.inline_tables,
    };
    let mut tables = None;
    if args.evaluate {
        let profile = args.effects.apply(args.profile.profile())?;
        // Evaluation here is analysis, not a run: never open a window, not
        // even the native one a Windows host would pick by itself.
        let platform = args.win.platform(
            Some(Path::new(&args.input)),
            input.resource_module.as_deref(),
            Some(Box::new(autoitv3_platform::winemu::HeadlessBackend::new())),
        )?;
        // A build's script saw `@Compiled = 1`; evaluating it as a source
        // script would take the wrong branch wherever the macro is tested.
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
        super::evaluate::report(&outcome);
        tables = Some(outcome.values);
    }

    let mut deobf = if args.rename {
        Deobfuscator::renaming()
    } else {
        Deobfuscator::new()
    };
    if let (Some(table_var), Some(builder)) = (&args.table_var, &args.table_builder) {
        deobf = deobf.with_function_table(table_var.clone(), builder.clone());
    }
    deobf = deobf.with_max_steps(args.steps.max_steps);
    let mut report = DeobfReport::default();
    match &tables {
        // The simplifier splices `Execute("...")` strings into the program as
        // real code, and that code reads the same tables the run produced. Do
        // the passes up to and including `Simplify`, substitute once more, then
        // finish — otherwise the spliced code keeps its `$table[i]` references.
        Some(values) => {
            let split = deobf.after_simplify();
            deobf.run_passes(&mut prog, &deobf.passes[..split], &mut report);
            let again = values.substitute_with(&mut prog, options);
            if again.total() > 0 {
                eprintln!(
                    "{}",
                    msg!(
                        "  {more} more values inlined in code the simplifier spliced",
                        more = again.total()
                    )
                );
            }
            deobf.run_passes(&mut prog, &deobf.passes[split..], &mut report);
        }
        None => deobf.run_passes(&mut prog, &deobf.passes, &mut report),
    }
    let renamed = if args.rename { "" } else { tr(" (renaming off; pass --rename)") };
    eprintln!(
        "{}",
        msg!(
            "deobfuscated: {folds} folds, {vars} vars, {funcs} funcs renamed{renamed}; table: {entries} entries, {calls} calls, {refs} refs; {simplified} indirect calls simplified",
            folds = report.folds,
            vars = report.renamed.vars,
            funcs = report.renamed.funcs,
            renamed = renamed,
            entries = report.table.entries,
            calls = report.table.calls,
            refs = report.table.refs,
            simplified = report.simplified.calls + report.simplified.executes
        )
    );

    if !args.evaluate {
        // Say why the output still shows `Global Const $t = Build()`: those are
        // the runtime-built tables, and only a run can resolve them.
        let computed = computed_globals(&prog);
        if computed > 0 {
            eprintln!(
                "{}",
                msg!(
                    "note: {computed} global(s) are built by a function call at load time; re-run with --evaluate to run the script body and inline their values",
                    computed = computed
                )
            );
        }
    }

    let mut printer = PrettyPrinter::new().strip_comments(true);
    let rendered = printer.print_program(&prog);

    write_output(args.output.output.as_deref(), &rendered)
}
/// Globals initialised by calling a function the script defines — the shape a
/// runtime-built table takes (`Global Const $strings = DecodeResources()`).
///
/// Only `--evaluate` can turn those into values, because the value depends on
/// what the call computes.
fn computed_globals(prog: &autoitv3_ast::ast::Program) -> usize {
    use autoitv3_ast::ast::{Expr, ExprKind, ItemKind, StmtKind, VarKind};
    use std::collections::HashSet;

    let defined: HashSet<String> = prog
        .items
        .iter()
        .filter_map(|i| match &i.kind {
            ItemKind::Func(f) => Some(f.name.name.to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    let mut count = 0;
    for item in &prog.items {
        let ItemKind::Stmt(s) = &item.kind else {
            continue;
        };
        let StmtKind::VarDecl(v) = &s.kind else {
            continue;
        };
        if !matches!(v.kind, VarKind::Global) {
            continue;
        }
        for decl in &v.vars {
            if let Some(Expr {
                kind: ExprKind::Call(c),
                ..
            }) = &decl.init
            {
                if defined.contains(&c.callee.name.to_ascii_lowercase()) {
                    count += 1;
                }
            }
        }
    }
    count
}

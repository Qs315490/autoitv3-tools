//! `au3 deobfuscate <FILE> [-o FILE] [--rename]` — run the deobfuscation
//! pipeline.
//!
//! Pipeline (see `autoitv3-deobf`): constant folding, function-table
//! resolution (`$fn_table[0x..](...)` → `FuncName(...)`) and indirect-call
//! simplification (`Call("Foo", ...)` → `Foo(...)`). Comments are stripped,
//! since they are noise once the code has been rewritten. A summary goes to
//! stderr so stdout stays a clean AutoIt program.
//!
//! Renaming is opt-in (`--rename`). By default every original variable and
//! function name is preserved, so the output still lines up with the input —
//! and with anything else that refers to it. `--rename` adds the deterministic
//! `$l_str_003` / `f042` aliases on top.

use autoitv3_deobf::{
    evaluate_with_options, DeobfReport, Deobfuscator, SubstituteOptions,
};
use autoitv3_format::PrettyPrinter;
use clap::Args;

use crate::args::{load_program, CliResult, OutputArgs, WinEmuArgs};
use crate::output::write_output;

/// Arguments for `au3 deobfuscate`.
#[derive(Args, Debug)]
pub struct DeobfuscateArgs {
    /// Input AutoIt v3 script
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Run the script body first and inline the table values it computed,
    /// then apply the syntactic passes
    #[arg(long)]
    pub evaluate: bool,

    /// With --evaluate, run with AutoIt semantics instead of the
    /// deterministic analysis profile
    #[arg(long)]
    pub faithful: bool,

    /// Rename identifiers: give every script-defined variable and function a
    /// deterministic `$l_str_003` / `f042` alias. Off by default, so the output
    /// keeps the names the script was written with
    #[arg(long)]
    pub rename: bool,

    /// Deprecated: renaming is off by default now, so this does nothing
    #[arg(long, hide = true)]
    pub no_rename: bool,

    /// With --evaluate, also rewrite `Global Const $t = Build()` into the
    /// table's literal value (otherwise the declaration keeps its call)
    #[arg(long)]
    pub inline_tables: bool,

    #[command(flatten)]
    pub win: WinEmuArgs,

    #[command(flatten)]
    pub output: OutputArgs,
}

/// Entry point for the `deobfuscate` subcommand.
pub fn run(args: &DeobfuscateArgs) -> CliResult<()> {
    let mut prog = load_program(&args.input)?;

    // Runtime evaluation first: it recovers values the syntactic passes cannot
    // see (the obfuscator's string table), and the later passes then fold and
    // rename the result.
    let options = SubstituteOptions {
        inline_declarations: args.inline_tables,
    };
    let mut tables = None;
    if args.evaluate {
        let outcome = evaluate_with_options(
            &mut prog,
            super::evaluate::profile(args.faithful),
            args.win.platform()?,
            options,
        );
        super::evaluate::report(&outcome);
        tables = Some(outcome.values);
    }

    let deobf = if args.rename {
        Deobfuscator::renaming()
    } else {
        Deobfuscator::new()
    };
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
                    "  {} more values inlined in code the simplifier spliced",
                    again.total()
                );
            }
            deobf.run_passes(&mut prog, &deobf.passes[split..], &mut report);
        }
        None => deobf.run_passes(&mut prog, &deobf.passes, &mut report),
    }
    eprintln!(
        "deobfuscated: {} folds, {} vars, {} funcs renamed{}; \
         table: {} entries, {} calls, {} refs; {} indirect calls simplified",
        report.folds,
        report.renamed.vars,
        report.renamed.funcs,
        if args.rename { "" } else { " (renaming off; pass --rename)" },
        report.table.entries,
        report.table.calls,
        report.table.refs,
        report.simplified.calls + report.simplified.executes,
    );

    if !args.evaluate {
        // Say why the output still shows `Global Const $t = Build()`: those are
        // the runtime-built tables, and only a run can resolve them.
        let computed = computed_globals(&prog);
        if computed > 0 {
            eprintln!(
                "note: {computed} global(s) are built by a function call at load time; \
                 re-run with --evaluate to run the script body and inline their values"
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

//! Orchestrator: runs the deobfuscation pipeline over a program.

use autoitv3_ast::ast::Program;

use crate::fold;
use crate::rename;
use crate::simplify;
use crate::table;

/// Summary of what a deobfuscation run did.
#[derive(Debug, Clone, Default)]
pub struct DeobfReport {
    /// Number of constant-folded expressions.
    pub folds: usize,
    /// Function-table resolution stats.
    pub table: TableCount,
    /// Indirect-call simplification stats.
    pub simplified: SimplifyCount,
    /// Number of renamed identifiers.
    pub renamed: RenameCount,
}

/// Counts of renamed identifiers by kind.
#[derive(Debug, Clone, Default)]
pub struct RenameCount {
    pub vars: usize,
    pub funcs: usize,
}

/// Counts for the indirect-call simplification pass.
#[derive(Debug, Clone, Default)]
pub struct SimplifyCount {
    /// `Call("Foo", ...)` rewritten to `Foo(...)`.
    pub calls: usize,
    /// `Execute("Foo(...)")` rewritten to `Foo(...)`.
    pub executes: usize,
}

impl SimplifyCount {
    /// Total number of indirect calls rewritten.
    pub fn total(&self) -> usize {
        self.calls + self.executes
    }
}

/// Counts for the function-table resolution pass.
#[derive(Debug, Clone, Default)]
pub struct TableCount {
    /// Number of `$fn_table[...](...)` indexed calls rewritten to plain calls.
    pub calls: usize,
    /// Number of `$fn_table[...]` indexed references rewritten to identifiers.
    pub refs: usize,
    /// Number of function entries resolved in the table.
    pub entries: usize,
}

/// The ordered set of deobfuscation passes to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Fold,
    /// Resolve the obfuscator's function table (`$fn_table[i](...)`).
    Table,
    /// Turn `Call("Foo", ...)` / `Execute("Foo(...)")` into direct calls.
    Simplify,
    Rename,
}

impl Pass {
    /// Every pass, in pipeline order.
    ///
    /// `Simplify` runs **before** `Table`: splicing an `Execute` string into the
    /// program is what exposes `$FN_TABLE[1094](...)` as ordinary code, which the
    /// table pass then resolves to a real name. `Rename` comes last so
    /// everything the earlier passes produced is renamed consistently.
    pub const ALL: &'static [Pass] = &[Pass::Fold, Pass::Simplify, Pass::Table, Pass::Rename];

    /// The default pipeline: everything except [`Pass::Rename`].
    ///
    /// Folding and table resolution change the *structure* of the program;
    /// renaming changes its *names*, which is what makes the output impossible
    /// to line up with the input. So renaming is opt-in — see
    /// [`Deobfuscator::renaming`].
    pub const DEFAULT: &'static [Pass] = &[Pass::Fold, Pass::Simplify, Pass::Table];
}

/// A deobfuscator configured with a set of passes.
///
/// [`new`](Self::new) is folding, simplification and function-table resolution:
/// the passes that change the program's structure, leaving every original name
/// alone. [`renaming`](Self::renaming) adds the rename pass, and
/// [`rename`](Self::rename) narrows which categories it may touch.
#[derive(Debug, Clone)]
pub struct Deobfuscator {
    pub passes: Vec<Pass>,
    /// Which categories the rename pass may touch.
    pub rename: rename::RenameOptions,
}

impl Default for Deobfuscator {
    fn default() -> Self {
        Self::new()
    }
}

impl Deobfuscator {
    /// The default pipeline: fold, simplify, resolve the function table.
    ///
    /// No renaming, so the output still lines up with the input.
    pub fn new() -> Self {
        Self {
            passes: Pass::DEFAULT.to_vec(),
            rename: rename::RenameOptions::none(),
        }
    }

    /// The full pipeline, including deterministic identifier renaming.
    pub fn renaming() -> Self {
        Self {
            passes: Pass::ALL.to_vec(),
            rename: rename::RenameOptions::all(),
        }
    }

    /// Same pipeline, with the rename pass limited to `options`.
    pub fn with_rename_options(mut self, options: rename::RenameOptions) -> Self {
        self.rename = options;
        self
    }

    /// Run the configured pipeline over `prog`, mutating it in place.
    pub fn run(&self, prog: &mut Program) -> DeobfReport {
        let mut report = DeobfReport::default();
        self.run_passes(prog, &self.passes, &mut report);
        report
    }

    /// Run `passes` — a prefix or any slice of the configured pipeline —
    /// accumulating into `report`.
    ///
    /// Lets a caller run the pipeline in two halves and slip its own step in
    /// between; see [`Deobfuscator::after_simplify`].
    pub fn run_passes(&self, prog: &mut Program, passes: &[Pass], report: &mut DeobfReport) {
        for pass in passes {
            match pass {
                Pass::Fold => report.folds += fold::fold_program(prog),
                Pass::Table => {
                    let r = table::resolve_function_table(prog, "fn_table", "BuildFunctionTable");
                    report.table.calls += r.calls_rewritten;
                    report.table.refs += r.refs_rewritten;
                    report.table.entries += r.entries;
                }
                Pass::Simplify => {
                    let r = simplify::simplify_program(prog);
                    report.simplified.calls += r.calls;
                    report.simplified.executes += r.executes;
                }
                Pass::Rename => {
                    let r = rename::rename_program_with(prog, self.rename);
                    report.renamed.vars += r.vars;
                    report.renamed.funcs += r.funcs;
                }
            }
        }
    }

    /// Where [`Pass::Simplify`] sits in the configured pipeline, plus one.
    ///
    /// `Simplify` is the pass that turns `Execute("...")` strings into real
    /// code, so a caller that inlined table values *before* the pipeline (see
    /// `evaluate`) has to substitute again after this point or the spliced code
    /// keeps its `$table[i]` references.
    pub fn after_simplify(&self) -> usize {
        self.passes
            .iter()
            .rposition(|p| *p == Pass::Simplify)
            .map_or(0, |i| i + 1)
    }
}

/// Convenience: run the default pipeline — folding, simplification and
/// function-table resolution, with names left as written.
///
/// Use [`Deobfuscator::renaming`] for the full pipeline.
pub fn deobfuscate(prog: &mut Program) -> DeobfReport {
    Deobfuscator::new().run(prog)
}

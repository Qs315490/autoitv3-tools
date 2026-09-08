//! Orchestrator: runs the deobfuscation pipeline over a program.

use autoitv3_ast::ast::Program;

use crate::fold;
use crate::rename;

/// Summary of what a deobfuscation run did.
#[derive(Debug, Clone, Default)]
pub struct DeobfReport {
    /// Number of constant-folded expressions.
    pub folds: usize,
    /// Number of renamed identifiers.
    pub renamed: RenameCount,
}

/// Counts of renamed identifiers by kind.
#[derive(Debug, Clone, Default)]
pub struct RenameCount {
    pub vars: usize,
    pub funcs: usize,
    pub macros: usize,
}

/// The ordered set of deobfuscation passes to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    Fold,
    Rename,
}

impl Pass {
    pub const ALL: &'static [Pass] = &[Pass::Fold, Pass::Rename];
}

/// A deobfuscator configured with a set of passes.
#[derive(Debug, Clone, Default)]
pub struct Deobfuscator {
    pub passes: Vec<Pass>,
}

impl Deobfuscator {
    pub fn new() -> Self {
        Self {
            passes: Pass::ALL.to_vec(),
        }
    }

    /// Run the configured pipeline over `prog`, mutating it in place.
    pub fn run(&self, prog: &mut Program) -> DeobfReport {
        let mut report = DeobfReport::default();
        for pass in &self.passes {
            match pass {
                Pass::Fold => report.folds += fold::fold_program(prog),
                Pass::Rename => {
                    let r = rename::rename_program(prog);
                    report.renamed.vars += r.vars;
                    report.renamed.funcs += r.funcs;
                    report.renamed.macros += r.macros;
                }
            }
        }
        report
    }
}

/// Convenience: parse-less pipeline entry that applies the default passes.
pub fn deobfuscate(prog: &mut Program) -> DeobfReport {
    Deobfuscator::new().run(prog)
}
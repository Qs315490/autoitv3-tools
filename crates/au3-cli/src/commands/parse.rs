//! `au3 parse <file.au3>` — parse the script and report its shape.
//!
//! This is the cheapest command: it proves the file is syntactically valid
//! AutoIt v3 and summarises what was found, without emitting any source.

use autoitv3_ast::ast::ItemKind;

use crate::args::{load_program, single_input, CliResult};

/// Entry point for the `parse` subcommand.
pub fn run(args: &[String]) -> CliResult<()> {
    let input = single_input("parse", args)?;
    let prog = load_program(&input)?;

    let funcs = prog
        .items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count();

    println!(
        "parsed OK: {} top-level items, {} functions",
        prog.items.len(),
        funcs
    );
    Ok(())
}
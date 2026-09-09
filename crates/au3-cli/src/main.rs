//! Command line entry point for the AutoIt v3 analysis tool.
//!
//! Usage:
//!   au3 <file.au3>                # parse, report stats
//!   au3 --pretty <file.au3>       # parse then pretty-print (normalize)
//!   au3 --deobfuscate <file.au3>   # constant-fold + rename, then pretty-print

use autoitv3_ast::{parse, pretty::PrettyPrinter};
use autoitv3_deobf::deobfuscate;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: au3 [--pretty | --deobfuscate] <file.au3>");
        std::process::exit(2);
    }

    let pretty = args.iter().any(|a| a == "--pretty");
    let deobfuscate_flag = args.iter().any(|a| a == "--deobfuscate");
    let file = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with('-'))
        .cloned()
        .expect("missing input file");

    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {file}: {e}");
            std::process::exit(2);
        }
    };

    let mut prog = match parse(&src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    };

    if deobfuscate_flag {
        let report = deobfuscate(&mut prog);
        eprintln!(
            "deobfuscated: {} folds, {} vars, {} funcs, {} macros renamed",
            report.folds, report.renamed.vars, report.renamed.funcs, report.renamed.macros
        );
    }

    // Deobfuscation output strips comments; plain pretty output preserves them.
    let strip = deobfuscate_flag;
    let mut pp = PrettyPrinter::new().strip_comments(strip);
    let out = pp.print_program(&prog);

    if pretty || deobfuscate_flag {
        println!("{out}");
    } else {
        let funcs = count_funcs(&prog);
        println!(
            "parsed OK: {} top-level items, {} functions",
            prog.items.len(),
            funcs
        );
    }
}

fn count_funcs(prog: &autoitv3_ast::Program) -> usize {
    use autoitv3_ast::ast::ItemKind;
    prog.items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count()
}

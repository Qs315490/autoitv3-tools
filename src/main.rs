//! Command line entry point for the AutoIt v3 AST analysis tool.
//!
//! Usage:
//!   au3-parser <file.au3>              # parse, report stats, dump AST
//!   au3-parser --pretty <file.au3>     # parse then pretty-print (deobfuscate)

use au3_parser::{parse, PrettyPrinter};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: au3-parser [--pretty] <file.au3>");
        std::process::exit(2);
    }

    let pretty = args.iter().any(|a| a == "--pretty");
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

    match parse(&src) {
        Ok(prog) => {
            if pretty {
                let mut pp = PrettyPrinter::new();
                let out = pp.print_program(&prog);
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
        Err(e) => {
            eprintln!("parse error: {e}");
            std::process::exit(1);
        }
    }
}

fn count_funcs(prog: &au3_parser::Program) -> usize {
    use au3_parser::ast::ItemKind;
    prog.items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count()
}
//! Command line entry point for the AutoIt v3 analysis tool.
//!
//! Usage:
//!   au3 <file.au3>                    # parse, report stats
//!   au3 --pretty <file.au3>           # parse then pretty-print (normalize)
//!   au3 --deobfuscate <file.au3>      # constant-fold + rename, then pretty-print
//!   au3 --pretty -o out.au3 <file>    # write output to a file
//!   au3 --pretty -o - <file>          # write output to stdout (explicit)
//!
//! `-o FILE` redirects the formatted output to FILE. `-o -` (or omitting `-o`)
//! writes to stdout, so the original input file is never modified.

use std::io::Write;
use autoitv3_ast::{parse, Program};
use autoitv3_format::PrettyPrinter;
use autoitv3_deobf::deobfuscate;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut pretty = false;
    let mut deobfuscate_flag = false;
    let mut out_file: Option<String> = None;
    let mut file: Option<String> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--pretty" => pretty = true,
            "--deobfuscate" => deobfuscate_flag = true,
            "-o" | "--output" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: -o requires a FILE argument (use '-' for stdout)");
                    std::process::exit(2);
                }
                out_file = Some(args[i].clone());
            }
            s if s.starts_with('-') => {
                eprintln!("unknown option: {s}");
                std::process::exit(2);
            }
            s => {
                if file.is_some() {
                    eprintln!("error: multiple input files given");
                    std::process::exit(2);
                }
                file = Some(s.to_string());
            }
        }
        i += 1;
    }

    let Some(file) = file else {
        eprintln!("usage: au3 [--pretty | --deobfuscate] [-o FILE] <file.au3>");
        eprintln!("       use -o - to redirect output to stdout explicitly");
        std::process::exit(2);
    };

    let src = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {file}: {e}");
            std::process::exit(2);
        }
    };

    let mut prog: Program = match parse(&src) {
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

    let mut pp = PrettyPrinter::new().strip_comments(deobfuscate_flag);
    let out = pp.print_program(&prog);

    if pretty || deobfuscate_flag {
        match out_file.as_deref() {
            Some("-") | None => {
                print!("{out}");
            }
            Some(path) => {
                if let Err(e) = std::fs::write(path, &out) {
                    eprintln!("error writing {path}: {e}");
                    std::process::exit(2);
                }
            }
        }
    } else {
        // Plain parse mode: only stats, never emits source output.
        let funcs = count_funcs(&prog);
        println!(
            "parsed OK: {} top-level items, {} functions",
            prog.items.len(),
            funcs
        );
        let _ = std::io::stdout().flush();
    }
}

fn count_funcs(prog: &Program) -> usize {
    use autoitv3_ast::ast::ItemKind;
    prog.items
        .iter()
        .filter(|it| matches!(it.kind, ItemKind::Func(_)))
        .count()
}

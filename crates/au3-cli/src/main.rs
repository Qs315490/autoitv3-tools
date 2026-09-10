//! `au3` — command line entry point for the AutoIt v3 analysis toolkit.
//!
//! The binary itself only collects `argv`, hands it to
//! [`commands::dispatch`], and turns a [`CliError`](args::CliError) into an exit
//! code. Each subcommand lives in its own module under [`commands`]:
//!
//! ```text
//! au3 parse       <file.au3>
//! au3 pretty      <file.au3> [-o FILE]
//! au3 deobfuscate <file.au3> [-o FILE]
//! au3 run         <Func> <file.au3> [--arg V]... [--init] [--trace]
//! au3 help
//! ```
//!
//! Run `au3 help` for the full text.

mod args;
mod commands;
mod output;

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    if let Err(err) = commands::dispatch(&argv) {
        eprintln!("error: {}", err.message);
        if err.code == 2 {
            eprintln!();
            eprintln!("{}", commands::USAGE);
        }
        std::process::exit(err.code);
    }
}
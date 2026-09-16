//! `au3` — command line entry point for the AutoIt v3 analysis toolkit.
//!
//! The binary only parses `argv` with `clap`, hands the result to
//! [`commands::dispatch`], and turns a [`CliError`](args::CliError) into an exit
//! code. Each subcommand lives in its own module under [`commands`]:
//!
//! ```text
//! au3 parse       <FILE>
//! au3 pretty      <FILE> [-o FILE]
//! au3 deobfuscate <FILE> [-o FILE]
//! au3 run         <FILE> [FUNC] [--arg V]... [--init] [--trace]
//! au3 debug       <FILE> [-c CMD]...
//! ```
//!
//! Subcommands may be abbreviated when unambiguous (`au3 deob`) and have
//! aliases (`au3 fmt`). Run `au3 --help` for the full text.
//!
//! Exit codes: `0` success, `2` usage errors (reported by `clap`) and IO
//! failures, `1` input processing failures (parse or runtime errors).

mod args;
mod cli;
#[cfg(test)]
mod tests_i18n;
mod commands;
mod elevate;
mod i18n_cli;
mod output;
mod progress;

fn main() {
    // The language must be known before clap renders help or anything else is
    // printed: `--lang` wins, then `AU3_LANG`, then the locale variables.
    autoitv3_i18n::set_lang(i18n_cli::lang_from_args());
    let cli = i18n_cli::parse();

    // The build fingerprint is a diagnostic, not part of the run: it is off
    // unless `--wrapper-notes` asked for it. The note is printed where the
    // script is loaded, well below the flag's owner, so it reads a switch the
    // way the language does.
    args::set_wrapper_notes(cli.wrapper_notes);

    // The elevated copy of an `#RequireAdmin` run is created by the shell
    // service, which hands it a console of its own — the output would open in a
    // second window. Attaching to the launcher's console first puts everything
    // back where the command was typed, including this note.
    if let Some(pid) = cli.attach_console {
        if autoitv3_platform::elevate::attach_console(pid) {
            eprintln!(
                "{}",
                autoitv3_i18n::msg!(
                    "note: #RequireAdmin: elevated, sharing the console of process {pid}",
                    pid = pid
                )
            );
        } else {
            eprintln!(
                "{}",
                autoitv3_i18n::msg!(
                    "note: #RequireAdmin: elevated, but process {pid} has no console to share, \
                     so this output has a window of its own",
                    pid = pid
                )
            );
        }
    }

    if let Err(err) = commands::dispatch(&cli) {
        eprintln!(
            "{}",
            autoitv3_i18n::msg!("error: {message}", message = err.message)
        );
        std::process::exit(err.code);
    }
}
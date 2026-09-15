//! `#RequireAdmin`: running the script in an elevated copy of this process.
//!
//! A script that says `#RequireAdmin` wants full administrator rights. Windows
//! cannot raise the token of a process that is already running — that is the
//! whole point of UAC — so the interpreter's only move, and the one this module
//! makes, is to start a *second* copy of itself through the shell's `runas`
//! verb and let that copy run the script. The original stops before the first
//! statement, exactly as AutoIt does.
//!
//! Two deliberate differences, both because this is a command line tool rather
//! than a double-clicked interpreter:
//!
//! * the original **waits** for the elevated copy and reports the code it
//!   exited with, so a batch file that runs `au3 run` sees the work finish;
//! * `--no-elevate` turns the directive off, and so does denying the `spawn`
//!   effect (`--deny spawn`). The *preset* profiles do not enter into it:
//!   whether the deterministic profile refuses the script's own `Run()` calls
//!   is a different question from which token the script runs with, and a
//!   script that asks for rights gets them (the OS asks the user first).
//!
//! The elevated copy is given the same command line plus `--no-elevate`, so it
//! cannot try to elevate itself again. Off Windows there is no elevation
//! mechanism at all: the directive is read, reported, and the script runs here
//! the way it always did.

use std::ffi::OsString;

use autoitv3_ast::Program;
use autoitv3_platform::elevate::{self, Elevated};
use crate::args::{CliError, CliResult};

/// Whether the program asks for administrator rights.
///
/// Only the top level counts: a `#RequireAdmin` written inside a function is a
/// statement the interpreter steps over, not a directive (see
/// [`autoitv3_preproc::has_directive`]).
pub fn is_required(program: &Program) -> bool {
    autoitv3_preproc::has_directive(program, "RequireAdmin")
}

/// A note about the directive, prefixed so it is not mistaken for output.
fn note(text: &str) {
    eprintln!("note: {text}");
}

/// Honour `#RequireAdmin`, if it applies here.
///
/// Returns `true` when an elevated copy ran the script and this process must
/// stop without executing it. Every reason *not* to elevate is reported once on
/// stderr and leaves the script to run in this process.
///
/// `spawn_denied` is the caller's own decision — `au3 run --deny spawn` — not
/// the preset profile's: that profile refuses what the *script* may do, while
/// this function is about the token the script runs with.
pub fn relaunch_if_required(
    program: &Program,
    no_elevate: bool,
    spawn_denied: bool,
) -> CliResult<bool> {
    if !is_required(program) {
        return Ok(false);
    }
    if no_elevate {
        note("#RequireAdmin: --no-elevate, running without administrator rights");
        return Ok(false);
    }
    if spawn_denied {
        note(
            "#RequireAdmin: --deny spawn, running without administrator rights",
        );
        return Ok(false);
    }
    if elevate::is_admin() {
        // Started from an elevated shell, or UAC is off and the token is the
        // same one: the directive is already satisfied.
        return Ok(false);
    }

    let exe = std::env::current_exe()
        .map_err(|e| CliError::io(format!("cannot find the running executable: {e}")))?;
    let args = elevated_args();
    // Relative script paths have to resolve in the elevated copy too, and the
    // shell is free to pick its own directory when we do not say.
    let dir = std::env::current_dir().ok();
    match elevate::relaunch_elevated(&exe, &args, dir.as_deref()) {
        Ok(Elevated::Finished(code)) => {
            note(&format!(
                "#RequireAdmin: the elevated copy finished with exit code {code}"
            ));
            Ok(true)
        }
        Ok(Elevated::Declined) => {
            note(
                "#RequireAdmin: the elevation prompt was dismissed, \
                 running without administrator rights",
            );
            Ok(false)
        }
        Ok(Elevated::Unsupported) => {
            note(
                "#RequireAdmin: elevation is a Windows mechanism and this host has none, \
                 running without administrator rights",
            );
            Ok(false)
        }
        Err(e) => Err(CliError::failure(format!("#RequireAdmin: {e}"))),
    }
}

/// This process's own arguments, with the two flags the copy needs.
///
/// `--no-elevate` goes right after the subcommand (`au3 run --no-elevate
/// FILE`), so it is still an option wherever the user put a later `--`;
/// `--attach-console PID` goes in front of the subcommand, where the top-level
/// flag lives, and is how the copy finds the console to print into (the callers
/// pid, ours).
fn elevated_args() -> Vec<OsString> {
    let own: Vec<OsString> = std::env::args_os().skip(1).collect();
    elevated_args_from(own, std::process::id())
}

/// [`elevated_args`], with the argument list and pid handed in, so the flag
/// placement can be checked without a real elevation.
fn elevated_args_from(mut args: Vec<OsString>, pid: u32) -> Vec<OsString> {
    if args.len() > 1 {
        args.insert(1, OsString::from("--no-elevate"));
        args.insert(0, OsString::from(pid.to_string()));
        args.insert(0, OsString::from("--attach-console"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use autoitv3_ast::parse;

    #[test]
    fn the_copy_gets_both_flags_where_the_parser_wants_them() {
        // `--attach-console` is a top-level flag, so it goes in front of the
        // subcommand; `--no-elevate` belongs to `run`, so it goes behind it and
        // in front of the file. Both survive a later `--`.
        let own: Vec<OsString> =
            vec!["run".into(), "a script.au3".into(), "--".into(), "-x".into()];
        assert_eq!(
            elevated_args_from(own, 4242),
            vec![
                "--attach-console",
                "4242",
                "run",
                "--no-elevate",
                "a script.au3",
                "--",
                "-x"
            ]
        );
    }

    #[test]
    fn the_directive_is_found_at_the_top_level_only() {
        assert!(is_required(&parse("#RequireAdmin\nGlobal $g = 1\n").unwrap()));
        assert!(is_required(&parse("#requireadmin\n").unwrap()));
        assert!(!is_required(&parse("#NoTrayIcon\n").unwrap()));
        assert!(!is_required(&parse("Func F()\n    #RequireAdmin\nEndFunc\n").unwrap()));
    }
}

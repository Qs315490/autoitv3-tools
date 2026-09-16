//! `#RequireAdmin`: running the script in an elevated copy of this process.
//!
//! A script that says `#RequireAdmin` wants full administrator rights. Windows
//! cannot raise the token of a process that is already running — that is the
//! whole point of UAC — so the interpreter's only move, and the one this module
//! makes, is to start a *second* copy of itself through the shell's `runas`
//! verb and let that copy run the script. The original stops before the first
//! statement, exactly as AutoIt does.
//!
//! When **this process already has the rights the script asks for** there is
//! nothing to do and nothing to say: no second copy, no consent prompt, no
//! note. That is the case when the command was typed at an already elevated
//! prompt, when UAC is off and the token is the same one, and when the process
//! *is* the copy our own launcher started — the check is the interpreter's own
//! [`IsAdmin`](autoitv3_platform::elevate::is_admin), not a guess from flags.
//! An administrator outranks every reason to skip, so `--no-elevate` and
//! `--deny spawn` cannot turn a satisfied directive into a complaint.
//!
//! Where it does apply, two deliberate differences from a double-clicked
//! interpreter, both because this is a command line tool:
//!
//! * the original **waits** for the elevated copy and reports the code it
//!   exited with, so a batch file that runs `au3 run` sees the work finish;
//! * `--no-elevate` turns the directive off, and so does denying the `spawn`
//!   effect (`--deny spawn`). The *preset* profiles do not enter into it:
//!   whether the deterministic profile refuses the script's own `Run()` calls
//!   is a different question from which token the script runs with, and a
//!   script that asks for rights gets them (the OS asks the user first).
//!
//! The elevated copy is given the same command line plus `--elevated-copy`, so
//! it cannot try to elevate itself again. Off Windows there is no elevation
//! mechanism at all: the directive is read, reported, and the script runs here
//! the way it always did.

use std::ffi::OsString;

use autoitv3_ast::Program;
use autoitv3_i18n::{msg, tr};
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
    eprintln!("{}{text}", tr("note: "));
}

/// What `#RequireAdmin` calls for, decided before anything is done about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Run the script here and say nothing: there is no directive, this *is*
    /// the elevated copy, or the process already has the rights it asks for.
    Nothing,
    /// Start an elevated copy and stop this process before it runs a statement.
    Relaunch,
    /// Run here, saying once why the directive is not being honoured.
    Skip(&'static str),
}

/// The decision, as a pure function of the facts — the counterpart of the
/// side effects in [`relaunch_if_required`], and what its tests pin down.
///
/// An administrator wins over every reason to skip: the directive is about
/// having the rights, so when the process has them there is nothing to report
/// and no prompt to raise, whatever else the command line says.
fn action(required: bool, admin: bool, no_elevate: bool, spawn_denied: bool) -> Action {
    if !required || admin {
        return Action::Nothing;
    }
    if no_elevate {
        return Action::Skip("#RequireAdmin: --no-elevate, running without administrator rights");
    }
    if spawn_denied {
        return Action::Skip("#RequireAdmin: --deny spawn, running without administrator rights");
    }
    Action::Relaunch
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
    elevated_copy: bool,
    no_elevate: bool,
    spawn_denied: bool,
) -> CliResult<bool> {
    if !is_required(program) {
        return Ok(false);
    }
    // The copy our own launcher started is elevated by construction, so the OS
    // does not have to be asked. Everything else asks *this* process's token:
    // started from an elevated shell, or UAC is off and the token is the same
    // one, the directive is already satisfied — no consent prompt, and nothing
    // to say about skipping it either.
    let admin = elevated_copy || elevate::is_admin();
    match action(true, admin, no_elevate, spawn_denied) {
        Action::Nothing => return Ok(false),
        Action::Skip(reason) => {
            note(tr(reason));
            return Ok(false);
        }
        Action::Relaunch => {}
    }

    let exe = std::env::current_exe()
        .map_err(|e| CliError::io(msg!("cannot find the running executable: {e}", e = e)))?;
    let args = elevated_args();
    // Relative script paths have to resolve in the elevated copy too, and the
    // shell is free to pick its own directory when we do not say.
    let dir = std::env::current_dir().ok();
    match elevate::relaunch_elevated(&exe, &args, dir.as_deref()) {
        Ok(Elevated::Finished(code)) => {
            note(&msg!(
                "#RequireAdmin: the elevated copy finished with exit code {code}",
                code = code
            ));
            Ok(true)
        }
        Ok(Elevated::Declined) => {
            note(tr(
                "#RequireAdmin: the elevation prompt was dismissed, running without administrator rights",
            ));
            Ok(false)
        }
        Ok(Elevated::Unsupported) => {
            note(tr(
                "#RequireAdmin: elevation is a Windows mechanism and this host has none, running without administrator rights",
            ));
            Ok(false)
        }
        Err(e) => Err(CliError::failure(msg!("#RequireAdmin: {e}", e = e))),
    }
}

/// This process's own arguments, with the two flags the copy needs.
///
/// `--elevated-copy` goes right after the subcommand (`au3 run --elevated-copy
/// FILE`), so it is still an option wherever the user put a later `--`;
/// `--attach-console PID` goes in front of the subcommand, where the top-level
/// flag lives, and is how the copy finds the console to print into (the caller's
/// pid, ours).
fn elevated_args() -> Vec<OsString> {
    let own: Vec<OsString> = std::env::args_os().skip(1).collect();
    elevated_args_from(own, std::process::id())
}

/// [`elevated_args`], with the argument list and pid handed in, so the flag
/// placement can be checked without a real elevation.
fn elevated_args_from(mut args: Vec<OsString>, pid: u32) -> Vec<OsString> {
    if args.len() > 1 {
        args.insert(1, OsString::from("--elevated-copy"));
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
    fn an_administrator_is_never_asked_again() {
        // The user's rule: already an administrator means no request. It also
        // means no note — there is nothing to skip, the rights are there.
        for (no_elevate, spawn_denied) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            assert_eq!(
                action(true, true, no_elevate, spawn_denied),
                Action::Nothing,
                "no_elevate={no_elevate} spawn_denied={spawn_denied}"
            );
        }
    }

    #[test]
    fn a_script_without_the_directive_is_left_alone() {
        assert_eq!(action(false, false, false, false), Action::Nothing);
        // ... even when the command line would otherwise have skipped it.
        assert_eq!(action(false, false, true, false), Action::Nothing);
    }

    #[test]
    fn the_two_switches_skip_with_their_own_reason() {
        assert_eq!(
            action(true, false, true, false),
            Action::Skip("#RequireAdmin: --no-elevate, running without administrator rights")
        );
        assert_eq!(
            action(true, false, false, true),
            Action::Skip("#RequireAdmin: --deny spawn, running without administrator rights")
        );
        // Both given: the flag that governs elevation itself is the one named.
        assert_eq!(
            action(true, false, true, true),
            Action::Skip("#RequireAdmin: --no-elevate, running without administrator rights")
        );
    }

    #[test]
    fn an_unelevated_run_of_a_script_that_asks_does_bring_the_prompt() {
        assert_eq!(action(true, false, false, false), Action::Relaunch);
    }

    #[test]
    fn the_copy_gets_both_flags_where_the_parser_wants_them() {
        // `--attach-console` is a top-level flag, so it goes in front of the
        // subcommand; `--elevated-copy` belongs to `run`, so it goes behind it
        // and in front of the file. Both survive a later `--`.
        let own: Vec<OsString> =
            vec!["run".into(), "a script.au3".into(), "--".into(), "-x".into()];
        assert_eq!(
            elevated_args_from(own, 4242),
            vec![
                "--attach-console",
                "4242",
                "run",
                "--elevated-copy",
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

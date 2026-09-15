//! `#RequireAdmin`: running a script with full administrator rights.
//!
//! The directive cannot be honoured in place — Windows has no way to raise an
//! existing process's token, and that is the whole point of UAC. The only
//! mechanism is the one the interpreter itself uses: start a *new* process
//! through the shell's `runas` verb, let the OS show its consent prompt, and
//! run the script in that copy. The elevated copy carries the same command
//! line, so it is the same script with the same options; the caller keeps
//! `--no-elevate` to say it is the copy that has to run.
//!
//! This module is the mechanism only. Deciding *whether* the directive applies
//! — the tool's effect profile, `--no-elevate`, the "already elevated" case —
//! belongs to the caller, which also knows which command it was invoked as.
//!
//! The elevated copy is also handed a console of its own by the shell service,
//! which for a command line run means a second window; [`attach_console`] is the
//! other half of the story — the copy attaches to the caller's console, so the
//! script's output stays where the command was typed.
//!
//! Off Windows there is nothing to elevate with, and [`relaunch_elevated`]
//! answers [`Elevated::Unsupported`] rather than failing: a Windows-targeted
//! script analysed on another host should still run the way it always did.

use std::ffi::OsString;
#[cfg(not(windows))]
use std::path::Path;

/// Whether this process already holds a full administrator token.
///
/// This is the same question `IsAdmin()` answers, asked by the tool about
/// itself rather than by a script: on Windows it is real membership of the
/// Administrators group, and on every other host there are no Windows
/// administrator rights to hold, so it is `false`.
pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        crate::windows::misc::is_admin()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// What came of asking the operating system for an elevated copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevated {
    /// The elevated copy ran; this is the exit code it finished with.
    Finished(u32),
    /// The user dismissed the consent prompt, so nothing was started.
    Declined,
    /// This host has no elevation mechanism (it is not Windows).
    Unsupported,
}

/// The Windows implementation, on a host that can actually launch a process.
#[cfg(windows)]
pub use crate::windows::elevate::{attach_console, relaunch_elevated};

/// Off Windows: there is no console to reattach to (and no elevation).
#[cfg(not(windows))]
pub fn attach_console(_pid: u32) -> bool {
    false
}

/// Off Windows: there is no consent prompt to raise and no second token to get.
#[cfg(not(windows))]
pub fn relaunch_elevated(
    _exe: &Path,
    _args: &[OsString],
    _dir: Option<&Path>,
) -> Result<Elevated, String> {
    Ok(Elevated::Unsupported)
}

/// A command line for `CommandLineToArgvW`, which is how the CRT of the process
/// we start will read it back.
///
/// The rules are the documented ones: an argument without spaces, tabs or
/// quotes is passed through as it is, and otherwise it is wrapped in quotes
/// where a run of backslashes before a quote (or before the closing quote) must
/// be doubled. Always wrapping would *break* `C:\somewhere\` — the closing
/// quote would be escaped by that last separator.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn command_line(args: &[OsString]) -> String {
    args.iter()
        .map(|a| quote(&a.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote one argument for `CommandLineToArgvW` (see [`command_line`]).
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_string();
    }
    fn backslashes(out: &mut String, count: usize) {
        out.extend(std::iter::repeat_n('\\', count));
    }
    let mut out = String::from("\"");
    let mut pending = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => pending += 1,
            '"' => {
                // Backslashes before a quote are doubled, and the quote itself
                // is escaped.
                backslashes(&mut out, pending * 2 + 1);
                pending = 0;
                out.push('"');
            }
            _ => {
                backslashes(&mut out, pending);
                pending = 0;
                out.push(ch);
            }
        }
    }
    // A run of backslashes before the closing quote is doubled too, so it is
    // read back as backslashes rather than escaping the quote.
    backslashes(&mut out, pending * 2);
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_are_passed_through() {
        assert_eq!(quote("run"), "run");
        assert_eq!(quote(r"C:\tmp\script.au3"), r"C:\tmp\script.au3");
        assert_eq!(quote("--max-steps"), "--max-steps");
    }

    #[test]
    fn spaces_tabs_and_quotes_are_wrapped_and_escaped() {
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a b"), "\"a b\"");
        assert_eq!(quote("a\tb"), "\"a\tb\"");
        assert_eq!(quote("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    #[test]
    fn backslashes_before_the_closing_quote_are_doubled() {
        // The classic trap: `"C:\dir\"` would read back as `C:\dir"`.
        assert_eq!(quote(r"C:\some dir\"), r#""C:\some dir\\""#);
        // A run of backslashes that needs no quotes stays unquoted, and one
        // that does is doubled rather than escaping the closing quote.
        assert_eq!(quote(r"a\"), r"a\");
        assert_eq!(quote(r"a\ b\"), r#""a\ b\\""#);
        // Backslashes before an escaped quote are doubled *and* the quote is
        // escaped.
        assert_eq!(quote("a\\\" b"), r#""a\\\" b""#);
    }

    #[test]
    fn a_command_line_joins_the_pieces() {
        let args: Vec<OsString> =
            vec!["run".into(), "my script.au3".into(), "--max-steps".into(), "0".into()];
        assert_eq!(command_line(&args), r#"run "my script.au3" --max-steps 0"#);
    }

    #[test]
    fn other_hosts_have_nothing_to_elevate_with() {
        if !cfg!(windows) {
            assert!(!is_admin());
            assert_eq!(
                relaunch_elevated(Path::new("au3"), &[], None),
                Ok(Elevated::Unsupported)
            );
        }
    }
}

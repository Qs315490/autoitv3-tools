//! `ShellExecute*` and `RunAs*` — launching a program through the emulated
//! shell.
//!
//! These are host-process operations with Windows argument conventions, so they
//! live next to the emulation rather than in the common [`crate::common`] layer:
//! `ShellExecute`'s second argument is a separate parameter string, `RunAs`
//! carries credentials AutoIt would pass to `CreateProcessAsUser`, and both
//! report success differently from [`crate::common::proc`]'s `Run`.
//!
//! # Deliberate approximations
//!
//! * `RunAs` does not change the user token: the credentials are accepted and
//!   the program is launched under the current user. There is no way to drop
//!   privileges portably, and inventing a failed launch would be worse.
//! * `ShellExecute` hands the file to `Command`; an association-based launch
//!   (opening a `.txt` with the registered editor) is not emulated, so only
//!   executable targets succeed.

use std::io;
use std::process::{Child, Command, Stdio};

/// Spawn `program` with `params` in `workdir`.
///
/// `opt` follows AutoIt's `Run`/`RunAs` flags: bit 0 pipes stdin, bit 1 stdout
/// and bit 2 stderr; anything else is left to `null` so host console output
/// does not leak into the emulation.
pub fn spawn(program: &str, params: &str, workdir: &str, opt: i64) -> io::Result<Child> {
    let mut tokens = split_command_line(program);
    if tokens.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty program"));
    }
    let exe = tokens.remove(0);
    let mut cmd = Command::new(exe);
    cmd.args(&tokens);
    cmd.args(split_command_line(params));
    if !workdir.is_empty() {
        cmd.current_dir(workdir);
    }
    cmd.stdin(if opt & 1 != 0 {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(if opt & 2 != 0 {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stderr(if opt & 4 != 0 {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.spawn()
}

/// Split a Windows-style command line into arguments, honouring quotes.
fn split_command_line(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    out
}

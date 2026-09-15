//! End-to-end tests for `#RequireAdmin`.
//!
//! Elevation itself is Windows-only — the run either gets a second, elevated
//! process or it does not, and a test cannot answer a UAC prompt — so what is
//! checked here is the decision and the reporting: the directive is found, the
//! two ways of switching it off are honoured, and a host with no elevation
//! mechanism says so and runs the script anyway.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Write `body` to a scratch script and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-requireadmin-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join("script.au3");
    std::fs::write(&path, body).expect("write script");
    path
}

/// Run `au3 <args>` and return `(succeeded, stdout and stderr folded together)`.
fn au3(args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_au3"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run au3");
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[test]
fn no_elevate_reports_the_directive_and_runs_the_script() {
    let path = script("no-elevate", "#RequireAdmin\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--no-elevate"]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("note: #RequireAdmin: --no-elevate"), "got:\n{out}");
    assert!(out.contains("ran"), "the script still ran:\n{out}");
}

#[test]
fn denying_spawn_switches_the_directive_off() {
    let path = script("deny-spawn", "#RequireAdmin\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--deny", "spawn"]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("note: #RequireAdmin: --deny spawn"), "got:\n{out}");
    assert!(out.contains("ran"), "got:\n{out}");
}

#[test]
fn a_script_without_the_directive_says_nothing() {
    let path = script("plain", "#NoTrayIcon\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(!out.contains("#RequireAdmin"), "got:\n{out}");
}

#[test]
fn a_directive_inside_a_function_is_not_the_script_s() {
    // AutoIt's preprocessor only reads the top level, so this is a statement
    // the interpreter steps over: no elevation, and no note about one.
    let path = script(
        "in-function",
        "Func F()\n    #RequireAdmin\n    Return 1\nEndFunc\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "F", "--no-elevate"]);
    assert!(ok, "got:\n{out}");
    assert!(!out.contains("#RequireAdmin"), "got:\n{out}");
}

/// Off Windows there is no consent prompt to raise, so a plain run reports
/// that and keeps going; on Windows the same run would be the elevated copy
/// and cannot be tested without answering UAC.
#[cfg(not(windows))]
#[test]
fn a_host_without_elevation_says_so_and_runs_the_script() {
    let path = script("no-mechanism", "#RequireAdmin\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: elevation is a Windows mechanism"),
        "got:\n{out}"
    );
    assert!(out.contains("ran"), "got:\n{out}");
}

/// The debugger cannot elevate — an elevated copy would be a second process
/// without this shell's stdin — so it says what it did not do instead.
#[test]
fn the_debugger_reports_the_directive_without_elevating() {
    let path = script("debug", "#RequireAdmin\nGlobal $g = 1\n");
    let (ok, out) = au3(&["debug", &path.to_string_lossy(), "-c", "quit"]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: this script wants administrator rights"),
        "got:\n{out}"
    );
}

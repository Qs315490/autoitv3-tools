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
    au3_env(args, &[])
}

/// [`au3`] with extra environment variables — `AU3_WIN_ADMIN=0` makes the
/// emulation answer `IsAdmin()` for a standard user, which is how the
/// simulated elevation can be told apart from the real answer off Windows.
fn au3_env(args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_au3"));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("run au3");
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

/// The elevated copy is told it is the copy: the elevation happened in the
/// process that started it, so it neither elevates again nor reports the
/// directive as skipped.
#[test]
fn the_elevated_copy_does_not_elevate_or_report() {
    let path = script("elevated-copy", "#RequireAdmin\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--elevated-copy"]);
    assert!(ok, "got:\n{out}");
    assert!(!out.contains("#RequireAdmin"), "got:\n{out}");
    assert!(out.contains("ran"), "got:\n{out}");
}

/// The elevated copy is told which console to print into, so its output does
/// not end up in a second window. Off Windows there is no Windows console to
/// attach to, and the copy says so instead of falling over; on Windows this
/// would be the elevation path itself, which a test cannot answer.
#[cfg(not(windows))]
#[test]
fn the_console_hand_off_is_reported_when_there_is_none() {
    let path = script("attach-console", "ConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["--attach-console", "1234", "run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: elevated, but process 1234"),
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

// ---------------------------------------------------------------------------
// `au3 debug`
// ---------------------------------------------------------------------------
//
// A debug session can be handed over too (`--attach-console` keeps the prompt
// in this window), but a test cannot answer a UAC prompt: what is checked here
// is where the session stays and what it says about it.

/// The test harness runs the debugger with `stdin` at `/dev/null`, so there is
/// no console to hand a session over to: it stays in this process, says so, and
/// still debugs the script.
#[test]
fn debug_stays_unelevated_without_a_console_to_hand_over() {
    let path = script("debug-pipe", "#RequireAdmin\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&[
        "debug",
        &path.to_string_lossy(),
        "-c",
        "run",
        "-c",
        "quit",
    ]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: this script wants administrator rights, but this session stays in this process"),
        "got:\n{out}"
    );
    assert!(out.contains("ran"), "the session still ran the script:\n{out}");
}

#[test]
fn debug_no_elevate_reports_the_skip_reason() {
    let path = script("debug-no-elevate", "#RequireAdmin\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&[
        "debug",
        &path.to_string_lossy(),
        "--no-elevate",
        "-c",
        "run",
        "-c",
        "quit",
    ]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: --no-elevate"),
        "got:\n{out}"
    );
    assert!(out.contains("ran"), "got:\n{out}");
}

#[test]
fn debug_without_the_directive_says_nothing() {
    let path = script("debug-plain", "#NoTrayIcon\nConsoleWrite(\"ran\")\n");
    let (ok, out) = au3(&["debug", &path.to_string_lossy(), "-c", "run", "-c", "quit"]);
    assert!(ok, "got:\n{out}");
    assert!(!out.contains("#RequireAdmin"), "got:\n{out}");
}

// ---------------------------------------------------------------------------
// The deterministic profile simulates the elevation
// ---------------------------------------------------------------------------

/// A standard-user emulation (`AU3_WIN_ADMIN=0`) is what makes the difference
/// visible: the simulation answers `IsAdmin()` = 1 without asking the OS for
/// anything, and a faithful run leaves the answer alone.
const IS_ADMIN: &str = "#RequireAdmin\nConsoleWrite(\"IsAdmin=\" & IsAdmin() & @CRLF)\n";

#[test]
fn a_deterministic_run_simulates_the_elevation() {
    let path = script("simulate", IS_ADMIN);
    let (ok, out) = au3_env(
        &["run", &path.to_string_lossy(), "--deterministic"],
        &[("AU3_WIN_ADMIN", "0")],
    );
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: the deterministic profile simulates the elevation"),
        "got:\n{out}"
    );
    assert!(out.contains("IsAdmin=1"), "the script sees an administrator:\n{out}");
    assert!(
        !out.contains("elevation is a Windows mechanism"),
        "nothing was asked of the OS:\n{out}"
    );
}

#[test]
fn a_deterministic_debug_session_simulates_the_elevation_too() {
    let path = script("simulate-debug", IS_ADMIN);
    let (ok, out) = au3_env(
        &[
            "debug",
            &path.to_string_lossy(),
            "--deterministic",
            "-c",
            "run",
            "-c",
            "quit",
        ],
        &[("AU3_WIN_ADMIN", "0")],
    );
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: the deterministic profile simulates the elevation"),
        "got:\n{out}"
    );
    assert!(out.contains("IsAdmin=1"), "got:\n{out}");
}

#[test]
fn no_elevate_still_declines_the_simulation() {
    let path = script("simulate-no-elevate", IS_ADMIN);
    let (ok, out) = au3_env(
        &[
            "run",
            &path.to_string_lossy(),
            "--deterministic",
            "--no-elevate",
        ],
        &[("AU3_WIN_ADMIN", "0")],
    );
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: #RequireAdmin: --no-elevate"),
        "got:\n{out}"
    );
    assert!(
        !out.contains("simulates the elevation"),
        "nothing is simulated when the directive is refused:\n{out}"
    );
    assert!(out.contains("IsAdmin=0"), "the real answer stands:\n{out}");
}

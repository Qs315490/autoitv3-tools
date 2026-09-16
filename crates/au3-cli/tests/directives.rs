//! The script directives that are not `#include` / `#RequireAdmin`.
//!
//! `#OnAutoItStartRegister` does something: AutoIt's compiler arranges for the
//! named function to run once, before the body's first statement. The other two
//! are accepted and have nothing to do **here** — we never draw a tray icon, and
//! we are not `AutoIt3.exe` — so these tests pin the silence down as deliberate
//! rather than an oversight.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Write `body` to a scratch script and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-directive-{tag}-{}", std::process::id()));
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
fn on_autoit_start_register_runs_before_the_body() {
    let path = script(
        "start",
        "#OnAutoItStartRegister \"Boot\"\n\
         Func Boot()\n\
             ConsoleWrite(\"boot\" & @CRLF)\n\
             Global $Seen = 1\n\
         EndFunc\n\
         ConsoleWrite(\"body seen=\" & $Seen & @CRLF)\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    let boot = out.find("boot").expect("the hook ran");
    let body = out.find("body seen=1").expect("the body ran");
    assert!(boot < body, "the hook runs first:\n{out}");
}

#[test]
fn a_missing_start_register_function_is_reported() {
    let path = script(
        "missing",
        "#OnAutoItStartRegister \"Nope\"\nConsoleWrite(\"body-ran\" & @CRLF)\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(!ok, "the run fails:\n{out}");
    assert!(
        out.contains("undefined function: Nope"),
        "got:\n{out}"
    );
    assert!(!out.contains("body-ran"), "the body never ran:\n{out}");
}

#[test]
fn no_tray_icon_is_accepted_and_changes_nothing() {
    // Nothing draws a tray icon, so the directive has nothing to suppress; the
    // `Tray*` functions answer exactly as they do without it.
    let plain = script(
        "tray-plain",
        "ConsoleWrite(TraySetState() & \" \" & TraySetIcon() & @CRLF)\n",
    );
    let (ok, without) = au3(&["run", &plain.to_string_lossy()]);
    assert!(ok, "got:\n{without}");

    let path = script(
        "tray-off",
        "#NoTrayIcon\nConsoleWrite(TraySetState() & \" \" & TraySetIcon() & @CRLF)\n",
    );
    let (ok, with) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{with}");
    assert!(
        with.contains("1 1"),
        "Tray* answers as usual:\n{with}"
    );
}

#[test]
fn no_autoit3_execute_is_accepted_because_we_are_not_autoit3() {
    // The directive tells `AutoIt3.exe` to refuse `/AutoIt3ExecuteScript` and
    // `/AutoIt3ExecuteLine`; this tool never starts a script that way, so a
    // plain run is unaffected by it.
    let path = script(
        "no-execute",
        "#NoAutoIt3Execute\nConsoleWrite(\"ran\" & @CRLF)\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("ran"), "got:\n{out}");
    assert!(
        !out.contains("autoit3execute"),
        "the directive is not reported as an error:\n{out}"
    );
}

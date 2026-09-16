//! Which execution profile each command defaults to, and what a refusal says.
//!
//! `run`/`debug` run a script the way AutoIt would — real side effects;
//! `evaluate`/`deobfuscate --evaluate` analyse it under the deterministic
//! profile, which refuses them. A refusal looks exactly like the machine
//! refusing it (the call returns its failure value with `@error = 1`), so it is
//! named on stderr once per kind.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Write `body` to a scratch script and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-profile-{tag}-{}", std::process::id()));
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

/// A script that creates a directory and reports what happened, then cleans up.
const CREATE: &str = "Local $dir = @TempDir & \"\\au3-profile-dir\"\n\
                      Local $r = DirCreate($dir)\n\
                      ConsoleWrite(\"created=\" & $r & \" err=\" & @error & @CRLF)\n\
                      DirRemove($dir)\n";

#[test]
fn run_does_the_side_effect_like_autoit_would() {
    let path = script("run-default", CREATE);
    let (ok, out) = au3(&["run", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("created=1 err=0"), "the write happened:\n{out}");
    assert!(!out.contains("was refused"), "nothing was refused:\n{out}");
}

#[test]
fn debug_does_the_side_effect_like_autoit_would() {
    let path = script("debug-default", CREATE);
    let (ok, out) = au3(&[
        "debug",
        &path.to_string_lossy(),
        "-c",
        "run",
        "-c",
        "quit",
    ]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("created=1 err=0"), "the write happened:\n{out}");
    assert!(!out.contains("was refused"), "nothing was refused:\n{out}");
}

#[test]
fn the_deterministic_profile_refuses_and_says_which_kind() {
    let path = script("run-deterministic", CREATE);
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--deterministic"]);
    assert!(ok, "got:\n{out}");
    assert!(out.contains("created=0 err=1"), "the write was refused:\n{out}");
    assert!(
        out.contains("note: a file side effect was refused by the execution profile"),
        "got:\n{out}"
    );
    assert!(
        out.contains("--allow file"),
        "the note says how to allow it:\n{out}"
    );
}

/// Under the deterministic profile `--gui auto` has to mean headless: the
/// platform's own backend is the real Win32 one on Windows, and a `MsgBox`
/// there waits for somebody to click it. The timeout keeps a regression from
/// hanging the suite — it would answer `-1` after a second instead.
#[test]
fn the_deterministic_profile_makes_auto_headless() {
    let path = script(
        "auto-headless",
        "Local $a = MsgBox(0, \"t\", \"b\", 1)\nConsoleWrite(\"answer=\" & $a & @CRLF)\n",
    );
    let (ok, out) = au3(&[
        "run",
        &path.to_string_lossy(),
        "--deterministic",
        "--gui",
        "auto",
    ]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("[winemu] MsgBox(0, \"t\", \"b\") -> 1"),
        "the dialog was answered, not shown:\n{out}"
    );
    assert!(out.contains("answer=1"), "got:\n{out}");
}

#[test]
fn a_refused_kind_is_reported_once() {
    // Two writes, one note: the second attempt is the same kind.
    let path = script(
        "once",
        "DirCreate(@TempDir & \"\\au3-profile-a\")\nDirCreate(@TempDir & \"\\au3-profile-b\")\n",
    );
    let (ok, out) = au3(&["run", &path.to_string_lossy(), "--deterministic"]);
    assert!(ok, "got:\n{out}");
    assert_eq!(
        out.matches("note: a file side effect").count(),
        1,
        "reported once per kind:\n{out}"
    );
}

#[test]
fn allowing_one_kind_silences_only_that_kind() {
    let path = script(
        "allow-file",
        "DirCreate(@TempDir & \"\\au3-profile-allowed\")\n\
         EnvSet(\"AU3_PROFILE_TEST\", \"1\")\n",
    );
    let (ok, out) = au3(&[
        "run",
        &path.to_string_lossy(),
        "--deterministic",
        "--allow",
        "file",
    ]);
    assert!(ok, "got:\n{out}");
    assert!(
        !out.contains("a file side effect"),
        "file writes are allowed:\n{out}"
    );
    assert!(
        out.contains("note: a env side effect was refused"),
        "the environment write is not:\n{out}"
    );
}

#[test]
fn analysing_a_script_keeps_the_deterministic_default() {
    let path = script("evaluate-default", CREATE);
    let (ok, out) = au3(&["evaluate", &path.to_string_lossy()]);
    assert!(ok, "got:\n{out}");
    assert!(
        out.contains("note: a file side effect was refused"),
        "evaluate analyses without side effects:\n{out}"
    );
}

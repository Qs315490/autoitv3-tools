//! End-to-end tests for `au3 run`'s argument order.
//!
//! `FILE` comes first and `FUNC` is optional: without it the whole script body
//! runs, with it one function is called.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A script that sets a global and defines a function to call.
const SCRIPT: &str = "\
Global $started = 0

Func Add($a, $b)
    Return $a + $b
EndFunc

$started = 1
";

/// Write `body` to a scratch file and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-run-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join("script.au3");
    std::fs::write(&path, body).expect("write script");
    path
}

/// Run `au3 <args>`, returning stdout and stderr folded together.
fn au3(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_au3"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run au3");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn no_function_runs_the_whole_script_body() {
    let path = script("body", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap()]);
    assert!(out.contains("script body ran to completion"), "got:\n{out}");
}

#[test]
fn a_trailing_function_is_called_with_its_args() {
    let path = script("call", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "Add", "--arg", "2", "--arg", "3"]);
    assert!(out.contains("Add() = 5"), "got:\n{out}");
}

#[test]
fn an_arg_without_a_function_becomes_the_script_cmdline() {
    let path = script(
        "cmdline",
        r#"
ConsoleWrite($CmdLine[0] & "|" & $CmdLine[1] & "|" & $CmdLine[2] & "|" & $CmdLineRaw & @CRLF)
"#,
    );
    let out = au3(&[
        "run",
        path.to_str().unwrap(),
        "--arg",
        "alpha",
        "--arg",
        "beta gamma",
    ]);
    assert!(
        out.contains("2|alpha|beta gamma|alpha \"beta gamma\""),
        "got:\n{out}"
    );
}

#[test]
fn a_function_call_leaves_the_script_cmdline_empty() {
    let path = script(
        "cmdline-func",
        r#"
Func Main()
    Return $CmdLine[0]
EndFunc
"#,
    );
    let out = au3(&["run", path.to_str().unwrap(), "Main"]);
    assert!(out.contains("Main() = 0"), "got:\n{out}");
}

#[test]
fn a_function_call_can_still_give_the_script_a_cmdline() {
    let path = script(
        "both",
        r#"
Func Main($x)
    Return $CmdLine[0] & "|" & $CmdLine[1] & "|" & $x
EndFunc
"#,
    );
    let out = au3(&[
        "run",
        path.to_str().unwrap(),
        "Main",
        "--cmdline",
        "s1",
        "--arg",
        "f1",
    ]);
    assert!(out.contains(r#"Main() = "1|s1|f1""#), "got:\n{out}");
}

#[test]
fn cmdline_and_arg_both_feed_the_script_when_no_function_is_named() {
    let path = script(
        "both-body",
        r#"
ConsoleWrite($CmdLine[0] & "|" & $CmdLine[1] & "|" & $CmdLine[2] & @CRLF)
"#,
    );
    let out = au3(&["run", path.to_str().unwrap(), "--cmdline", "a", "--arg", "b"]);
    assert!(out.contains("2|a|b"), "got:\n{out}");
}

#[test]
fn the_input_decides_compiled_and_the_flag_overrides_it() {
    let path = script("compiled", r#"
ConsoleWrite("[" & @Compiled & "]" & @CRLF)
"#);
    let source = au3(&["run", path.to_str().unwrap()]);
    assert!(source.contains("[0]"), "a .au3 answers 0: {source}");
    let forced = au3(&["run", path.to_str().unwrap(), "--compiled"]);
    assert!(forced.contains("[1]"), "--compiled was ignored: {forced}");
}

// ---------------------------------------------------------------------------
// GUI backend selection
// ---------------------------------------------------------------------------

#[test]
fn gui_calls_are_answered_with_and_without_a_backend() {
    // Without `--gui` the platform picks: nothing is drawn on this host, and a
    // Windows one opens a real window for the moment the process lives. Either
    // way the 165 GUI functions answer, which is what this pins — with
    // `--gui headless` nothing is drawn anywhere.
    let body = "Func F()\n    Return GUICreate(\"T\", 100, 50) > 0\nEndFunc\n";
    let path = script("gui-headless", body);
    let out = au3(&["run", path.to_str().unwrap(), "F"]);
    assert!(out.contains("F() = true"), "got:\n{out}");

    let out = au3(&["run", path.to_str().unwrap(), "--gui", "headless", "F"]);
    assert!(out.contains("F() = true"), "got:\n{out}");

    let out = au3(&["run", path.to_str().unwrap(), "--gui", "auto", "F"]);
    assert!(out.contains("F() = true"), "got:\n{out}");
}

#[test]
fn gui_rejects_an_unknown_mode() {
    let path = script("gui-bad", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "--gui", "bogus"]);
    assert!(
        out.contains("invalid value") && out.contains("bogus"),
        "got:\n{out}"
    );
}

/// `--gui window` needs the eframe-backed build; without the feature the CLI
/// has to say how to get one rather than failing obscurely.
#[cfg(not(feature = "gui-window"))]
#[test]
fn gui_window_without_the_feature_says_how_to_build_it() {
    let path = script("gui-window-off", SCRIPT);
    let out = au3(&["run", path.to_str().unwrap(), "--gui", "window"]);
    assert!(out.contains("gui-window"), "got:\n{out}");
    assert!(out.contains("--features gui-window"), "got:\n{out}");
}

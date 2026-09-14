//! End-to-end tests for the shared `--max-steps` budget.
//!
//! The step limit is a runtime knob surfaced on every command that runs the
//! interpreter, so these drive the real binary and check it reaches both the
//! `run` path (a bare `Runtime`) and the `evaluate` path
//! (`evaluate_with_options`), which used to carry their own hard-coded values.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A function that never returns, so only the step budget stops it.
const SPIN: &str = "\
Func Spin()
    Local $i = 0
    While 1
        $i += 1
    WEnd
EndFunc
";

/// Write `body` to a scratch file and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-steps-{tag}-{}", std::process::id()));
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
fn run_honours_max_steps() {
    let path = script("run", SPIN);
    let out = au3(&["run", "Spin", path.to_str().unwrap(), "--max-steps", "1000"]);
    assert!(out.contains("step limit exceeded (1000)"), "got:\n{out}");
}

#[test]
fn evaluate_honours_max_steps() {
    let path = script("eval", &format!("{SPIN}\nSpin()\n"));
    let out = au3(&["evaluate", path.to_str().unwrap(), "--max-steps", "1000"]);
    assert!(out.contains("step limit exceeded (1000)"), "got:\n{out}");
}

#[test]
fn evaluate_accepts_no_progress() {
    // The run is far shorter than the heartbeat interval, so the point here is
    // only that the flag parses and the quiet path runs to the same report;
    // `progress::reporter` is unit-tested for the actual suppression.
    let path = script("quiet", &format!("{SPIN}\nSpin()\n"));
    let out = au3(&[
        "evaluate",
        path.to_str().unwrap(),
        "--max-steps",
        "1000",
        "--no-progress",
    ]);
    assert!(out.contains("evaluated:"), "got:\n{out}");
    assert!(!out.contains("evaluating:"), "got:\n{out}");
}

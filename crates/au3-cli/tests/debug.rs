//! End-to-end tests for `au3 debug`.
//!
//! They drive the real binary, because the thing under test is the *shell*:
//! the command grammar, the prompt's control flow, and the hand-off between the
//! driver loop and the interpreter when a stop happens.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A script with a loop and a function, so stepping has somewhere to go.
const SCRIPT: &str = "\
Global $counter = 0
Global $total = 0

Func Add($a, $b)
    Local $sum = $a + $b
    Return $sum
EndFunc

Func Main()
    For $i = 1 To 3
        $counter += 1
        $total += Add($i, 10)
    Next
    Return $total
EndFunc

Main()
";

/// Write `body` to a scratch file and return its path.
fn script(tag: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("au3-debug-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join("script.au3");
    std::fs::write(&path, body).expect("write script");
    path
}

/// Whether some line of `out` is exactly `want`.
///
/// Whole-line matching keeps the assertions honest: `1` is not found inside
/// `11`, and no assertion depends on how many newlines happen to surround it.
fn has_line(out: &str, want: &str) -> bool {
    out.lines().any(|l| l.trim_end() == want)
}

/// Run `au3 debug <file>` with `-c` commands, returning stdout and stderr.
fn shell(path: &Path, commands: &[&str]) -> String {
    run_full(path, &[], commands, "")
}

/// As [`shell`], with flags of the `debug` subcommand (`--stop-at-start`, ...).
fn shell_with(path: &Path, flags: &[&str], commands: &[&str]) -> String {
    run_full(path, flags, commands, "")
}

/// As [`shell`], but feeding `input` on stdin after the `-c` commands.
fn run_with_stdin(path: &Path, commands: &[&str], input: &str) -> String {
    run_full(path, &[], commands, input)
}

fn run_full(path: &Path, flags: &[&str], commands: &[&str], input: &str) -> String {
    use std::io::Write;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_au3"));
    cmd.arg("debug").arg(path);
    for f in flags {
        cmd.arg(f);
    }
    for c in commands {
        cmd.arg("-c").arg(c);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn au3 debug");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait for au3 debug");
    // stderr is folded in so a panic or a warning shows up in the assertion
    // message rather than vanishing.
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_breakpoint_stops_and_the_prompt_sees_the_frame() {
    let path = script("break", SCRIPT);
    let out = shell(&path, &["break 11", "run", "print $i", "info locals", "quit"]);
    assert!(out.contains("Breakpoint 1 at line 11"), "got:\n{out}");
    assert!(out.contains("Breakpoint 1, line 11"), "got:\n{out}");
    // The source line is shown, then `$i` evaluates in the loop's frame.
    assert!(out.contains("$counter += 1"), "got:\n{out}");
    assert!(has_line(&out, "1"), "print $i should be 1:\n{out}");
    assert!(out.contains("i = 1"), "got:\n{out}");
}

#[test]
fn next_steps_over_a_call_and_step_enters_it() {
    let path = script("step", SCRIPT);
    let out = shell(
        &path,
        &[
            "break 11",
            "run",
            "next",
            "step",
            "print $a",
            "backtrace",
            "quit",
        ],
    );
    assert!(out.contains("Stopped at line 12"), "got:\n{out}");
    // `step` from the call site lands inside `Add`.
    assert!(out.contains("Stopped at line 5"), "got:\n{out}");
    assert!(out.contains("Local $sum = $a + $b"), "got:\n{out}");
    assert!(has_line(&out, "1"), "print $a should be 1:\n{out}");
    // gdb numbering: #0 is the innermost frame.
    assert!(out.contains("#0  Add at 5:5"), "got:\n{out}");
    assert!(out.contains("#1  Main at 12:9"), "got:\n{out}");
}

#[test]
fn a_conditional_breakpoint_fires_only_when_it_holds() {
    let path = script("cond", SCRIPT);
    let out = shell(
        &path,
        &[
            "break 11 if $i = 3",
            "run",
            "print $i",
            "info breakpoints",
            "quit",
        ],
    );
    // `=` is AutoIt's equality in expression position; the condition must not
    // assign, and must not fire on the first two iterations.
    assert!(
        out.contains("Breakpoint 1 at line 11 if $i = 3"),
        "got:\n{out}"
    );
    assert!(has_line(&out, "3"), "stopped on the wrong iteration:\n{out}");
    assert!(out.contains("hits=1"), "got:\n{out}");
}

#[test]
fn set_changes_what_the_program_computes() {
    let path = script("set", SCRIPT);
    let out = shell(
        &path,
        &["break 11", "run", "set $i = 3", "next", "step", "print $a", "quit"],
    );
    // The call on the next line reads `$i`, so `$a` proves the write landed in
    // the program rather than in a copy of it.
    assert!(has_line(&out, "3"), "the write did not reach the call:\n{out}");
}

#[test]
fn a_guarded_print_does_not_assign() {
    let path = script("noassign", SCRIPT);
    // `print $i = 3` is a comparison. Had it assigned, `$i` would now be 3 and
    // the next print would say so.
    let out = shell(
        &path,
        &["break 11", "run", "print $i = 3", "next", "print $i", "quit"],
    );
    assert!(has_line(&out, "false"), "`$i = 3` should compare at $i = 1:\n{out}");
    assert!(has_line(&out, "1"), "the loop variable was overwritten:\n{out}");
}

#[test]
fn a_session_can_be_driven_from_stdin() {
    let path = script("stdin", SCRIPT);
    let out = run_with_stdin(&path, &[], "break 11\nrun\nprint $i\nquit\n");
    assert!(out.contains("Breakpoint 1, line 11"), "got:\n{out}");
    assert!(has_line(&out, "1"), "got:\n{out}");
    // Nothing was prompted for, because stdin is a pipe rather than a terminal.
    assert!(!out.contains("(au3)"), "a pipe should not draw a prompt:\n{out}");
}

#[test]
fn list_shows_the_source_around_the_stop() {
    let path = script("list", SCRIPT);
    let out = shell(&path, &["break 11", "run", "list", "quit"]);
    assert!(out.contains("=> "), "no current-line marker:\n{out}");
    assert!(out.contains("Func Main()"), "got:\n{out}");
    assert!(out.contains("For $i = 1 To 3"), "got:\n{out}");
}

#[test]
fn an_idle_session_evaluates_without_running() {
    let path = script("idle", SCRIPT);
    let out = shell(&path, &["print 2 + 3", "info functions", "backtrace", "quit"]);
    assert!(has_line(&out, "5"), "got:\n{out}");
    assert!(out.contains("Add"), "got:\n{out}");
    assert!(out.contains("Main"), "got:\n{out}");
    // Nothing was run, so there is nothing to step and no frames to show.
    assert!(out.contains("the script is not stopped"), "got:\n{out}");
}

#[test]
fn stop_at_start_halts_on_the_first_statement() {
    let path = script("start", SCRIPT);
    let out = shell_with(
        &path,
        &["--stop-at-start"],
        &["run", "backtrace", "quit"],
    );
    assert!(out.contains("Stopped at line 1"), "got:\n{out}");
    assert!(out.contains("Global $counter = 0"), "got:\n{out}");
    assert!(out.contains("#0  <script>"), "got:\n{out}");
}

#[test]
fn run_at_a_stop_starts_over_and_keeps_the_breakpoints() {
    let path = script("restart", SCRIPT);
    let out = shell(
        &path,
        &["break 11", "run", "continue", "print $total", "run", "print $total", "quit"],
    );
    // One iteration of the first pass has added 11; a fresh pass starts at 0.
    assert!(has_line(&out, "11"), "first pass:\n{out}");
    assert!(has_line(&out, "0"), "the second run did not start over:\n{out}");
    // The breakpoint survived the restart — it stopped three times in all.
    assert_eq!(out.matches("Breakpoint 1, line 11").count(), 3, "got:\n{out}");
}

#[test]
fn help_lists_the_commands() {
    let path = script("help", SCRIPT);
    let out = shell(&path, &["help", "quit"]);
    assert!(out.contains("break <line>"), "got:\n{out}");
    assert!(out.contains("backtrace"), "got:\n{out}");
}

#[test]
fn a_bad_breakpoint_line_is_reported_not_fatal() {
    let path = script("badline", SCRIPT);
    let out = shell(&path, &["break 0", "break 9999", "print 1", "quit"]);
    assert!(out.contains("line 0 is outside"), "got:\n{out}");
    assert!(out.contains("line 9999 is outside"), "got:\n{out}");
    assert!(has_line(&out, "1"), "the session kept going:\n{out}");
}

#[test]
fn an_unknown_command_is_reported_not_fatal() {
    let path = script("unknown", SCRIPT);
    let out = shell(&path, &["frobnicate", "print 1", "quit"]);
    assert!(out.contains("unknown command"), "got:\n{out}");
    assert!(out.contains("\n1\n"), "got:\n{out}");
}

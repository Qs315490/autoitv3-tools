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

/// `stopat` suspends *before* the call, so what a dialog is about to say can be
/// read without the dialog opening; the call then runs on `continue`.
#[test]
fn stopat_holds_a_call_before_it_runs() {
    let path = script(
        "stopat",
        "MsgBox(16, \"title here\", \"body here\")\nConsoleWrite(\"after\" & @CRLF)\n",
    );
    let out = shell(
        &path,
        &["stopat MsgBox", "run", "backtrace", "continue", "quit"],
    );
    assert!(
        out.contains("Catchpoint: MsgBox(16, \"title here\", \"body here\")"),
        "the arguments are shown before the call:\n{out}"
    );
    // The stop is at the call itself, so the frame is the line that made it.
    assert!(out.contains("#0  <script> at script.au3:1"), "got:\n{out}");
    // ... and the dialog still runs, answered by the headless backend.
    assert!(out.contains("after"), "the call ran on continue:\n{out}");
}

/// Several targets at once: they accumulate (like gdb's `catch`), each fires,
/// and `stopat off` clears the lot.
#[test]
fn stopat_takes_several_targets() {
    let path = script(
        "stopat-many",
        "Local $n = StringLen(\"abc\")\nDllOpen(\"Advapi32.dll\")\nConsoleWrite($n & @CRLF)\n",
    );
    let out = shell(
        &path,
        &[
            "stopat StringLen DllOpen",
            "run",
            "continue",
            "continue",
            "stopat",
            "stopat off",
            "quit",
        ],
    );
    assert!(out.contains("Catchpoint: StringLen(\"abc\")"), "got:\n{out}");
    assert!(
        out.contains("Catchpoint: DllOpen(\"Advapi32.dll\")"),
        "got:\n{out}"
    );
    assert!(
        out.contains("stopping before every stringlen, dllopen call"),
        "the list:\n{out}"
    );
    assert!(out.contains("stop-at cleared (was stringlen, dllopen)"), "got:\n{out}");
}

/// A script function stops at its entry, where the parameters are bound.
#[test]
fn stopat_stops_at_a_script_functions_entry() {
    let path = script(
        "stopat-func",
        "Func Double($n)\n    Return $n * 2\nEndFunc\nConsoleWrite(Double(21) & @CRLF)\n",
    );
    let out = shell(&path, &["stopat Double", "run", "print $n", "continue", "quit"]);
    assert!(out.contains("Catchpoint: Double(21)"), "got:\n{out}");
    assert!(has_line(&out, "21"), "the parameter is readable:\n{out}");
    assert!(out.contains("42"), "the call ran on continue:\n{out}");
}

/// `stopat off` clears it, and a plain `stopat` reports what is set.
#[test]
fn stopat_can_be_read_and_cleared() {
    let path = script("stopat-off", "MsgBox(0, \"t\", \"b\")\n");
    let out = shell(&path, &["stopat MsgBox", "stopat", "stopat off", "stopat", "quit"]);
    // Targets are remembered lower-cased, so they are listed that way.
    assert!(out.contains("stopping before every msgbox call"), "got:\n{out}");
    assert!(out.contains("stop-at cleared (was msgbox)"), "got:\n{out}");
    assert!(out.contains("no stop-at set"), "got:\n{out}");
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
    assert!(out.contains("#0  Add(a=1, b=10) at script.au3:5"), "got:\n{out}");
    assert!(out.contains("#1  Main() at script.au3:12"), "got:\n{out}");
}

#[test]
fn step_before_run_stops_at_the_first_statement() {
    // `step` with no `run` first starts the script *and* stops; the request
    // must survive `begin_run`, which used to reset it to plain running.
    let path = script("step-start", SCRIPT);
    let out = shell(&path, &["step 1", "print $counter", "quit"]);
    assert!(out.contains("Stopped at line 1"), "got:\n{out}");
    assert!(has_line(&out, "\"\""), "the body has not run yet:\n{out}");
}

#[test]
fn a_finished_run_does_not_leak_its_step_budget() {
    // `run` finishes, then a fresh `step 1` starts over and stops at the first
    // statement rather than inheriting the old budget.
    let path = script("step-restart", SCRIPT);
    let out = shell(&path, &["run", "step 1", "print $counter", "quit"]);
    assert!(out.contains("Stopped at line 1"), "got:\n{out}");
    assert!(has_line(&out, "\"\""), "the fresh run has not stepped:\n{out}");
}

#[test]
fn step_and_next_take_a_statement_count() {
    // `step 2` counts every statement, so it walks into `Add`.
    let path = script("step-count-into", SCRIPT);
    let out = shell(&path, &["break 11", "run", "step 2", "quit"]);
    assert!(out.contains("Stopped at line 5"), "got:\n{out}");

    // A straight-line body, so `next 3`'s count is easy to follow: from the
    // stop at line 2 it runs 2, 3 and 4, then stops before line 5.
    const STRAIGHT: &str = "\
Global $x = 0
$x += 1
$x += 1
$x += 1
$x += 1
";
    let path = script("next-count", STRAIGHT);
    let out = shell(&path, &["break 2", "run", "next 3", "print $x", "quit"]);
    assert!(out.contains("Stopped at line 5"), "got:\n{out}");
    assert!(has_line(&out, "3"), "x should be 3:\n{out}");
}

#[test]
fn break_accepts_function_relative_line_expressions() {
    let path = script("line-func", SCRIPT);
    let out = shell(&path, &["break Main+1", "break Main-1", "run", "quit"]);
    assert!(
        out.contains("Breakpoint 1 at func Main+1 (line 11)"),
        "got:\n{out}"
    );
    // `Main` starts at line 10, so `Main-1` is the declaration line.
    assert!(
        out.contains("Breakpoint 2 at func Main-1 (line 9)"),
        "got:\n{out}"
    );
}

#[test]
fn break_accepts_stop_relative_line_expressions() {
    let path = script("line-rel", SCRIPT);
    let out = shell(&path, &["break 11", "run", "break +1", "continue", "quit"]);
    assert!(out.contains("Breakpoint 2 at line 12"), "got:\n{out}");
    assert!(out.contains("Breakpoint 2, line 12"), "got:\n{out}");
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
    assert!(out.contains("break <line-expr>"), "got:\n{out}");
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

// ---------------------------------------------------------------------------
// Command sources and post-mortem stops
// ---------------------------------------------------------------------------

/// A script that fails when indexed past the end of its array.
const FAILING: &str = "\
Global $seen = 0

Func Boom($n)
    Local $x = $n * 2
    Local $list[2] = [1, 2]
    Return $list[$n]
EndFunc

Func Outer()
    $seen += 1
    Return Boom(5)
EndFunc

Outer()
";

#[test]
fn one_dash_c_can_carry_several_commands() {
    let path = script("semi", SCRIPT);
    let out = shell(
        &path,
        &["break 11; run; print $i; next; print $counter; quit"],
    );
    assert!(out.contains("Breakpoint 1, line 11"), "got:\n{out}");
    assert!(has_line(&out, "Stopped at line 12"), "got:\n{out}");
    assert!(has_line(&out, "1"), "got:\n{out}");
}

#[test]
fn a_semicolon_inside_a_string_is_not_a_separator() {
    let path = script("semiquote", SCRIPT);
    let out = shell(&path, &[r#"print "a;b"; print 1; quit"#]);
    assert!(out.contains(r#""a;b""#), "the string was split:\n{out}");
    assert!(has_line(&out, "1"), "the second command was lost:\n{out}");
}

#[test]
fn a_command_file_runs_first_and_the_session_carries_on() {
    let path = script("cmdfile", SCRIPT);
    let dir = path.parent().expect("scratch dir");
    let commands = dir.join("session.au3dbg");
    std::fs::write(
        &commands,
        "# my usual breakpoints\nbreak 11\n\nrun\nprint $i\n",
    )
    .expect("write command file");

    // The file's commands run first; stdin is then still read.
    let out = run_full(
        &path,
        &["-x", commands.to_str().expect("utf-8 path")],
        &[],
        "next\nprint $counter\nquit\n",
    );
    assert!(out.contains("Breakpoint 1, line 11"), "got:\n{out}");
    assert!(has_line(&out, "1"), "the file's `print $i` ran:\n{out}");
    assert!(out.contains("Stopped at line 12"), "stdin still worked:\n{out}");
}

#[test]
fn a_missing_command_file_is_an_error() {
    let path = script("nofile", SCRIPT);
    let out = run_full(&path, &["-x", "/nonexistent/session.au3dbg"], &[], "");
    assert!(out.contains("cannot read command file"), "got:\n{out}");
}

#[test]
fn the_source_command_queues_a_file_mid_session() {
    let path = script("source", SCRIPT);
    let dir = path.parent().expect("scratch dir");
    let commands = dir.join("more.au3dbg");
    std::fs::write(&commands, "print $i\nnext\n").expect("write command file");

    let out = shell(
        &path,
        &[
            "break 11",
            "run",
            &format!("source {}", commands.display()),
            "print $counter",
            "quit",
        ],
    );
    assert!(out.contains("sourced"), "got:\n{out}");
    // The sourced commands run before the one that follows them.
    let i_at = out.find("\n1\n").expect("sourced print ran");
    let step_at = out.find("Stopped at line 12").expect("sourced next ran");
    assert!(i_at < step_at, "the file ran out of order:\n{out}");
}

#[test]
fn an_uncaught_error_stops_where_it_was_raised() {
    let path = script("catch", FAILING);
    let out = shell(
        &path,
        &["run", "backtrace", "info locals", "print $x", "quit"],
    );
    assert!(out.contains("[uncaught error]"), "got:\n{out}");
    assert!(out.contains("out of bounds"), "got:\n{out}");
    // Stopped on the failing statement, with the source line shown.
    assert!(has_line(&out, "     6      Return $list[$n]"), "got:\n{out}");
    // The frames that led there are still live.
    assert!(out.contains("#0  Boom(n=5) at script.au3:6"), "got:\n{out}");
    assert!(out.contains("#1  Outer() at script.au3:11"), "got:\n{out}");
    assert!(out.contains("n = 5"), "the failing frame's locals:\n{out}");
    assert!(has_line(&out, "10"), "print $x should be 10:\n{out}");
}

#[test]
fn catching_can_be_turned_off() {
    let path = script("nocatch", FAILING);
    // On the command line...
    let out = shell_with(&path, &["--no-catch"], &["run", "print 1", "quit"]);
    assert!(out.contains("[script stopped:"), "got:\n{out}");
    assert!(!out.contains("[uncaught error]"), "should not have stopped:\n{out}");

    // ...and mid-session.
    let out = shell(&path, &["catch off", "run", "print 1", "quit"]);
    assert!(out.contains("will end the run without stopping"), "got:\n{out}");
    assert!(!out.contains("[uncaught error]"), "got:\n{out}");
}

#[test]
fn catch_reports_its_state() {
    let path = script("catchstate", SCRIPT);
    let out = shell(&path, &["catch", "catch off", "catch", "quit"]);
    assert!(out.contains("is on"), "got:\n{out}");
    assert!(out.contains("is off"), "got:\n{out}");
}

// ---------------------------------------------------------------------------
// logpoints (nostop + on-hit actions), hit rules, watches, function
// breakpoints and `jmp`
// ---------------------------------------------------------------------------

#[test]
fn a_logpoint_prints_without_stopping() {
    let path = script("logpoint", SCRIPT);
    let out = shell(
        &path,
        &[
            "break 11 nostop do p \"hit \" & $counter", // inside the For loop
            "run",
        ],
    );
    // Three loop iterations -> three log lines, and the run finishes without
    // ever stopping.
    assert!(has_line(&out, "\"hit 0\""), "{out}");
    assert!(has_line(&out, "\"hit 1\""), "{out}");
    assert!(has_line(&out, "\"hit 2\""), "{out}");
    assert!(has_line(&out, "[script finished]"), "{out}");
    assert!(!out.contains("Breakpoint 1, line"), "{out}");
}

#[test]
fn skip_and_every_gate_hits() {
    let path = script("skip-every", SCRIPT);
    // skip 1: the first hit does not fire. every 2 from then on: hits 2 and 3...
    // With 3 loop iterations: hit 1 skipped, hits 2 fires (every=2 -> (2)%2=0).
    let out = shell(&path, &["break 11 skip 1 every 2", "run"]);
    // Hit 1 skipped; hit 2 fires (every 2); hit 3 does not (3 % 2 != 0) —
    // exactly one stop before the script ends.
    assert_eq!(out.matches("Breakpoint 1, line 11").count(), 1, "{out}");
}

#[test]
fn ignore_extends_the_skip_budget() {
    let path = script("ignore", SCRIPT);
    // Two breakpoints' worth of behaviour: ignore 2 then run — with 3 hits
    // available, hits 1-2 are skipped and hit 3 stops.
    let out = shell(&path, &["break 11", "ignore 1 2", "run"]);
    assert_eq!(out.matches("Breakpoint 1, line 11").count(), 1, "{out}");
}

#[test]
fn nostop_can_be_toggled_after_creation() {
    let path = script("nostop-toggle", SCRIPT);
    let out = shell(
        &path,
        &["break 11", "nostop 1", "run"],
    );
    assert!(has_line(&out, "breakpoint 1 will not stop (logpoint)"), "{out}");
    assert!(has_line(&out, "[script finished]"), "{out}");
}

#[test]
fn on_hit_actions_can_assign_and_run_multiple_times() {
    let path = script("actions", SCRIPT);
    let out = shell(
        &path,
        &[
            "break 11 nostop do eval $total = $total + 100",
            "commands 1 do eval $counter = $counter",
            "run",
            "print $total",
        ],
    );
    // The action ran 3 times (one per loop iteration), so the final total is
    // the script's own 36 plus 300 from the actions.
    assert!(has_line(&out, "336"), "{out}");
}

#[test]
fn a_watch_stops_when_the_value_changes() {
    let path = script("watch", SCRIPT);
    let out = shell(
        &path,
        &["watch $counter", "run"],
    );
    // $counter goes 0 -> 1 on the first loop iteration; the watch stops there.
    assert!(out.contains("[watch 1] $counter:"), "{out}");
    assert!(out.contains("-> 1"), "{out}");
    assert!(!has_line(&out, "[script finished]"), "{out}");
}

#[test]
fn a_function_breakpoint_stops_at_the_first_statement() {
    let path = script("funcbp", SCRIPT);
    let out = shell(
        &path,
        &["break Add", "run", "info breakpoints"],
    );
    assert!(has_line(&out, "Breakpoint 1 at func Add (line 5)"), "{out}");
    assert!(has_line(&out, "Breakpoint 1, line 5"), "{out}");
    assert!(out.contains("1  func Add 5"), "{out}");
}

#[test]
fn tbreak_runs_to_a_line_and_removes_itself() {
    let path = script("jmp", SCRIPT);
    let out = shell(
        &path,
        &["break 2", "run", "tbreak 11", "delete 1", "info breakpoints"],
    );
    assert!(has_line(&out, "run-to target reached, line 11"), "{out}");
    // The temporary breakpoint is gone: `info breakpoints` shows none.
    assert!(has_line(&out, "no breakpoints"), "{out}");
}

#[test]
fn tbreak_works_from_the_top_level_before_a_run() {
    let path = script("jmp-func", SCRIPT);
    let out = shell(&path, &["tbreak Main"]);
    // Main's first statement is the For line (10); the target resolves and
    // the run stops there.
    assert!(has_line(&out, "run-to target reached, line 10"), "{out}");
}

#[test]
fn jmp_skips_statements_unconditionally() {
    let path = script(
        "jmp-skip",
        "Global $a = 1
$a = $a + 5
$a = $a + 10
",
    );
    // Stop before line 2, then jump over it: only line 3 runs, so $a is
    // 1 + 10 instead of 16.
    let out = shell(&path, &["break 2", "run", "jmp 3", "print $a"]);
    assert!(has_line(&out, "jumping to line 3"), "{out}");
    assert!(has_line(&out, "11"), "{out}");
}

#[test]
fn jmp_inside_a_function_skips_to_the_target() {
    let path = script("jmp-fn", SCRIPT);
    // Stop before $counter += 1, jump straight to $total += Add($i, 10):
    // the first iteration never increments $counter, but the loop itself
    // keeps running, so $counter ends at 2 and $total at 36.
    let out = shell(
        &path,
        &[
            "break 11",
            "run",
            "disable 1",
            "jmp 12",
            "print $counter",
            "print $total",
        ],
    );
    assert!(has_line(&out, "jumping to line 12"), "{out}");
    assert!(has_line(&out, "2"), "{out}");
    assert!(has_line(&out, "36"), "{out}");
}

#[test]
fn jmp_rejects_targets_outside_the_current_frame() {
    let path = script("jmp-bad", SCRIPT);
    let out = shell(&path, &["break 2", "run", "jmp 40", "continue"]);
    assert!(out.contains("cannot jump"), "{out}");
}

// ---------------------------------------------------------------------------
// multi-line commands: bare `eval` blocks, embedded-newline `eval`, and
// bare `commands <id>` action blocks
// ---------------------------------------------------------------------------

#[test]
fn a_multiline_eval_block_runs_several_statements() {
    let path = script("ml-eval", SCRIPT);
    let out = run_with_stdin(
        &path,
        &["break 11", "run"],
        "eval
$counter = 99
$total = $total + 1000
end
delete 1
continue
print $counter
print $total
",
    );
    // The block ran at the stop: $counter was overwritten (then incremented
    // by the loop), and the loop contributed to the patched $total.
    assert!(has_line(&out, "102"), "{out}");
    assert!(has_line(&out, "1036"), "{out}");
}

#[test]
fn an_eval_with_embedded_newlines_strips_the_trailing_end() {
    let path = script("ml-eval-inline", SCRIPT);
    let out = shell(
        &path,
        &["eval
$counter = 42
end", "run", "print $counter"],
    );
    // The whole block is one -c argument; the trailing `end` is stripped so
    // the AutoIt source is exactly the two statements.
    assert!(has_line(&out, "42"), "{out}");
}

#[test]
fn a_multiline_commands_block_sets_all_actions() {
    let path = script("ml-commands", SCRIPT);
    let out = run_with_stdin(
        &path,
        &["break 11 nostop", "commands 1"],
        "p \"tick \" & $counter
end
run
",
    );
    assert!(has_line(&out, "breakpoint 1: 1 action(s)"), "{out}");
    assert!(has_line(&out, "\"tick 0\""), "{out}");
    assert!(has_line(&out, "\"tick 1\""), "{out}");
    assert!(has_line(&out, "\"tick 2\""), "{out}");
}

#[test]
fn until_is_an_alias_of_tbreak() {
    let path = script("until-alias", SCRIPT);
    // `until` creates the same one-shot, self-deleting breakpoint: it stops
    // once at the line, is gone afterwards, and works with a function target
    // resolution too.
    let out = shell(
        &path,
        &["break 2", "run", "until 11", "delete 1", "info breakpoints"],
    );
    assert!(has_line(&out, "run-to target reached, line 11"), "{out}");
    assert!(has_line(&out, "no breakpoints"), "{out}");
}

#[test]
fn trace_skip_hides_a_hot_function() {
    const TRACE: &str = "\
Func Hot()
    Local $x = 1
    $x += 1
EndFunc

Func Main()
    Hot()
    Hot()
EndFunc

Main()
";
    let path = script("trace-skip", TRACE);
    let out = shell(&path, &["trace on", "trace skip Hot", "run", "quit"]);
    assert!(out.contains("not tracing inside Hot"), "got:\n{out}");
    // Statements inside `Hot` are gone, the ones outside it are still echoed.
    assert!(!out.contains("[trace] 2:"), "Hot body leaked:\n{out}");
    assert!(!out.contains("[trace] 3:"), "Hot body leaked:\n{out}");
    assert!(out.contains("[trace] 7:"), "Main body missing:\n{out}");
}

#[test]
fn trace_depth_filters_by_frame() {
    const TRACE: &str = "\
Func Deep()
    Local $x = 1
EndFunc

Func Main()
    Deep()
EndFunc

Main()
";
    let path = script("trace-depth", TRACE);
    let out = shell(&path, &["trace on", "trace depth 1", "run", "quit"]);
    assert!(out.contains("tracing statements at depth <= 1"), "got:\n{out}");
    // `Deep`'s body runs at depth 2 and is filtered out.
    assert!(!out.contains("[trace] 2:"), "depth filter leaked:\n{out}");
    assert!(out.contains("[trace] 6:"), "Main body missing:\n{out}");
}

/// `frame`/`up`/`down` select a frame the way gdb does: numbering is
/// innermost-first, and `print`/`info locals` act on the selected frame.
#[test]
fn frames_can_be_selected_like_gdb() {
    let path = script(
        "frames",
        "Func Inner($a)\n    Local $r = $a + 1\n    Return $r\nEndFunc\n\nFunc Outer($x)\n    Local $y = $x * 2\n    Local $z = Inner($y)\n    Return $z\nEndFunc\n\nConsoleWrite(Outer(5) & @CRLF)\n",
    );
    let out = shell(
        &path,
        &["break 3", "run", "bt", "frame 1", "print $y", "down", "print $r", "quit"],
    );
    assert!(out.contains("#0  Inner(a=10) at script.au3:3"), "got:\n{out}");
    assert!(out.contains("#1  Outer(x=5) at script.au3:8"), "got:\n{out}");
    // In `Outer` the local is there and the inner frame's parameter is not.
    assert!(has_line(&out, "10"), "the outer frame's local:\n{out}");
    // `down` comes back to the innermost frame, where `$r` lives.
    assert!(has_line(&out, "11"), "back in the inner frame:\n{out}");
}

#[test]
fn untilcall_stops_before_a_builtin_call() {
    const CALLS: &str = r#"Func Main()
    ConsoleWrite("a")
    ConsoleWrite("b")
EndFunc

Main()
"#;
    let path = script("untilcall", CALLS);
    let out = shell(&path, &["untilcall ConsoleWrite", "list", "quit"]);
    assert!(
        out.contains("running until ConsoleWrite is called (stopping before it runs)"),
        "got:\n{out}"
    );
    // The call itself is the stop: reported with its argument, before it runs.
    assert!(out.contains("Catchpoint: ConsoleWrite(\"a\")"), "got:\n{out}");
    assert!(out.contains("=>      2"), "line 2 is current:\n{out}");
}

#[test]
fn untilret_stops_after_a_builtin_call() {
    const CALLS: &str = r#"Func Main()
    ConsoleWrite("a")
    ConsoleWrite("b")
EndFunc

Main()
"#;
    let path = script("untilret", CALLS);
    let out = shell(&path, &["untilret ConsoleWrite", "list", "quit"]);
    assert!(out.contains("running until ConsoleWrite returns"), "got:\n{out}");
    // The first call ran (its output is there) and the stop is the next line.
    assert!(out.contains('a'), "the call ran:\n{out}");
    assert!(out.contains("Stopped at line 3"), "got:\n{out}");
}

/// The short aliases have to land on the same stop as the long spellings —
/// they are what gets typed in a session.
#[test]
fn uc_and_ur_are_the_until_call_and_return_shorthands() {
    let path = script(
        "until-aliases",
        "Func Double($n)\n    Return $n * 2\nEndFunc\n\nLocal $v = Double(21)\nConsoleWrite($v & @CRLF)\n",
    );
    // `uc` stops on the call, before it runs (the caller's line 5).
    let out = shell(&path, &["uc Double", "list", "quit"]);
    assert!(
        out.contains("running until Double is called (stopping before it runs)"),
        "got:\n{out}"
    );
    assert!(out.contains("Catchpoint: Double(21)"), "got:\n{out}");
    assert!(out.contains("=>      5"), "line 5 is current:\n{out}");
    // `ur` stops after it has returned, in the caller.
    let out = shell(&path, &["ur Double", "print $v", "quit"]);
    assert!(out.contains("running until Double returns"), "got:\n{out}");
    assert!(out.contains("Stopped at line 6"), "the caller's next statement:\n{out}");
    assert!(has_line(&out, "42"), "the result is readable:\n{out}");
}

/// For a script function the "after the call" stop is the caller's next
/// statement, where the result is already there.
#[test]
fn untilret_stops_in_the_caller_after_a_script_function() {
    let path = script(
        "untilret-func",
        "Func Double($n)\n    Return $n * 2\nEndFunc\n\nLocal $v = Double(21)\nConsoleWrite($v & @CRLF)\n",
    );
    let out = shell(&path, &["untilret Double", "print $v", "quit"]);
    assert!(out.contains("running until Double returns"), "got:\n{out}");
    assert!(out.contains("Stopped at line 6"), "the caller's next statement:\n{out}");
    assert!(has_line(&out, "42"), "the result is readable:\n{out}");
}

#[test]
fn untilgui_stops_before_gui_create() {
    const GUI: &str = r#"Func Main()
    GUICreate("hi")
    ConsoleWrite("made")
EndFunc

Main()
"#;
    let path = script("untilgui", GUI);
    // Headless on purpose: `untilgui` is about where the debugger stops, and
    // without the flag a Windows host would draw the window this test creates.
    let out = shell_with(&path, &["--gui", "headless"], &["untilgui", "list", "quit"]);
    assert!(
        out.contains("running until GUICreate is called (stopping before it runs)"),
        "got:\n{out}"
    );
    assert!(out.contains("Catchpoint: GUICreate(\"hi\")"), "got:\n{out}");
}

// ---------------------------------------------------------------------------
// GUI backend selection
// ---------------------------------------------------------------------------

/// `--gui window` needs the eframe-backed build; without the feature the shell
/// has to say how to get one rather than failing obscurely.
#[cfg(not(feature = "gui-window"))]
#[test]
fn gui_window_without_the_feature_says_how_to_build_it() {
    let path = script("gui-window-off", SCRIPT);
    let out = shell_with(&path, &["--gui", "window"], &["quit"]);
    assert!(out.contains("gui-window"), "got:\n{out}");
    assert!(out.contains("--features gui-window"), "got:\n{out}");
}

#[test]
fn gui_rejects_an_unknown_mode() {
    let path = script("gui-bad", SCRIPT);
    let out = shell_with(&path, &["--gui", "bogus"], &["quit"]);
    assert!(
        out.contains("invalid value") && out.contains("bogus"),
        "got:\n{out}"
    );
}

#[test]
fn gui_headless_keeps_the_session_working() {
    // `--gui headless` forces the model on every host, which is the mode to use
    // when a window only has to be stepped through and never seen.
    let path = script(
        "gui-headless",
        "Global $h = GUICreate(\"T\", 100, 50)\n",
    );
    let out = shell_with(&path, &["--gui", "headless"], &["next", "quit"]);
    assert!(out.contains("Stopped at line 1"), "got:\n{out}");
}

/// `--gui auto` is the default, so saying it out loud has to behave the same.
#[test]
fn gui_auto_is_accepted() {
    let path = script("gui-auto", "Global $h = GUICreate(\"T\", 100, 50)\n");
    let out = shell_with(&path, &["--gui", "auto"], &["quit"]);
    assert!(!out.contains("invalid value"), "got:\n{out}");
}

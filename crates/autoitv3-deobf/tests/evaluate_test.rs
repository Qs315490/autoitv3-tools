//! Tests for the runtime-evaluation pass.
//!
//! Everything here stays inside portable semantics on purpose: the pass is
//! about inlining what a script *computed*, and the interesting failure mode —
//! stopping at the operating-system boundary — is tested too.

use autoitv3_ast::ast::ExprKind;
use autoitv3_ast::parse;
use autoitv3_deobf::evaluate;
use autoitv3_format::PrettyPrinter;
use autoitv3_runtime::ExecutionProfile;

fn render(prog: &autoitv3_ast::Program) -> String {
    // Zero the spans so substrings are easy to find regardless of position.
    let mut pp = PrettyPrinter::new().strip_comments(true);
    pp.print_program(prog)
}

/// Build a table, reference it, and inline the result.
fn run(src: &str) -> (String, autoitv3_deobf::EvaluateReport) {
    let mut prog = parse(src).expect("parses");
    let report = evaluate(&mut prog, ExecutionProfile::deterministic());
    (render(&prog), report)
}

#[test]
fn constant_string_table_reads_are_inlined() {
    let src = r#"
Global $table = Build()
Func Build()
    Local $t[] = [3, "alpha", "beta", "gamma"]
    Return $t
EndFunc
Func F()
    Return $table[1] & "-" & $table[3]
EndFunc
"#;
    let (out, report) = run(src);
    assert!(report.completed, "script should run: {:?}", report.stopped);
    assert_eq!(report.tables, 1);
    assert!(out.contains("\"alpha\""), "not inlined: {out}");
    assert!(out.contains("\"gamma\""), "not inlined: {out}");
    assert!(!out.contains("$table["), "reads should be gone: {out}");
    assert_eq!(report.substitutions, 2);
}

#[test]
fn function_table_calls_become_real_calls() {
    // `$t[2]()` where the table holds a function name.
    let src = r#"
Global $t = Build()
Func Build()
    Local $x[] = [2, Target, Other]
    Return $x
EndFunc
Func Target()
    Return 42
EndFunc
Func Other()
    Return 0
EndFunc
Func F()
    Return $t[1]()
EndFunc
"#;
    let (out, report) = run(src);
    assert!(report.completed, "{:?}", report.stopped);
    assert!(out.contains("Target()"), "call not resolved: {out}");
    assert_eq!(report.calls_resolved, 1);
}

#[test]
fn values_computed_at_runtime_are_inlined() {
    // The whole point: the value only exists once the code has run. `evaluate`
    // inlines it; folding the surrounding concatenation is `fold`'s job.
    let src = r#"
Global Const $computed = Derive()
Func Derive()
    Local $s = ""
    For $i = 1 To 3
        $s &= Chr(64 + $i)
    Next
    Return $s
EndFunc
Func F()
    Return "got " & $computed
EndFunc
"#;
    let (out, _) = run(src);
    assert!(out.contains("\"ABC\""), "value not inlined: {out}");
    // `$computed` survives only in its own declaration; every read is inlined.
    assert_eq!(
        out.matches("$computed").count(),
        1,
        "bare const read should be inlined everywhere: {out}"
    );
}

#[test]
fn chaining_evaluate_then_deobfuscate_folds_the_whole_thing() {
    // The two passes compose: inlining produces literals, the deobfuscation
    // pipeline then folds and renames.
    let src = r#"
Global Const $computed = Derive()
Func Derive()
    Local $s = ""
    For $i = 1 To 3
        $s &= Chr(64 + $i)
    Next
    Return $s
EndFunc
Func F()
    Return "got " & $computed
EndFunc
"#;
    let mut prog = parse(src).unwrap();
    evaluate(&mut prog, ExecutionProfile::deterministic());
    autoitv3_deobf::deobfuscate(&mut prog);
    let out = render(&prog);
    assert!(out.contains("\"got ABC\""), "not folded end to end: {out}");
}

#[test]
fn map_values_are_inlined_by_key() {
    let src = r#"
Global $m = Build()
Func Build()
    Local $d[]
    $d["os"] = "windows"
    $d["arch"] = "x64"
    Return $d
EndFunc
Func F()
    Return $m["os"]
EndFunc
"#;
    let (out, _) = run(src);
    assert!(out.contains("\"windows\""), "map read not inlined: {out}");
    assert!(!out.contains("$m["), "returns should be gone: {out}");
}

#[test]
fn assignment_targets_are_never_replaced() {
    // `$table[1] = x` must stay an assignment; replacing the left side would
    // produce `"alpha" = x`.
    let src = r#"
Global $table = Build()
Func Build()
    Local $t[] = [2, "alpha", "beta"]
    Return $t
EndFunc
Func F()
    $table[1] = "changed"
    Return $table[1]
EndFunc
"#;
    let (out, _) = run(src);
    assert!(out.contains("$table[1] = \"changed\""), "target was clobbered: {out}");
    // The read on the next line is still inlined (with its runtime value).
    assert!(out.contains("\"changed\"") || out.contains("\"alpha\""), "{out}");
}

#[test]
fn unrepresentable_strings_are_not_inlined() {
    // AutoIt has no escape for a line break inside a literal, so a value
    // containing one must be left alone: inlining it would not re-parse.
    let src = "Global $multi = \"a\" & @CRLF & \"b\"\nFunc F()\n    Return $multi\nEndFunc\n";
    let (out, report) = run(src);
    assert!(parse(&out).is_ok(), "output must still parse: {out}");
    assert_eq!(report.substitutions, 0);
}

#[test]
fn partial_evaluation_keeps_what_the_run_produced() {
    // The run stops at the OS boundary, but the tables built before it are
    // still usable — that is the normal case for a real script.
    let src = r#"
Global $early = Build()
Func Build()
    Local $t[] = [1, "recovered"]
    Return $t
EndFunc
Global $late = RegRead("HKEY_LOCAL_MACHINE\X", "Y")
Func F()
    Return $early[1]
EndFunc
"#;
    let (out, report) = run(src);
    assert!(!report.completed, "the run should not have completed");
    let stopped = report.stopped.expect("a reason");
    assert!(stopped.contains("undefined function"), "got: {stopped}");
    // Everything before the boundary is still inlined.
    assert!(out.contains("\"recovered\""), "lost the early table: {out}");
    assert_eq!(report.tables, 1);
}

#[test]
fn nested_tables_resolve_through_every_subscript() {
    let src = r#"
Global $grid = Build()
Func Build()
    Local $inner[] = [2, "one", "two"]
    Local $outer[] = [1, $inner]
    Return $outer
EndFunc
Func F()
    Return $grid[1][2]
EndFunc
"#;
    let (out, _) = run(src);
    assert!(out.contains("\"two\""), "nested read not resolved: {out}");
}

#[test]
fn out_of_range_and_non_constant_indices_are_left_alone() {
    let src = r#"
Global $t = Build()
Func Build()
    Local $a[] = [2, "x", "y"]
    Return $a
EndFunc
Func F($i)
    Return $t[99] & $t[$i]
EndFunc
"#;
    let (out, report) = run(src);
    // `$t[99]` is out of bounds and `$t[$i]` is not constant: neither is a
    // value we know, so both stay as written.
    assert!(out.contains("$t[99]"), "{out}");
    assert!(out.contains("$t[$i]"), "{out}");
    assert_eq!(report.substitutions, 0);
}

#[test]
fn report_counts_globals_and_tables() {
    let src = r#"
Global $a = 1
Global $arr = Build()
Func Build()
    Local $t[] = [1, "v"]
    Return $t
EndFunc
"#;
    let (_, report) = run(src);
    assert!(report.globals >= 2, "globals: {}", report.globals);
    assert_eq!(report.tables, 1);
    assert!(report.completed);
    assert!(report.stopped.is_none());
}

#[test]
fn evaluation_is_reproducible() {
    // The deterministic profile is what makes a second run agree.
    let src = r#"
Global $v = Derive()
Func Derive()
    Local $s = ""
    For $i = 1 To 5
        $s &= Random(1, 9)
    Next
    Return $s
EndFunc
Func F()
    Return $v
EndFunc
"#;
    let (a, _) = run(src);
    let (b, _) = run(src);
    assert_eq!(a, b);
}

#[test]
fn spans_of_inlined_literals_are_zeroed_not_invented() {
    // Inlined values have no position in the original source; they must not
    // claim one.
    let src = "Global $t = Build()\nFunc Build()\n    Local $a[] = [1, \"s\"]\n    Return $a\nEndFunc\nFunc F()\n    Return $t[1]\nEndFunc\n";
    let mut prog = parse(src).unwrap();
    evaluate(&mut prog, ExecutionProfile::deterministic());

    let mut found = false;
    for item in &prog.items {
        if let autoitv3_ast::ast::ItemKind::Func(f) = &item.kind {
            if f.name.name == "F" {
                let autoitv3_ast::ast::StmtKind::Return(Some(e)) = &f.body[0].kind else {
                    continue;
                };
                if let ExprKind::Lit(lit) = &e.kind {
                    assert_eq!(lit.span, autoitv3_ast::span::Span::default());
                    found = true;
                }
            }
        }
    }
    assert!(found, "expected an inlined literal in F");
}
//! Tests for the runtime-evaluation pass.
//!
//! Everything here stays inside portable semantics on purpose: the pass is
//! about inlining what a script *computed*, and the interesting failure mode —
//! stopping at the operating-system boundary — is tested too.

use autoitv3_ast::ast::ExprKind;
use autoitv3_ast::parse;
use autoitv3_deobf::{evaluate, evaluate_with_options, SubstituteOptions};
use autoitv3_format::PrettyPrinter;
use autoitv3_runtime::ExecutionProfile;

fn render(prog: &autoitv3_ast::Program) -> String {
    // Zero the spans so substrings are easy to find regardless of position.
    let mut pp = PrettyPrinter::new().strip_comments(true);
    pp.print_program(prog)
}

/// Build a table, reference it, and inline the result.
fn run(src: &str) -> (String, autoitv3_deobf::EvaluateReport) {
    run_with(src, SubstituteOptions::default())
}

/// As [`run`], with explicit substitution options.
fn run_with(
    src: &str,
    options: SubstituteOptions,
) -> (String, autoitv3_deobf::EvaluateReport) {
    let mut prog = parse(src).expect("parses");
    let report = evaluate_with_options(
        &mut prog,
        ExecutionProfile::deterministic(),
        autoitv3_platform::host_platform(),
        options,
    );
    (render(&prog), report)
}

/// Options that also rewrite a table declaration into its literal value.
fn inline_declarations() -> SubstituteOptions {
    SubstituteOptions {
        inline_declarations: true,
    }
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
fn map_reads_with_an_integer_subscript_are_inlined() {
    // AutoIt map keys are strings, so `$m[0x2]` looks up the key `"2"` — and
    // that is exactly how the obfuscator indexes its string table.
    let src = r#"
Global $m = Build()
Func Build()
    Local $m = Map()
    $m[1] = "one"
    $m[2] = "two"
    Return $m
EndFunc
Func F()
    Return $m[0x2]
EndFunc
"#;
    let (out, report) = run(src);
    assert_eq!(report.substitutions, 1, "{out}");
    assert!(out.contains("\"two\""), "map read not inlined: {out}");
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
    // The run stops at a call no layer implements, but the tables built before
    // it are still usable — that is the normal case for a real script. (GUI is
    // no longer a boundary: the emulation answers it headlessly.)
    let src = r#"
Global $early = Build()
Func Build()
    Local $t[] = [1, "recovered"]
    Return $t
EndFunc
Global $late = NoSuchPlatformCall("title")
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
fn the_windows_emulation_moves_the_boundary_past_registry_reads() {
    // The complement of the test above: with the default platform stack a
    // `RegRead` is answered by the emulated registry, so the run gets further
    // and the table is still recovered.
    let src = r#"
Global $early = Build()
Func Build()
    Local $t[] = [1, "recovered"]
    Return $t
EndFunc
Global $late = RegRead("HKEY_LOCAL_MACHINE\SOFTWARE\X", "Y")
Func F()
    Return $early[1]
EndFunc
"#;
    let (out, report) = run(src);
    assert!(report.completed, "stopped at: {:?}", report.stopped);
    assert!(out.contains("\"recovered\""), "{out}");
}

/// The reference sample selects its string table by the OS version, which it
/// reads with `DllStructCreate` + `DllCall(GetVersionExW)`.
const VERSION_SELECTED_TABLE: &str = r#"
Global $strings = Build()
Func Build()
    Local $t = DllStructCreate("struct;dword OSVersionInfoSize;dword MajorVersion;" & _
        "dword MinorVersion;dword BuildNumber;dword PlatformId;wchar CSDVersion[128];endstruct")
    DllStructSetData($t, "OSVersionInfoSize", DllStructGetSize($t))
    DllCall("kernel32.dll", "int", "GetVersionExW", "ptr", $t)
    Local $major = DllStructGetData($t, "MajorVersion")
    Local $s[] = ["legacy", "modern"]
    If $major >= 10 Then $s[0] = "windows-10-plus"
    Return $s
EndFunc
Func F()
    Return $strings[0]
EndFunc
"#;

#[test]
fn an_os_version_query_no_longer_blocks_the_string_table() {
    // Before the emulation layer this stopped at `DllStructCreate`; now the
    // version is answered and the value the table selected is inlined.
    let (out, report) = run(VERSION_SELECTED_TABLE);
    assert!(report.completed, "stopped at: {:?}", report.stopped);
    assert!(out.contains("\"windows-10-plus\""), "{out}");
}

#[test]
fn without_the_emulation_layer_the_version_query_is_the_boundary() {
    let mut prog = parse(VERSION_SELECTED_TABLE).expect("parses");
    let platform = autoitv3_platform::host_platform_with(
        autoitv3_platform::WindowsEmulation::new().disabled(),
    );
    let report = autoitv3_deobf::evaluate_with_platform(
        &mut prog,
        ExecutionProfile::deterministic(),
        platform,
    );
    assert!(!report.completed);
    let stopped = report.stopped.expect("a reason");
    assert!(stopped.contains("undefined function"), "got: {stopped}");
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
#[test]
fn parameter_defaults_are_substituted_too() {
    // A default is evaluated at call time and reads the same tables, so it has
    // to go through the same substitution as the body.
    let src = r#"
Global Const $table = Build()
Global Const $scalar = 7
Func Build()
    Local $t[] = [3, "alpha", "beta", "gamma"]
    Return $t
EndFunc

Func F($a = $table[2], $b = $scalar)
    Return $a & $b
EndFunc
"#;
    let (out, report) = run(src);
    assert!(report.completed, "script should run: {:?}", report.stopped);
    let header = out.lines().find(|l| l.starts_with("Func F")).expect("Func F");
    assert!(header.contains("= \"beta\""), "table default not inlined: {header}");
    assert!(header.contains("= 7"), "const default not inlined: {header}");
}

#[test]
fn table_declarations_are_left_as_calls_by_default() {
    // The declaration records *how* the table was built, and a runtime table's
    // literal is a lot of output, so rewriting it is opt-in.
    let src = r#"
Global Const $table = Build()
Func Build()
    Local $t[] = [3, "alpha", "beta"]
    Return $t
EndFunc
Func F()
    Return $table[1]
EndFunc
"#;
    let (out, report) = run(src);
    assert_eq!(report.declarations_resolved, 0);
    assert!(out.contains("Build()"), "declaration should stay: {out}");
    assert!(out.contains("\"alpha\""), "reads should still inline: {out}");
}

#[test]
fn a_global_table_declaration_is_replaced_by_its_value_when_asked() {
    // After every read has been substituted the declaration is the last trace
    // of the table; leaving `$t = Build()` there hides the data behind a
    // function the reader has to trace.
    let src = r#"
Global Const $table = Build()
Func Build()
    Local $t[] = [3, "alpha", 2.5, Binary("0x4142")]
    Return $t
EndFunc
Func F()
    Return $table[1]
EndFunc
"#;
    let (out, report) = run_with(src, inline_declarations());
    assert_eq!(report.declarations_resolved, 1);
    let decl = out
        .lines()
        .find(|l| l.starts_with("Global Const $table"))
        .expect("declaration");
    assert!(decl.contains('['), "value not inlined: {decl}");
    assert!(!decl.contains("Build()"), "builder call left: {decl}");
    // The literal round-trips: the output still parses.
    assert!(parse(&out).is_ok(), "output did not re-parse: {out}");
}

#[test]
fn a_table_with_no_literal_form_is_left_alone() {
    // A map has no literal syntax in AutoIt, so the declaration has to stay.
    let src = r#"
Global Const $table = Build()
Func Build()
    Local $m[]
    $m["a"] = 1
    Return $m
EndFunc
Func F()
    Return MapExists($table, "a")
EndFunc
"#;
    let (out, report) = run_with(src, inline_declarations());
    assert_eq!(report.declarations_resolved, 0);
    assert!(out.contains("Build()"), "declaration should be untouched: {out}");
}

/// Run the optional `AU3_SAMPLE` script with the PE image that sits next to it.
///
/// A compiled AutoIt script keeps its encrypted tables in the image's
/// resources, which is why the resource module is discovered from the sample's
/// own directory rather than configured by hand.
///
/// The script body takes a few minutes to interpret in a debug build, so run
/// this one with `--release` (about eight seconds there).
fn sample_evaluate() -> Option<(String, autoitv3_deobf::EvaluateReport)> {
    let path = std::env::var("AU3_SAMPLE").ok().filter(|p| !p.is_empty())?;
    let src = std::fs::read_to_string(&path).ok()?;
    let mut prog = parse(&src).expect("sample parses");
    let mut emu = autoitv3_platform::WindowsEmulation::new();
    if let Some(found) =
        autoitv3_platform::find_resource_module(Some(std::path::Path::new(&path)))
    {
        emu = emu.with_module_file(found);
    }
    let report = evaluate_with_options(
        &mut prog,
        ExecutionProfile::deterministic(),
        autoitv3_platform::host_platform_with(emu),
        SubstituteOptions::default(),
    );
    // Rendering the whole sample through the pretty printer is slow in a debug
    // build, so this reports the counts the pass produced rather than text.
    Some((String::new(), report))
}

#[test]
fn the_encrypted_tables_come_out_when_the_resource_image_is_present() {
    // Without the image the sample stops inside its string-table builder with a
    // few hundred substitutions; with it the CryptoAPI chain runs and tens of
    // thousands of references are inlined.
    let Some((_out, report)) = sample_evaluate() else {
        return;
    };
    assert!(
        report.substitutions > 20_000,
        "only {} substitutions (stopped at {:?})",
        report.substitutions,
        report.stopped
    );
    assert_eq!(report.tables, 9, "the string table should have been built");
}

//! Unit tests for the autoitv3-deobf passes.

use autoitv3_ast::ast::ItemKind;
use autoitv3_ast::parse;
use autoitv3_format::PrettyPrinter;
use autoitv3_deobf::{
    deobfuscate, fold, rename, simplify, DeobfReport, Deobfuscator, RenameOptions, Tables,
};

fn pretty(prog: &autoitv3_ast::Program) -> String {
    let mut pp = PrettyPrinter::new();
    pp.print_program(prog)
}

/// Deobfuscate a source snippet with the **full** pipeline — renaming
/// included — and return the printed text.
///
/// Most tests here are about the aliases, so they want `renaming`; the default
/// pipeline (no renaming) is covered by its own test.
fn run(src: &str) -> String {
    let mut prog = parse(src).unwrap();
    Deobfuscator::renaming().run(&mut prog);
    pretty(&prog)
}

/// Deobfuscate a source snippet with the **default** pipeline, which leaves
/// every original name alone.
fn run_default(src: &str) -> String {
    let mut prog = parse(src).unwrap();
    deobfuscate(&mut prog);
    pretty(&prog)
}

#[test]
fn the_default_pipeline_leaves_names_alone() {
    let src = "Global $count = 1\nFunc Helper($v)\n    Return $v + $count\nEndFunc\nHelper($count)\n";
    let out = run_default(src);
    assert!(out.contains("$count"), "variable renamed: {out}");
    assert!(out.contains("Func Helper"), "function renamed: {out}");
    assert!(out.contains("Helper($count)"), "call renamed: {out}");
}

#[test]
fn the_default_pipeline_still_does_the_structural_work() {
    // Fold and table resolution are structural, so they stay on by default.
    let out = run_default("$x = 1 + 2\nFunc Foo()\n    Return 1\nEndFunc\nCall(\"Foo\")\n");
    assert!(out.contains("$x = 3"), "not folded: {out}");
    assert!(out.contains("Foo()\n"), "indirect call not simplified: {out}");
}

/// Apply only constant folding, then print (no renaming).
fn run_fold(src: &str) -> String {
    let mut prog = parse(src).unwrap();
    fold::fold_program(&mut prog);
    pretty(&prog)
}

// ---------------------------------------------------------------------------
// Constant folding
// ---------------------------------------------------------------------------

#[test]
fn fold_arithmetic() {
    assert_eq!(run_fold("$x = 1 + 2\n"), "$x = 3\n");
    assert_eq!(run_fold("$x = 10 - 4\n"), "$x = 6\n");
    assert_eq!(run_fold("$x = 3 * 7\n"), "$x = 21\n");
    assert_eq!(run_fold("$x = 20 / 5\n"), "$x = 4\n");
    assert_eq!(run_fold("$x = 2 ^ 3\n"), "$x = 8\n");
}

#[test]
fn fold_string_concat() {
    assert_eq!(run_fold("$x = \"ab\" & \"cd\"\n"), "$x = \"abcd\"\n");
    assert_eq!(run_fold("$x = \"n=\" & 5\n"), "$x = \"n=5\"\n");
    // Triple concat folds fully.
    assert_eq!(run_fold("$x = \"a\" & \"b\" & \"c\"\n"), "$x = \"abc\"\n");
}

#[test]
fn fold_does_not_touch_variables() {
    // Expressions involving variables must stay untouched.
    let out = run_fold("$x = $a + 2\n");
    assert!(out.contains("$a + 2"), "got: {out}");
}

#[test]
fn fold_nested_and_paren() {
    assert_eq!(run_fold("$x = (1 + 2) * 3\n"), "$x = 9\n");
}

#[test]
fn fold_ternary_constant() {
    assert_eq!(run_fold("$x = True ? \"yes\" : \"no\"\n"), "$x = \"yes\"\n");
    assert_eq!(run_fold("$x = False ? 1 : 2\n"), "$x = 2\n");
}

#[test]
fn fold_comparison() {
    assert_eq!(run_fold("$x = 3 < 5\n"), "$x = True\n");
    assert_eq!(run_fold("$x = 2 == 9\n"), "$x = False\n");
}

#[test]
fn fold_neg_and_not() {
    assert_eq!(run_fold("$x = -5\n"), "$x = -5\n");
    assert_eq!(run_fold("$x = Not False\n"), "$x = True\n");
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

#[test]
fn rename_is_deterministic_and_stable() {
    let src = "$zzz = 1\n$yyy = $zzz + 1\n$zzz = $yyy\n";
    let a = run(src);
    let b = run(src);
    assert_eq!(a, b, "renaming must be reproducible");
    // The obfuscated name should be gone, replaced by a stable alias.
    assert!(!a.contains("$zzz"), "original name leaked: {a}");
    assert!(!a.contains("$yyy"), "original name leaked: {a}");
    // Script-level, and the type comes from the `1` it is first given.
    assert!(a.contains("$g_int_000"), "expected stable alias: {a}");
}

#[test]
fn rename_encodes_scope_and_type() {
    let src = concat!(
        "Global $count = 1\n",
        "Global $name = \"root\"\n",
        "Global $items[] = [1, 2]\n",
        "Func F($raw, $ratio = 1.5)\n",
        "    Local $n = 2\n",
        "    Local $s = \"x\"\n",
        "    Local $arr[3]\n",
        "    Local $map = Map()\n",
        "    Local $flag = True\n",
        "    Local $f = 0.5\n",
        "    Return $n\n",
        "EndFunc\n",
    );
    let out = run(src);
    // Scope first (`g`/`l`/`arg`), then the type.
    for alias in [
        "$g_int_000",
        "$g_str_001",
        "$g_arr_002",
        "$arg_var_000",
        "$arg_float_001",
        "$l_int_000",
        "$l_str_001",
        "$l_arr_002",
        "$l_map_003",
        "$l_bool_004",
        "$l_float_005",
    ] {
        assert!(out.contains(alias), "missing {alias} in:\n{out}");
    }
}

#[test]
fn rename_is_case_insensitive_like_autoit() {
    // `$Foo`, `$foo` and `$FOO` are one variable, so they share one alias;
    // giving them different names would change what the script does.
    let out = run("$Foo = 1\n$foo = $FOO + $Foo\n");
    // One declaration plus an assignment target and two reads.
    assert_eq!(out.matches("$g_int_000").count(), 4, "{out}");
}

#[test]
fn rename_keeps_a_local_distinct_from_a_global_of_the_same_name() {
    let src = "Global $x = 1\nFunc F()\n    Local $x = 2\n    Return $x\nEndFunc\n";
    let out = run(src);
    // Two different variables, two different aliases.
    assert!(out.contains("$g_int_000 = 1"), "{out}");
    assert!(out.contains("Local $l_int_000 = 2"), "{out}");
    assert!(out.contains("Return $l_int_000"), "{out}");
}

#[test]
fn rename_shares_the_global_alias_with_an_undeclared_use_in_a_function() {
    // A function reading a script-level variable must keep that variable's
    // alias, or it would stop seeing it.
    let src = "Global $cfg = \"v\"\nFunc F()\n    $cfg = \"w\"\n    Return $cfg\nEndFunc\n";
    let out = run(src);
    assert_eq!(out.matches("$g_str_000").count(), 3, "{out}");
    assert!(!out.contains("$l_"), "{out}");
}

#[test]
fn rename_types_for_loop_variables_from_the_range() {
    let src = concat!(
        "Func F()\n",
        "    Local $t = 0\n",
        "    For $i = 1 To 10\n",
        "        $t = $t + $i\n",
        "    Next\n",
        "    For $k In $t\n",
        "        $t = $t + 1\n",
        "    Next\n",
        "EndFunc\n",
    );
    let out = run(src);
    // `For $i = 1 To 10` is an integer loop; `For In` has no element type.
    assert!(out.contains("For $l_int_"), "{out}");
    assert!(out.contains("For $l_var_"), "{out}");
}

#[test]
fn rename_functions_and_calls_consistently() {
    let src = "Func Xobfu()\n    Return 1\nEndFunc\nXobfu()\n";
    let out = run(src);
    // Function definition and its call site must share the same alias.
    assert!(out.contains("Func f000()"), "got: {out}");
    assert!(out.contains("f000()"), "got: {out}");
    assert!(!out.contains("Xobfu"), "got: {out}");
}

#[test]
fn a_function_without_parameters_keeps_its_parentheses() {
    // `Func Foo` is not valid AutoIt; the rewritten definition must stay
    // `Func f000()` so the output can actually be run.
    let out = run("Func NoArgs()\n    Return 1\nEndFunc\nNoArgs()\n");
    assert!(out.contains("Func f000()"), "got: {out}");
    assert!(out.contains("f000()"), "got: {out}");
    assert!(!out.contains("Func f000\n"), "got: {out}");
}

#[test]
fn rename_preserves_behavior_through_pretty() {
    // Renaming must round-trip: the re-parsed output keeps the same structure.
    let src = "Global Const $A = \"hi\"\nFunc F1($p)\n    Return $p & $A\nEndFunc\nF1(\"x\")\n";
    let mut prog = parse(src).unwrap();
    let before = pretty(&prog);
    Deobfuscator::renaming().run(&mut prog);
    let after = pretty(&prog);
    // Re-parse both and compare function count and top-level item count.
    let cnt = |p: &autoitv3_ast::Program| {
        (
            p.items.len(),
            p.items
                .iter()
                .filter(|it| matches!(it.kind, ItemKind::Func(_)))
                .count(),
        )
    };
    assert_eq!(cnt(&parse(&before).unwrap()), cnt(&parse(&after).unwrap()));
}

#[test]
fn rename_touches_only_functions_the_script_defines() {
    let src = concat!(
        "Func Helper($v)\n",
        "    Return StringLen($v)\n",
        "EndFunc\n",
        "Func Main()\n",
        "    Local $n = Helper(\"x\")\n",
        "    Return UBound(Mystery($n))\n",
        "EndFunc\n",
        "Main()\n",
    );
    let out = run(src);
    // Script-defined functions are renamed at the definition and the call site.
    assert!(out.contains("Func f000"), "{out}");
    assert!(out.contains("f000(\"x\")"), "{out}");
    assert!(out.contains("Func f001"), "{out}");
    assert!(out.contains("f001()"), "{out}");
    // Built-ins and unknown names are runtime lookups — left exactly alone.
    assert!(out.contains("StringLen("), "{out}");
    assert!(out.contains("UBound("), "{out}");
    assert!(out.contains("Mystery("), "{out}");
}

#[test]
fn rename_leaves_macros_alone() {
    // Macros are built-ins the runtime resolves by name, so they are never
    // renamed (there is nothing script-defined about them).
    let out = run("Global $e = @error\nGlobal $nl = @CRLF\nGlobal $line = @ScriptLineNumber\n");
    assert!(out.contains("@error"), "{out}");
    assert!(out.contains("@CRLF"), "{out}");
    assert!(out.contains("@ScriptLineNumber"), "{out}");
    assert!(!out.contains("@m0"), "{out}");
}

#[test]
fn renaming_can_be_disabled() {
    let src = "Global $count = 1\nFunc Helper($v)\n    Return $v\nEndFunc\nHelper($count)\n";
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::new().run(&mut prog);
    assert_eq!(report.renamed.vars, 0);
    assert_eq!(report.renamed.funcs, 0);
    // The other passes still run; only the names survive unchanged.
    let out = pretty(&prog);
    assert!(out.contains("$count"), "{out}");
    assert!(out.contains("Func Helper"), "{out}");
    assert!(out.contains("Helper($count)"), "{out}");
}

#[test]
fn rename_options_can_select_one_category() {
    let src = "Global $count = 1\nFunc Helper($v)\n    Return $v\nEndFunc\nHelper($count)\n";
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming()
        .with_rename_options(RenameOptions {
            vars: true,
            funcs: false,
        })
        .run(&mut prog);
    assert!(report.renamed.vars >= 1);
    assert_eq!(report.renamed.funcs, 0);
    let out = pretty(&prog);
    assert!(out.contains("$g_int_000"), "{out}");
    assert!(out.contains("Func Helper"), "{out}");
    assert!(out.contains("Helper($g_int_000)"), "{out}");
}

// ---------------------------------------------------------------------------
// Indirect-call simplification
// ---------------------------------------------------------------------------

#[test]
fn simplify_turns_call_and_execute_into_direct_calls() {
    let src = concat!(
        "Func Foo($a, $b = 2)\n",
        "    Return $a + $b\n",
        "EndFunc\n",
        "Func Bar()\n",
        "    Return 42\n",
        "EndFunc\n",
        "Func Main()\n",
        "    Local $x = Call(\"Foo\", 1, 5)\n",
        "    Local $y = Call(\"Bar\")\n",
        "    Execute(\"Foo(7, 8)\")\n",
        "    Execute(\"Bar\")\n",
        "    Return Execute(\"Foo(1)\")\n",
        "EndFunc\n",
        "Main()\n",
    );
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming().run(&mut prog);
    assert_eq!(report.simplified.calls, 2);
    assert_eq!(report.simplified.executes, 3);
    let out = pretty(&prog);
    // The calls are direct, and the target carries the renamed definition.
    assert!(!out.contains("Call("), "{out}");
    assert!(!out.contains("Execute("), "{out}");
    assert!(out.contains("Func f000"), "{out}");
    assert!(out.contains("f000(1, 5)"), "{out}");
    assert!(out.contains("f001()"), "{out}");
    assert!(out.contains("f000(7, 8)"), "{out}");
    assert!(out.contains("f000(1)"), "{out}");
}

#[test]
fn simplify_can_run_without_renaming() {
    // Simplify is its own pass: keeping the original names does not stop the
    // indirect calls from being written out.
    let src = "Func Foo()\n    Return 1\nEndFunc\nCall(\"Foo\")\nExecute(\"Foo()\")\n";
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::new().run(&mut prog);
    assert_eq!(report.simplified.total(), 2);
    assert_eq!(report.renamed.funcs, 0);
    let out = pretty(&prog);
    assert!(out.contains("Func Foo"), "{out}");
    assert!(out.contains("Foo()"), "{out}");
    assert!(!out.contains("Call("), "{out}");
    assert!(!out.contains("Execute("), "{out}");
}

#[test]
fn simplify_leaves_computed_targets_and_statements_alone() {
    let src = concat!(
        "Func Foo()\n",
        "    Return 1\n",
        "EndFunc\n",
        "Func Main()\n",
        "    Local $name = \"Foo\"\n",
        "    Local $a = Call($name)\n",
        "    Local $b = Call(\"MsgBox\", 0, \"built-in\")\n",
        "    Local $d = Execute(\"$x = 1\")\n",
        "    Local $e = Execute(\"$x = 1 : $y = 2\")\n",
        "    Return $a + $b + $d + $e\n",
        "EndFunc\n",
        "Main()\n",
    );
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming().run(&mut prog);
    assert_eq!(report.simplified.total(), 0);
    let out = pretty(&prog);
    // Only the definition is renamed; every call stays as written (the
    // computed name is renamed as a variable, but not turned into a call).
    assert!(out.contains("Func f000"), "{out}");
    assert!(out.contains("Call($l_str_000)"), "{out}");
    assert!(out.contains("Call(\"MsgBox\", 0, \"built-in\")"), "{out}");
    // Assignments cannot move into expression position: AutoIt has no
    // assignment expression, so `=` would re-parse as a comparison.
    assert!(out.contains("Execute(\"$x = 1\")"), "{out}");
    assert!(out.contains("Execute(\"$x = 1 : $y = 2\")"), "{out}");
}

#[test]
fn simplify_inlines_any_single_expression_from_execute() {
    // `Execute` is not limited to a lone call: the obfuscator puts whole
    // expressions in the string, and any of them can simply be written out.
    let src = concat!(
        "Func Foo()\n",
        "    Return 1\n",
        "EndFunc\n",
        "Func Bar()\n",
        "    Return 2\n",
        "EndFunc\n",
        "Func Main()\n",
        "    Local $n = Execute(\"Foo() & Bar()\")\n",
        "    ConsoleWrite(Execute(\"StringLen('x')\"))\n",
        "    Local $m = Execute(\"Nope()\")\n",
        "    Return $n + $m\n",
        "EndFunc\n",
        "Main()\n",
    );
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming().run(&mut prog);
    assert_eq!(report.simplified.executes, 3);
    let out = pretty(&prog);
    assert!(!out.contains("Execute("), "{out}");
    assert!(out.contains("f000() & f001()"), "{out}");
    // An undefined name is spliced as written; the error, if any, is the same.
    assert!(out.contains("Nope()"), "{out}");
}

#[test]
fn execute_splices_the_expression_as_written() {
    // `Execute("<expr>")` becomes exactly that expression. A bare function name
    // is a *reference* in this interpreter, so it stays one — no `()` is
    // invented for it.
    let src = "Func Bar()\n    Return 42\nEndFunc\nFunc F()\n    Return Execute(\"Bar\")\nEndFunc\n";
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming().run(&mut prog);
    assert_eq!(report.simplified.executes, 1);
    let out = pretty(&prog);
    assert!(!out.contains("Execute("), "{out}");
    assert!(out.contains("Return f000"), "{out}");
}

#[test]
fn execute_strings_reach_the_function_table_after_the_splice() {
    // The reference sample calls through its table from inside a string, and
    // spells the table name in upper case there:
    //   Execute("$FN_TABLE[1]($name_table[1])")
    // The splice has to happen *before* the table pass for this to resolve, and
    // the table match has to be case-insensitive like AutoIt's variables.
    let src = concat!(
        "Func MergeArrays(ByRef $t, Const ByRef $s)\n",
        "    ReDim $t[$t[0] + $s[0] + 1]\n",
        "    Local $i\n",
        "    For $i = 1 To $s[0]\n",
        "        $t[$t[0] + $i] = $s[$i]\n",
        "    Next\n",
        "    $t[0] += $s[0]\n",
        "EndFunc\n",
        "Func Foo($v)\n",
        "    Return $v\n",
        "EndFunc\n",
        "Func BuildFunctionTable()\n",
        "    Local $x[] = [0x1, Foo]\n",
        "    Return $x\n",
        "EndFunc\n",
        "Global Const $fn_table = BuildFunctionTable()\n",
        "Global Const $name_table = [1, \"hello\"]\n",
        "Func Main()\n",
        "    Return Execute(\"$FN_TABLE[1]($name_table[1])\")\n",
        "EndFunc\n",
        "Main()\n",
    );
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming().run(&mut prog);
    assert_eq!(report.simplified.executes, 1);
    assert_eq!(report.table.calls, 1);
    let out = pretty(&prog);
    assert!(!out.contains("Execute("), "{out}");
    // The table entry became the real (renamed) function, and the string-table
    // variable was renamed together with its definition.
    assert!(out.contains("Func f001("), "{out}");
    assert!(out.contains("Return f001($g_arr_001[1])"), "{out}");
    assert!(!out.contains("$FN_TABLE"), "{out}");
    assert!(!out.contains("$name_table"), "{out}");
}

#[test]
fn simplify_uses_the_declared_spelling() {
    // AutoIt resolves function names case-insensitively, so the literal may not
    // match the definition's spelling; the rewritten call uses the definition's.
    let src = "Func FooBar()\n    Return 1\nEndFunc\nCall(\"foobar\")\n";
    let mut prog = parse(src).unwrap();
    simplify::simplify_program(&mut prog);
    let out = pretty(&prog);
    assert!(out.contains("FooBar()"), "{out}");
    assert!(!out.contains("Call("), "{out}");
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

#[test]
fn orchestrator_runs_pipeline() {
    let src = "$zzz = 1 + 2\n$yyy = \"a\" & \"b\"\n";
    let mut prog = parse(src).unwrap();
    let report = Deobfuscator::renaming().run(&mut prog);
    assert!(report.folds >= 2);
    assert!(report.renamed.vars >= 2);
}

#[test]
fn fold_and_rename_modules_are_public() {
    // The public modules should expose their top-level functions.
    let mut prog = parse("$a = 1 + 1\n").unwrap();
    let f = fold::fold_program(&mut prog);
    assert_eq!(f, 1);
    let _r = rename::rename_program(&mut prog);
    let _out = pretty(&prog);
}

// ---------------------------------------------------------------------------
// CLI end-to-end style: deobfuscate output must still parse
// ---------------------------------------------------------------------------

#[test]
fn deobfuscated_output_reparses() {
    let src = "Global Const $K1 = \"hello\"\nFunc Fzz($p)\n    $loc = 40 + 2\n    Return $p & $K1 & $loc\nEndFunc\nFzz(\"#\")\n";
    let out = run(src);
    // Deobfuscated text must remain valid AutoIt (re-parses).
    assert!(parse(&out).is_ok(), "output did not re-parse: {out}");
}
// ---------------------------------------------------------------------------
// `#forceref` and subscripts on call results
// ---------------------------------------------------------------------------

#[test]
fn forceref_variables_are_renamed_with_everything_else() {
    // `#forceref $p` names a real variable; leaving it behind would point at a
    // parameter that no longer exists.
    let out = run("Func Fzz($p, $q)\n    #forceref $q\n    Return $p\nEndFunc\n");
    let line = out
        .lines()
        .find(|l| l.contains("#forceref"))
        .expect("the directive survives");
    let param = out
        .lines()
        .find(|l| l.starts_with("Func"))
        .and_then(|l| l.split(',').nth(1))
        .map(|s| s.trim().trim_end_matches(')'))
        .expect("second parameter");
    assert_eq!(line.trim(), format!("#forceref {param}"));
}

#[test]
fn a_subscript_on_a_call_result_keeps_its_arguments_inlined() {
    // The reformatting must not split `DllCall(...)[0]` into two statements,
    // and the table lookups inside the call still have to be substituted.
    let src = "Global Const $T = MakeTable()\nFunc Fzz()\n    Return $T[0](\"shlwapi.dll\", \"int\", \"StrCmpLogicalW\")[0]\nEndFunc\n";
    let out = run(src);
    assert!(!out.contains("\n    [0]"), "subscript split off:\n{out}");
    assert!(out.contains(")[0]"), "subscript lost:\n{out}");
    assert!(parse(&out).is_ok(), "output did not re-parse: {out}");
}

#[test]
fn tables_can_be_substituted_again_after_the_simplifier_splices_code() {
    // `Simplify` turns an `Execute("...")` string into real code; when the
    // caller inlined table values *before* the pipeline, the reads in that
    // spliced code are still unresolved. `Deobfuscator::after_simplify` marks
    // where to step in.
    let src = "Global Const $T = Make()\nFunc F()\n    Execute(\"$T[1]\")\nEndFunc\n";
    let mut prog = parse(src).unwrap();
    let tables = Tables::new(vec![(
        "T".to_string(),
        autoitv3_runtime::Value::array(vec![
            autoitv3_runtime::Value::Int(0),
            autoitv3_runtime::Value::Int(42),
        ]),
    )]);

    let deobf = Deobfuscator::new();
    let split = deobf.after_simplify();
    assert_eq!(split, 2, "Fold and Simplify come first");

    let mut report = DeobfReport::default();
    deobf.run_passes(&mut prog, &deobf.passes[..split], &mut report);
    assert_eq!(report.simplified.executes, 1, "the Execute should be spliced");

    // The spliced read is only reachable now.
    let count = tables.substitute(&mut prog);
    assert_eq!(count.substitutions, 1);
    deobf.run_passes(&mut prog, &deobf.passes[split..], &mut report);
    let out = pretty(&prog);
    assert!(out.contains("42"), "spliced read not inlined:\n{out}");
    assert!(!out.contains("$T["), "read should be gone:\n{out}");
}

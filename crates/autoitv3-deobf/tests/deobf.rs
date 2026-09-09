//! Unit tests for the autoitv3-deobf passes.

use autoitv3_ast::ast::{ExprKind, ItemKind, LitKind, StmtKind};
use autoitv3_ast::parse;
use autoitv3_format::PrettyPrinter;
use autoitv3_deobf::{deobfuscate, rename, fold};

fn pretty(prog: &autoitv3_ast::Program) -> String {
    let mut pp = PrettyPrinter::new();
    pp.print_program(prog)
}

/// Deobfuscate a source snippet (parse, fold+rename, print) and return text.
fn run(src: &str) -> String {
    let mut prog = parse(src).unwrap();
    deobfuscate(&mut prog);
    pretty(&prog)
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
    assert!(a.contains("$v000"), "expected stable alias: {a}");
}

#[test]
fn rename_functions_and_calls_consistently() {
    let src = "Func Xobfu()\n    Return 1\nEndFunc\nXobfu()\n";
    let out = run(src);
    // Function definition and its call site must share the same alias.
    assert!(out.contains("Func f000"), "got: {out}");
    assert!(out.contains("f000()"), "got: {out}");
    assert!(!out.contains("Xobfu"), "got: {out}");
}

#[test]
fn rename_preserves_behavior_through_pretty() {
    // Renaming must round-trip: the re-parsed output keeps the same structure.
    let src = "Global Const $A = \"hi\"\nFunc F1($p)\n    Return $p & $A\nEndFunc\nF1(\"x\")\n";
    let mut prog = parse(src).unwrap();
    let before = pretty(&prog);
    deobfuscate(&mut prog);
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

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

#[test]
fn orchestrator_runs_pipeline() {
    let src = "$zzz = 1 + 2\n$yyy = \"a\" & \"b\"\n";
    let mut prog = parse(src).unwrap();
    let report = deobfuscate(&mut prog);
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
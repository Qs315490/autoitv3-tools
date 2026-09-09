//! Unit tests for the autoitv3-format pretty-printer crate.

use autoitv3_ast::ast::ItemKind;
use autoitv3_ast::parse;
use autoitv3_format::PrettyPrinter;

#[test]
fn pretty_roundtrip_preserves_items() {
    let src = "#NoTrayIcon\nGlobal Const $A = 1\nFunc Add($x, $y = 2)\n    Return $x + $y\nEndFunc\nAdd(1)\n";
    let prog = parse(src).unwrap();
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);
    // The printed output must parse back with the same item/function counts.
    let reparsed = parse(&out).unwrap();
    assert_eq!(reparsed.items.len(), prog.items.len());
    let count = |p: &autoitv3_ast::Program| {
        p.items
            .iter()
            .filter(|it| matches!(it.kind, ItemKind::Func(_)))
            .count()
    };
    assert_eq!(count(&reparsed), count(&prog));
}

#[test]
fn pretty_strips_comments_when_requested() {
    let src = "Func A() ; this is a comment\n    Return 1 ; trailing\nEndFunc\n";
    let prog = parse(src).unwrap();
    let mut pp = PrettyPrinter::new().strip_comments(true);
    let out = pp.print_program(&prog);
    assert!(!out.contains("this is a comment"));
    assert!(!out.contains("trailing"));
}

#[test]
fn pretty_preserves_comments_by_default() {
    let src = "Func A() ; this is a comment\n    Return 1 ; trailing\nEndFunc\n";
    let prog = parse(src).unwrap();
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);
    assert!(out.contains("this is a comment"));
    assert!(out.contains("trailing"));
}

// ---------------------------------------------------------------------------
// Regression: comments after EndFunc (from the real obfuscated target sample.au3)
// ---------------------------------------------------------------------------
// The obfuscated target uses marker comments like `;==>MARKER` trailing
// after `EndFunc`. These must parse and be preserved (or stripped when asked)
// without tripping the parser into "expected expression".

#[test]
fn comment_after_endfunc_parses_and_is_preserved_by_default() {
    let src = "Func F($x)\n    Return $x\nEndFunc   ;==>MARKER\r\n";
    let prog = parse(src).expect("must parse");
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);
    // The marker comment survives in the default (preserve) output.
    assert!(out.contains(";==>MARKER"), "comment lost: {out}");
    // And the output re-parses cleanly.
    parse(&out).expect("re-parsed output must parse");
}

#[test]
fn comment_after_endfunc_is_stripped_when_requested() {
    let src = "Func F($x)\n    Return $x\nEndFunc   ;==>MARKER\r\n";
    let prog = parse(src).expect("must parse");
    let mut pp = PrettyPrinter::new().strip_comments(true);
    let out = pp.print_program(&prog);
    assert!(!out.contains("MARKER"), "comment not stripped: {out}");
    // And the output re-parses cleanly.
    parse(&out).expect("re-parsed output must parse");
}

// The full real target must still parse (regression for the stale-binary bug).
#[test]
fn full_obfuscated_target_parses_and_formats() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../../sample.au3");
    if !std::path::Path::new(path).exists() {
        eprintln!("skipping: {path} not found");
        return;
    }
    let src = std::fs::read_to_string(path).unwrap();
    let prog = parse(&src).expect("whole obfuscated file must parse");
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);
    assert!(out.contains(";==>"), "expected marker comments preserved");
    parse(&out).expect("formatted obfuscated file must re-parse");
}

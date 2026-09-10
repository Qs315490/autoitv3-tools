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

#[test]
fn empty_array_brackets_are_not_printed_as_null() {
    // `Local $a[] = [...]` uses "empty brackets, size from the initializer".
    // The parser records that as a `Null` placeholder dimension, which must
    // print as `[]` — printing `[Null]` silently changes the declaration.
    let src = "Local $a[] = [1, 2, 3]\nGlobal $b[4]\nReDim $b[6]\n";
    let prog = parse(src).unwrap();
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);

    assert!(out.contains("Local $a[] = [1, 2, 3]"), "out: {out}");
    assert!(!out.contains("Null"), "empty brackets leaked as Null: {out}");
    assert!(out.contains("Global $b[4]"), "out: {out}");
    assert!(out.contains("ReDim $b[6]"), "out: {out}");

    // The rendering must still be valid, re-parseable AutoIt.
    let reparsed = parse(&out).unwrap();
    assert_eq!(reparsed.items.len(), prog.items.len());
}

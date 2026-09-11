//! Unit tests for the autoitv3-format pretty-printer crate.

use autoitv3_ast::ast::{ItemKind, StmtKind};
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
fn functions_without_parameters_keep_their_parentheses() {
    // AutoIt requires `Func Foo()`; `Func Foo` is not valid AutoIt, so the
    // empty parameter list must be printed even when there is nothing in it.
    let src = concat!(
        "Func NoArgs()\n",
        "    Return 1\n",
        "EndFunc\n",
        "Volatile Func AlsoNone()\n",
        "    Return 2\n",
        "EndFunc\n",
    );
    let prog = parse(src).unwrap();
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);

    assert!(out.contains("Func NoArgs()"), "out: {out}");
    assert!(out.contains("Volatile Func AlsoNone()"), "out: {out}");
    // The bare form must not survive anywhere.
    assert!(!out.contains("Func NoArgs\n"), "out: {out}");
    assert!(!out.contains("Func AlsoNone\n"), "out: {out}");
}

#[test]
fn else_branches_keep_their_keyword() {
    // Without the `Else` keyword the else body is printed inside the `Then`
    // branch: the output still re-parses, it just means something else.
    let src = concat!(
        "If $a Then\n",
        "    $x = 1\n",
        "ElseIf $b Then\n",
        "    $x = 2\n",
        "Else\n",
        "    $x = 3\n",
        "EndIf\n",
    );
    let prog = parse(src).unwrap();
    let mut pp = PrettyPrinter::new();
    let out = pp.print_program(&prog);

    assert!(out.contains("ElseIf $b Then"), "out: {out}");
    assert!(out.contains("Else\n"), "out: {out}");

    // The re-parsed tree must still have one `ElseIf` and one `Else`, each
    // with its own body.
    let reparsed = parse(&out).unwrap();
    let ItemKind::Stmt(stmt) = &reparsed.items[0].kind else {
        panic!("expected a statement, got: {out}");
    };
    let StmtKind::If(if_) = &stmt.kind else {
        panic!("expected an If, got: {out}");
    };
    assert_eq!(if_.then_block.len(), 1, "out: {out}");
    assert_eq!(if_.else_ifs.len(), 1, "out: {out}");
    assert_eq!(if_.else_ifs[0].1.len(), 1, "out: {out}");
    assert_eq!(if_.else_block.len(), 1, "out: {out}");
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

#[test]
fn a_long_array_literal_wraps_with_continuations() {
    // A deobfuscated table can run to thousands of entries; wrapping keeps it
    // readable without breaking AutoIt's syntax.
    let items: Vec<String> = (0..40).map(|i| i.to_string()).collect();
    let src = format!("Local $a = [{}]\n", items.join(", "));
    let prog = autoitv3_ast::parse(&src).expect("parses");
    let mut pp = autoitv3_format::PrettyPrinter::new();
    let out = pp.print_program(&prog);
    assert!(out.contains(" _"), "no continuation added: {out}");
    // The wrapped form is still valid AutoIt, and means the same thing.
    let reparsed = autoitv3_ast::parse(&out).expect("wrapped output re-parses");
    assert_eq!(reparsed.items.len(), prog.items.len());
}

#[test]
fn a_short_array_literal_stays_on_one_line() {
    let prog = autoitv3_ast::parse("Local $a = [1, 2, 3]\n").expect("parses");
    let mut pp = autoitv3_format::PrettyPrinter::new();
    assert_eq!(pp.print_program(&prog), "Local $a = [1, 2, 3]\n");
}

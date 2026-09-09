//! Unit tests for the function-table resolution pass (autoitv3-deobf).

use autoitv3_ast::parse;
use autoitv3_deobf::table;

fn run(src: &str) -> (String, table::TableReport) {
    let mut prog = parse(src).unwrap();
    let rep = table::resolve_function_table(&mut prog, "fn_table", "BuildFunctionTable");
    let mut pp = autoitv3_format::PrettyPrinter::new();
    (pp.print_program(&prog), rep)
}

#[test]
fn table_resolves_minimal_builder() {
    let src = "Func BuildFunctionTable()\n    Local $x[] = [0x2, Foo, Bar]\n    Local $y[] = [0x1, Baz]\n    MergeArrays($x, $y)\n    Return $x\nEndFunc\nGlobal Const $fn_table = BuildFunctionTable()\n$fn_table[0x1]()\n$x = $fn_table[0x2] + $fn_table[0x3]\n";
    let (out, rep) = run(src);
    // Element 0 is the count; entries are Foo, Bar, Baz -> 3.
    assert_eq!(rep.entries, 3);
    assert_eq!(rep.calls_rewritten, 1);
    assert_eq!(rep.refs_rewritten, 2);
    // Indexed call rewritten to a real function name.
    assert!(out.contains("Foo()"), "out: {out}");
    // Indexed references rewritten to identifiers.
    assert!(out.contains("Bar"), "out: {out}");
    assert!(out.contains("Baz"), "out: {out}");
    assert!(!out.contains("$fn_table[1]"), "out: {out}");
    // Output must still parse.
    assert!(parse(&out).is_ok(), "did not reparse: {out}");
}

#[test]
fn table_resolves_full_builder_from_target() {
    // The real sample.au3 builder is pure array construction -> 1108 entries.
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../sample.au3")).unwrap();
    let mut prog = parse(&src).unwrap();
    let rep = table::resolve_function_table(&mut prog, "fn_table", "BuildFunctionTable");
    assert_eq!(rep.entries, 1108, "function table should have 1108 entries");
    // Most of the several thousand obfuscated references must have been rewritten.
    assert!(
        rep.calls_rewritten + rep.refs_rewritten > 1000,
        "got calls={} refs={}",
        rep.calls_rewritten,
        rep.refs_rewritten
    );
}
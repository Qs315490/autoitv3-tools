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
    // The builder is *executed* by autoitv3-runtime, so the `MergeArrays`
    // helper it calls must be present (as a real script's would be).
    let src = "Func MergeArrays(ByRef $t, Const ByRef $s)\n    ReDim $t[$t[0] + $s[0] + 1]\n    Local $i\n    For $i = 1 To $s[0]\n        $t[$t[0] + $i] = $s[$i]\n    Next\n    $t[0] += $s[0]\nEndFunc\nFunc BuildFunctionTable()\n    Local $x[] = [0x2, Foo, Bar]\n    Local $y[] = [0x1, Baz]\n    MergeArrays($x, $y)\n    Return $x\nEndFunc\nGlobal Const $fn_table = BuildFunctionTable()\n$fn_table[0x1]()\n$x = $fn_table[0x2] + $fn_table[0x3]\n";
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
fn table_resolves_a_real_script() {
    // A real obfuscated script hides its table behind a name the tool cannot
    // know in advance, so the pass has to detect it.
    let Some(src) = sample_script() else { return };
    let mut prog = parse(&src).unwrap();
    let rep = table::resolve_function_table_with(&mut prog, &table::TableOptions::default());
    assert!(
        rep.entries > 100,
        "expected a large table, got {} entries",
        rep.entries
    );
    assert!(
        rep.calls_rewritten + rep.refs_rewritten > 100,
        "got calls={} refs={}",
        rep.calls_rewritten,
        rep.refs_rewritten
    );
}

/// Read the optional obfuscated sample script used by the integration checks.
///
/// Point the `AU3_SAMPLE` environment variable at a real obfuscated AutoIt
/// script to enable them; they are skipped when it is unset or unreadable, so
/// `cargo test` stays green without any external fixture.
fn sample_script() -> Option<String> {
    let path = std::env::var("AU3_SAMPLE").ok().filter(|p| !p.is_empty())?;
    match std::fs::read_to_string(&path) {
        Ok(src) => Some(src),
        Err(e) => {
            eprintln!("skipping: cannot read {path}: {e}");
            None
        }
    }
}

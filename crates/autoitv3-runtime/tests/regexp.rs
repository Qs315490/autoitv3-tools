//! Tests for `StringRegExp` / `StringRegExpReplace`.
//!
//! AutoIt uses PCRE; this crate implements the same surface with the pure-Rust
//! `regex` engine (see `autoitv3_runtime::regexp`), so these tests double as the
//! specification of the subset that is supported and of how the unsupported
//! PCRE-only features are reported.

use autoitv3_runtime::regexp;
use autoitv3_runtime::{Runtime, Value};

/// Run `Func F()` from `body` and return its result.
fn call(body: &str) -> Value {
    let src = format!("Func F()\n{body}\nEndFunc\n");
    let prog = autoitv3_ast::parse(&src).expect("test source parses");
    let mut rt = Runtime::with_program(&prog);
    rt.call_function("F", vec![]).expect("no runtime error")
}

/// Run `Func F()` and return the result rendered as AutoIt text.
fn text(body: &str) -> String {
    call(body).to_autoit_string()
}

/// Collect an array result into strings.
fn array(body: &str) -> Vec<String> {
    match call(body) {
        Value::Array(a) => a.borrow().iter().map(|v| v.to_autoit_string()).collect(),
        other => panic!("expected an array, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// StringRegExp — the five flag modes
// ---------------------------------------------------------------------------

#[test]
fn flag_0_is_a_boolean_match() {
    assert_eq!(text(r#"Return StringRegExp("hello123", "\d+")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("hello", "\d+")"#), "0");
}

#[test]
fn flag_1_returns_captured_groups() {
    let a = array(r#"Return StringRegExp("2024-08-23", "(\d+)-(\d+)-(\d+)", 1)"#);
    assert_eq!(a, vec!["2024", "08", "23"]);
}

#[test]
fn flag_1_falls_back_to_the_match_when_there_are_no_groups() {
    let a = array(r#"Return StringRegExp("ab12", "\d+", 1)"#);
    assert_eq!(a, vec!["12"]);
}

#[test]
fn flag_2_puts_the_full_match_first() {
    let a = array(r#"Return StringRegExp("ab12", "([a-z]+)(\d+)", 2)"#);
    assert_eq!(a, vec!["ab12", "ab", "12"]);
}

#[test]
fn flag_3_returns_every_match() {
    let a = array(r#"Return StringRegExp("a1b2c3", "\d", 3)"#);
    assert_eq!(a, vec!["1", "2", "3"]);
}

#[test]
fn flag_4_returns_an_array_per_match() {
    let outer = call(r#"Return StringRegExp("a1b2", "([a-z])(\d)", 4)"#);
    let Value::Array(outer) = outer else { panic!("expected array") };
    let outer = outer.borrow();
    assert_eq!(outer.len(), 2);
    let inner: Vec<String> = match &outer[0] {
        Value::Array(a) => a.borrow().iter().map(|v| v.to_autoit_string()).collect(),
        other => panic!("expected nested array, got {other:?}"),
    };
    assert_eq!(inner, vec!["a1", "a", "1"]);
}

#[test]
fn unknown_flag_is_an_error() {
    let src = "Func F()\n    Return StringRegExp(\"x\", \"x\", 9)\nEndFunc\n";
    let prog = autoitv3_ast::parse(src).unwrap();
    let err = Runtime::with_program(&prog)
        .call_function("F", vec![])
        .unwrap_err();
    assert!(err.message().contains("flag 9"), "got: {}", err.message());
}

// ---------------------------------------------------------------------------
// StringRegExp — @error / @extended / offset
// ---------------------------------------------------------------------------

#[test]
fn bad_pattern_reports_error_and_offset() {
    // `@error = 2` means "bad pattern"; `@extended` carries the offset.
    let out = text(r#"Local $r = StringRegExp("x", "([a-z")
    Return @error"#);
    assert_eq!(out, "2");
}

#[test]
fn no_match_sets_error_1_for_array_flags() {
    assert_eq!(text(r#"StringRegExp("hello", "\d+", 1)
    Return @error"#), "1");
    assert_eq!(text(r#"StringRegExp("hello", "\d+", 3)
    Return @error"#), "1");
}

#[test]
fn match_leaves_error_clear_and_reports_next_offset() {
    // For flags 1/2, `@extended` is the position after the match (1-based).
    assert_eq!(text(r#"StringRegExp("ab12cd", "\d+", 1)
    Return @error"#), "0");
    assert_eq!(text(r#"StringRegExp("ab12cd", "\d+", 1)
    Return @extended"#), "5");
}

#[test]
fn offset_parameter_starts_the_search_later() {
    // `offset` is the position the search *starts* at (1-based), not a position
    // the match is pinned to: from 1 the digit is found, from 2 it is skipped.
    assert_eq!(text(r#"Return StringRegExp("1aa", "\d", 0, 1)"#), "1");
    assert_eq!(text(r#"Return StringRegExp("1aa", "\d", 0, 2)"#), "0");
    // And a later offset still finds a later match.
    assert_eq!(text(r#"Return StringRegExp("aa123", "\d", 0, 3)"#), "1");
}

#[test]
fn offset_past_the_end_is_a_clean_no_match() {
    assert_eq!(text(r#"StringRegExp("aa", "\d", 1, 99)
    Return @error"#), "1");
}

// ---------------------------------------------------------------------------
// StringRegExpReplace
// ---------------------------------------------------------------------------

#[test]
fn replace_without_backreferences() {
    assert_eq!(
        text(r#"Return StringRegExpReplace("Where have all", "[aeiou]", "@")"#),
        "Wh@r@ h@v@ @ll"
    );
}

#[test]
fn replace_supports_dollar_backreferences() {
    assert_eq!(
        text(
            r#"Return StringRegExpReplace("12/31/2009", "(\d{2})/(\d{2})/(\d{4})", "$2.$1.$3")"#
        ),
        "31.12.2009"
    );
}

#[test]
fn replace_supports_backslash_backreferences() {
    assert_eq!(
        text(r#"Return StringRegExpReplace("ab", "(a)(b)", "\2\1")"#),
        "ba"
    );
}

#[test]
fn replace_supports_braced_backreferences() {
    // `${1}5` separates the reference from a following digit.
    assert_eq!(
        text(r#"Return StringRegExpReplace("a", "(a)", "${1}5")"#),
        "a5"
    );
}

#[test]
fn replace_requires_doubled_backslash_for_a_literal_one() {
    // AutoIt: a literal `\` in the replacement must be written `\\`.
    assert_eq!(
        text(r#"Return StringLen(StringRegExpReplace("%X%", "%([^%]*?)%", "C:\\dir"))"#),
        "6"
    );
}

#[test]
fn replace_count_limits_the_number_of_replacements() {
    assert_eq!(text(r#"Return StringRegExpReplace("aaa", "a", "b", 1)"#), "baa");
    assert_eq!(text(r#"Return StringRegExpReplace("aaa", "a", "b", 0)"#), "bbb");
}

#[test]
fn replace_reports_the_number_of_replacements_in_extended() {
    assert_eq!(
        text(r#"StringRegExpReplace("aaa", "a", "b")
    Return @extended"#),
        "3"
    );
}

// ---------------------------------------------------------------------------
// Pattern feature coverage
// ---------------------------------------------------------------------------

#[test]
fn pcre_global_settings_are_stripped() {
    // AutoIt allows `(*UCP)`, newline conventions and friends at the head.
    assert_eq!(text(r#"Return StringRegExp("Hello", "(*UCP)(?i)hello")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("a", "(*ANYCRLF)a")"#), "1");
}

#[test]
fn inline_option_groups_work() {
    assert_eq!(text(r#"Return StringRegExp("ABC", "(?i)abc")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("a" & @LF & "b", "(?m)^b")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("a" & @LF & "b", "a.b")"#), "0");
    assert_eq!(text(r#"Return StringRegExp("a" & @LF & "b", "(?s)a.b")"#), "1");
}

#[test]
fn posix_classes_and_named_groups_work() {
    assert_eq!(array(r#"Return StringRegExp("ab12", "[[:alpha:]]+", 3)"#), vec!["ab"]);
    let a = array(r#"Return StringRegExp("2024-08", "(?<y>\d+)-(?<m>\d+)", 1)"#);
    assert_eq!(a, vec!["2024", "08"]);
}

#[test]
fn lazy_quantifiers_work() {
    assert_eq!(array(r#"Return StringRegExp("<a><b>", "<(.+?)>", 3)"#), vec!["<a>", "<b>"]);
}

#[test]
fn unsupported_pcre_features_are_reported_as_a_bad_pattern() {
    // The Rust engine is a finite-automaton one: lookaround and backreferences
    // are not available. They must fail loudly (`@error = 2`) rather than being
    // approximated.
    for pattern in [r"(?=x)", r"(?<=x)y", r"(a)\1", r"(?>ab)"] {
        let body = format!(
            "    StringRegExp(\"x\", \"{pattern}\")\n    Return @error"
        );
        assert_eq!(text(&body), "2", "pattern {pattern} should be a bad pattern");
    }
}

// ---------------------------------------------------------------------------
// Unit tests for the helpers
// ---------------------------------------------------------------------------

#[test]
fn replacement_translation_maps_onto_the_engine_syntax() {
    assert_eq!(regexp::translate_replacement(r"$1.$2"), "$1.$2");
    assert_eq!(regexp::translate_replacement(r"\1\2"), "${1}${2}");
    assert_eq!(regexp::translate_replacement(r"a\\b"), r"a\b");
    assert_eq!(regexp::translate_replacement("${1}5"), "${1}5");
}

#[test]
fn prologue_stripping_keeps_the_body() {
    let re = regexp::compile("(*UCP)(?i)hello").expect("compiles");
    assert!(re.is_match("HELLO"));
}

#[test]
fn char_offsets_are_one_based_and_utf8_safe() {
    // `é` is two bytes; the offset must be counted in characters, not bytes.
    let s = "aé1";
    let byte = regexp::char_offset_to_byte(s, 3).expect("in range");
    assert_eq!(&s[byte..], "1");
    assert_eq!(regexp::char_offset_to_byte(s, 99), None);
    assert_eq!(regexp::byte_to_char_offset(s, 3), 3);
}
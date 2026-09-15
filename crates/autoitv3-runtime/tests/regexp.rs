//! Tests for `StringRegExp` / `StringRegExpReplace`.
//!
//! AutoIt uses PCRE; this crate implements the same surface with the pure-Rust
//! `fancy-regex` engine (see `autoitv3_runtime::regexp`), so these tests double
//! as the specification of the supported subset. `fancy-regex` keeps the
//! linear-time `regex` syntax for ordinary patterns and adds a backtracking VM
//! for the PCRE features scripts use: lookaround, backreferences, atomic groups
//! and conditionals.

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

/// Render `s` as an AutoIt string literal (an embedded `"` is doubled).
///
/// Patterns and subjects with quotes are awkward to write inline, so the
/// lookbehind tests below build the call from Rust values through this.
fn autoit_literal(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Run `StringRegExp(subject, pattern, flag)` and summarise the result the way
/// the deobfuscator's probe wrapper does: `"ERR"` when the call set `@error`,
/// otherwise `"N:<ubound>"`.
fn re(subject: &str, pattern: &str, flag: i64) -> String {
    let body = format!(
        "    Local $m = StringRegExp({}, {}, {flag})\n    If @error Then Return \"ERR\"\n    Return \"N:\" & UBound($m, 1)",
        autoit_literal(subject),
        autoit_literal(pattern),
    );
    text(&body)
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
fn extended_is_absolute_even_when_the_search_starts_later() {
    // The position is measured from the start of the subject, not from
    // `offset`: a relative answer would send a caller that feeds `@extended`
    // back as the next offset backwards, into an endless loop.
    assert_eq!(text(r#"StringRegExp("ab12cd", "\d+", 1, 3)
    Return @extended"#), "5");
}

#[test]
fn a_scan_that_feeds_extended_back_advances() {
    let out = text(
        r#"
Local $s = "1a2b3c"
Local $off = 1, $n = 0
While 1
    StringRegExp($s, "\d", 1, $off)
    If @error Then ExitLoop
    $n += 1
    Local $next = @extended
    If $next <= $off Then ExitLoop
    $off = $next
WEnd
Return $n"#,
    );
    assert_eq!(out, "3");
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
fn backtracking_features_are_available() {
    // `fancy-regex` adds a backtracking VM for the PCRE constructs scripts
    // actually use: lookaround, backreferences and atomic groups.
    assert_eq!(text(r#"Return StringRegExp("x", "(?=x)")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("xy", "(?<=x)y")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("aa", "(a)\1")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("ab", "(?>ab)")"#), "1");
    // A pattern that really is malformed is still a bad pattern.
    assert_eq!(
        text("    StringRegExp(\"x\", \"([a-z\")\n    Return @error"),
        "2"
    );
}

// ---------------------------------------------------------------------------
// Backtracking features
// ---------------------------------------------------------------------------

#[test]
fn native_lookbehind_handles_the_tokenizer_scan() {
    // `(?<!\\)"` is the obfuscator's "unescaped quote" scan. The backtracking
    // engine supports the lookbehind natively, so it consumes nothing before
    // the quote: all four quotes match, and alternations containing it compile.
    let subject = r#"{"Form": null, "Ctrls": {}}"#;
    assert_eq!(re(subject, r#"(?<!\\)""#, 3), "N:4");
    assert_eq!(re(subject, r#"(?<!\\)"|\{|\["#, 3), "N:6");
}

#[test]
fn tokenizer_variable_names_keep_their_mode_semantics() {
    // The generated names are `$STR` + six letters; the flags used to locate
    // them behave as elsewhere in this file.
    assert_eq!(re("$STRABCXYZ: null", r"\s*(\$[A-Z]{3}[A-Z]{6})\s*:", 1), "N:1");
    assert_eq!(re("$STRABCXYZ", r"\$[A-Z]{3}[A-Z]{6}", 3), "N:1");
    assert_eq!(re("STRABCXYZ", r"STR[A-Z]{6}", 1), "N:1");
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
    assert!(re.is_match("HELLO").expect("no engine error"));
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
// ---------------------------------------------------------------------------
// PCRE constructs the engine spells differently
// ---------------------------------------------------------------------------

#[test]
fn assertion_conditionals_are_expanded() {
    // `fancy-regex` implements the group conditional `(?(1)yes|no)` but not the
    // assertion form, which AutoIt patterns use to branch on a lookaround.
    assert_eq!(text(r#"Return StringRegExp("b", "^(?(?=a)a|b)$")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("a", "^(?(?=a)a|b)$")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("c", "^(?(?=a)a|b)$")"#), "0");
    // Without an else branch the pattern only matches when the condition holds.
    assert_eq!(text(r#"Return StringRegExp("a", "^(?(?=a)a)$")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("b", "^(?(?=a)a)$")"#), "0");
    // Negated and lookbehind conditions, and a quantified conditional. A
    // negative condition is a plain assertion too: when it holds, only the
    // "yes" branch is tried, so `^(?(?!a)a|b)$` never matches either subject.
    assert_eq!(text(r#"Return StringRegExp("a", "^(?(?!x)a|b)$")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("b", "^(?(?!a)a|b)$")"#), "0");
    assert_eq!(text(r#"Return StringRegExp("xa", "(?(?<=x)a|b)")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("aab", "^(?:(?(?=a)a|b))+$")"#), "1");
    // A conditional inside a capture group keeps the group numbering.
    assert_eq!(text(r#"Return StringRegExp("b", "^(x)?(?(?=b)b|c)$")"#), "1");
}

#[test]
fn character_class_escapes_keep_the_pcre_meaning() {
    // `fancy-regex` follows Oniguruma for `\h`/`\H`, where they mean "hex
    // digit"; PCRE — and AutoIt — mean horizontal white space. Getting this
    // wrong is silent: the pattern still compiles, it just consumes one
    // character too many.
    assert_eq!(text(r#"Return StringRegExp(" ", "\h")"#), "1");
    assert_eq!(text(r#"Return StringRegExp(@TAB, "\h")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("C", "\h")"#), "0");
    assert_eq!(text(r#"Return StringRegExp(ChrW(160), "\h")"#), "1");
    assert_eq!(text(r#"Return StringRegExp("x", "\H")"#), "1");
    assert_eq!(text(r#"Return StringRegExp(" ", "\H")"#), "0");
    // `\h` inside a class means the same thing.
    assert_eq!(text(r#"Return StringRegExp(" ", "[\h\d]")"#), "1");
    // `\v` is any vertical white space in PCRE, not just the C escape.
    assert_eq!(text(r#"Return StringRegExp(@LF, "\v")"#), "1");
    assert_eq!(text(r#"Return StringRegExp(Chr(11), "\v")"#), "1");
    // An escaped backslash before an `h` is left alone: `\\h` is a literal
    // backslash followed by a literal `h`.
    assert_eq!(text(r#"Return StringRegExp("\h", "\\h")"#), "1");
}

#[test]
fn the_drive_letter_splitter_keeps_its_groups() {
    // The drive-letter splitter AutoIt builds use to split a path into drive / dir /
    // file name / extension. `\h` must not eat the drive letter: `C` is a hex
    // digit, and the engine would otherwise consume it before the group opens.
    let pattern = r"^\h*((?:\\\\\?\\)*(\\[^\?\/\\]+|[A-Za-z]:)?(.*[\/\\]\h*)?((?:[^\.\/\\]|(?(?=\.[^\/\\]*\.)\.))*)?([^\/\\]*))$";
    let body = format!(
        "    Local $a = StringRegExp({}, {}, 1)\n    Return UBound($a) & \":\" & $a[1] & \":\" & $a[2] & \":\" & $a[3] & \":\" & $a[4]",
        autoit_literal(r"C:\Users\user\Desktop\build\app.exe"),
        autoit_literal(pattern),
    );
    assert_eq!(
        text(&body),
        r"5:C::\Users\user\Desktop\build\:app:.exe"
    );
}

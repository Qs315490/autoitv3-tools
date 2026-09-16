//! Unit tests for the debug shell's tab completion.
//!
//! Kept out of `debug.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it reach
//! the private completer and word-splitting helper.

use super::*;

fn replacements(pairs: Vec<Pair>) -> Vec<String> {
    pairs.into_iter().map(|p| p.replacement).collect()
}

#[test]
fn the_first_word_completes_commands() {
    let data = CompletionData {
        commands: vec!["break".into(), "continue".into(), "print".into()],
        ..CompletionData::default()
    };
    assert_eq!(
        replacements(data.candidates(None, "br")),
        vec!["break".to_string()]
    );
    assert_eq!(
        replacements(data.candidates(None, "")),
        vec!["break".to_string(), "continue".to_string(), "print".to_string()]
    );
}

#[test]
fn untilret_completes_functions_and_builtins() {
    let data = CompletionData {
        builtins: vec!["MsgBox".into()],
        functions: vec!["Main".into()],
        ..CompletionData::default()
    };
    assert_eq!(
        replacements(data.candidates(Some("untilret"), "")),
        vec!["Main".to_string(), "MsgBox".to_string()]
    );
}

#[test]
fn stopat_completes_functions_and_builtins() {
    let data = CompletionData {
        builtins: vec!["MsgBox".into(), "Sleep".into()],
        functions: vec!["Main".into()],
        ..CompletionData::default()
    };
    assert_eq!(
        replacements(data.candidates(Some("stopat"), "Ms")),
        vec!["MsgBox".to_string()]
    );
    // The short alias works the same way, and functions are offered too.
    assert_eq!(
        replacements(data.candidates(Some("sa"), "")),
        vec!["Main".to_string(), "MsgBox".to_string(), "Sleep".to_string()]
    );
}

#[test]
fn arguments_complete_from_the_command_word() {
    let data = CompletionData {
        functions: vec!["Main".into()],
        builtins: vec!["GUICreate".into(), "String".into()],
        globals: vec!["$x".into()],
        macros: vec!["@YEAR".into()],
        ..CompletionData::default()
    };
    // `break` takes a line or a function.
    assert_eq!(
        replacements(data.candidates(Some("break"), "M")),
        vec!["Main".to_string()]
    );
    // `untilcall` accepts builtins too.
    assert_eq!(
        replacements(data.candidates(Some("untilcall"), "GUI")),
        vec!["GUICreate".to_string()]
    );
    // …and so do the short aliases.
    assert_eq!(
        replacements(data.candidates(Some("uc"), "GUI")),
        vec!["GUICreate".to_string()]
    );
    assert_eq!(
        replacements(data.candidates(Some("ur"), "M")),
        vec!["Main".to_string()]
    );
    // `print`/`set`/`eval` offer variables and macros.
    assert_eq!(
        replacements(data.candidates(Some("print"), "$")),
        vec!["$x".to_string()]
    );
    assert_eq!(
        replacements(data.candidates(Some("print"), "@")),
        vec!["@YEAR".to_string()]
    );
    // An argument-taking command with nothing to offer completes nothing.
    assert!(data.candidates(Some("continue"), "x").is_empty());
}

#[test]
fn the_word_and_command_are_split_off_the_line() {
    assert_eq!(
        current_word("break Ma", 8),
        (6, "Ma", Some("break".to_string()))
    );
    assert_eq!(current_word("bre", 3), (0, "bre", None));
    assert_eq!(current_word("print $x ", 9), (9, "", Some("print".to_string())));
}

//! Unit tests for `vocab`'s function and macro tables.
//!
//! Kept out of `vocab.rs` so the module reads as implementation;
//! `#[path]` pulls the file back in as a unit-test module, which is what
//! lets it reach private state the module does not expose.

use super::*;

#[test]
fn ids_are_positions_so_the_order_is_the_contract() {
    // `Abs` is function 0 and `ACos` function 1 — the numbers a compiled
    // script carries. `extended` is macro 0, not `@error`.
    assert_eq!(FUNCTIONS[0], "Abs");
    assert_eq!(FUNCTIONS[1], "ACos");
    assert_eq!(FUNCTIONS[FUNCTIONS.len() - 1], "WinWaitNotActive");
    assert_eq!(MACROS[0], "extended");
}

#[test]
fn lookups_restore_the_canonical_spelling() {
    assert_eq!(canonical_function("stringlen"), Some("StringLen"));
    assert_eq!(canonical_function("MSGBOX"), Some("MsgBox"));
    assert_eq!(canonical_macro("crlf"), Some("CRLF"));
    assert_eq!(canonical_macro("WINDOWSDIR"), Some("WindowsDir"));
    // A user-defined name is not in the table and must survive untouched.
    assert_eq!(canonical_function("MyHelper"), None);
}

#[test]
fn no_two_names_differ_only_by_case() {
    // The reverse lookups are case-insensitive, so a collision would make
    // them ambiguous.
    for table in [&FUNCTIONS[..], &MACROS[..]] {
        for (i, a) in table.iter().enumerate() {
            for b in &table[i + 1..] {
                assert_ne!(a.to_ascii_uppercase(), b.to_ascii_uppercase(), "{a} vs {b}");
            }
        }
    }
}

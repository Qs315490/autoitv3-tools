//! Unit tests for `vocab`'s keyword table, and its agreement with the lexer.
//!
//! Kept out of `vocab.rs` so the module reads as implementation;
//! `#[path]` pulls the file back in as a unit-test module, which is what
//! lets it reach private state the module does not expose.

use super::*;

#[test]
fn ids_are_positions_so_the_order_is_the_contract() {
    // The numbers a compiler writes, not an editorial ordering.
    assert_eq!(KEYWORDS[0], "<Dummy>", "0 is the placeholder");
    assert_eq!(KEYWORDS[1], "And");
    assert_eq!(KEYWORDS[2], "Or");
    assert_eq!(KEYWORDS[KEYWORDS.len() - 1], "Enum");
}

#[test]
fn lookups_restore_the_canonical_spelling() {
    assert_eq!(canonical_keyword("AND"), Some("And"));
    assert_eq!(canonical_keyword("and"), Some("And"));
    assert_eq!(canonical_keyword("elseif"), Some("ElseIf"));
    assert_eq!(canonical_keyword("not a keyword"), None);
}

#[test]
fn no_two_keywords_differ_only_by_case() {
    // The reverse lookup is case-insensitive, so a collision would make it
    // ambiguous rather than merely inefficient.
    for (i, a) in KEYWORDS.iter().enumerate() {
        for b in &KEYWORDS[i + 1..] {
            assert_ne!(a.to_ascii_uppercase(), b.to_ascii_uppercase(), "{a} vs {b}");
        }
    }
}

#[test]
fn the_lexer_and_this_table_are_the_same_list() {
    // Index 0 is the compiler's placeholder rather than a word; every
    // other entry has to be a word the lexer knows, because a compiled
    // script can name any of them.
    for keyword in &KEYWORDS[1..] {
        assert!(
            crate::lexer::keyword(keyword).is_some(),
            "the lexer does not recognise {keyword:?}"
        );
    }
}

//! Unit tests for the input loader's build detection.
//!
//! Kept out of `args.rs` so the module reads as implementation; `#[path]` pulls
//! the file back in as a unit-test module, which is what lets it reach the
//! private `is_compiled_build` predicate.

use super::*;

#[test]
fn a_pe_header_is_a_build() {
    assert!(is_compiled_build(b"MZ\x90\x00"));
}

#[test]
fn a_bare_compiled_chunk_is_a_build() {
    assert!(is_compiled_build(b"AU3!EA06rest of the chunk"));
}

#[test]
fn source_text_is_not_a_build() {
    assert!(!is_compiled_build(b"Func Main()\nEndFunc\n"));
    assert!(!is_compiled_build(b""));
    // The marker only counts at the very start, not somewhere in the bytes.
    assert!(!is_compiled_build(b"Global $s = \"MZ\"\n"));
    assert!(!is_compiled_build(b"Global $s = \"AU3!EA06\"\n"));
}

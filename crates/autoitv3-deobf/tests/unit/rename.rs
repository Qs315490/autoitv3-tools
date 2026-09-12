//! Unit tests for `rename`'s `#forceref` rewriting.
//!
//! Kept out of `rename.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::rewrite_forceref;

#[test]
fn only_forceref_is_rewritten() {
    assert!(rewrite_forceref("noinline", |v| v.to_string()).is_none());
}

#[test]
fn a_forceref_list_keeps_its_shape() {
    let out = rewrite_forceref("forceref $unused, $other", |v| format!("<{v}>"));
    assert_eq!(out.as_deref(), Some("forceref <$unused>, <$other>"));
}

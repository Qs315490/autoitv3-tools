//! Unit tests for the native `DllCall` argument-type table.
//!
//! Kept out of `dll.rs` so the module reads as implementation; `#[path]` pulls
//! the file back in as a unit-test module, which is what lets it reach the
//! private `parse_type` table.

use super::*;

fn ty(token: &str) -> ArgType {
    parse_type(token).unwrap_or_else(|| panic!("{token} should parse"))
}

#[test]
fn handle_and_hwnd_are_pointer_sized() {
    // AutoIt's `HANDLE`/`HWND` are as wide as a pointer on both x86 and x64, so
    // a 32-bit read-back would truncate every handle a Win32 call returns.
    let expected = (usize::BITS / 8) as u8;
    for token in ["handle", "hwnd", "hmodule", "hglobal", "hprocess"] {
        let t = ty(token);
        assert_eq!(t.class, ArgClass::Int, "{token}");
        assert!(t.is_pointer, "{token}");
        assert_eq!(t.width, expected, "{token}");
    }
}

#[test]
fn pointer_suffixed_handles_stay_pointer_sized_by_reference() {
    let t = ty("handle*");
    assert!(t.by_ref);
    assert_eq!(t.width, (usize::BITS / 8) as u8);
}

#[test]
fn pointer_sized_integer_aliases_match_a_pointer() {
    let expected = (usize::BITS / 8) as u8;
    for token in ["uint_ptr", "ulong_ptr", "dword_ptr", "int_ptr", "long_ptr"] {
        assert_eq!(ty(token).width, expected, "{token}");
    }
}

#[test]
fn small_integer_types_keep_their_width() {
    assert_eq!(ty("byte").width, 1);
    assert_eq!(ty("char").width, 1);
    assert_eq!(ty("short").width, 2);
    assert_eq!(ty("ushort").width, 2);
    assert_eq!(ty("wchar").width, 2);
}

#[test]
fn struct_pointers_are_by_reference() {
    let t = ty("struct*");
    assert!(t.by_ref);
    assert_eq!(t.class, ArgClass::Struct);
}

#[test]
fn unknown_types_are_rejected() {
    assert!(parse_type("no_such_type").is_none());
}

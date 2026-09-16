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

#[test]
fn a_string_argument_slot_owns_a_writable_buffer() {
    // Both spellings hand the callee the documented 65536-character buffer —
    // `str*`/`wstr*` are the same allocation with a write-back, not a shorter
    // one. Anything less is a heap overwrite the moment a function fills the
    // buffer in.
    let ansi = ansi_slot_buffer("");
    assert_eq!(ansi.len(), crate::abi::DLLCALL_STRING_CHARS);
    assert!(ansi.len() >= 260, "a path has to fit");

    let wide = wstr_slot_buffer("");
    assert_eq!(wide.len(), crate::abi::DLLCALL_STRING_CHARS);
    assert!(wide.len() >= 260, "a path has to fit");

    // The by-reference form keeps the same room, in the byte shape the slot
    // stores it in.
    assert_eq!(
        wstr_slot_bytes(&wide).len(),
        crate::abi::DLLCALL_STRING_CHARS * 2
    );
}

#[test]
fn a_string_argument_reads_the_callees_write_back_out() {
    // What the result array does with an argument a function filled in: the
    // pointer the slot hands out is the one the read-back looks at.
    let slot = ArgSlot::WStr(wstr_slot_buffer(""));
    let path: Vec<u16> = "C:\\written by the callee".encode_utf16().collect();
    unsafe {
        let out = slot.word() as *mut u16;
        std::ptr::copy_nonoverlapping(path.as_ptr(), out, path.len());
        std::ptr::write(out.add(path.len()), 0);
    }
    assert_eq!(
        slot.result(&ty("wstr"), &Value::str("")).to_autoit_string(),
        "C:\\written by the callee"
    );
}

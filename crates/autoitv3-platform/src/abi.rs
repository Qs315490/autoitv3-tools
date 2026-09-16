//! `DllCall` type tokens: the type itself, the `*` suffix, and the calling
//! convention.
//!
//! AutoIt spells the convention **after** the type — `"INT:cdecl"`,
//! `"DOUBLE:cdecl"` — and that is the form real scripts write, so it is the
//! one the splitter has to read first. The prefix form (`"cdecl:INT"`) is
//! accepted as well: it costs nothing, and it is how everyone writes the
//! convention when they think of it as a modifier on the declaration.
//!
//! Only the native Windows backend byte-calls through this. The emulation
//! layer answers by function name and never looks at the return type, but the
//! module is still built for the crate's own tests on every host, so the
//! rules are covered by `cargo test` wherever it runs.

use autoitv3_runtime::value::Value;

/// The calling convention an argument list is invoked with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Convention {
    /// The Windows default (`extern "system"`).
    StdCall,
    /// `extern "cdecl"` — what AutoIt's `":cdecl"` asks for.
    Cdecl,
}

/// Split a declared return type into its convention and its type.
///
/// The convention is the token after the `:`, the way AutoIt documents it
/// (`"INT:cdecl"`); a leading convention is accepted too.
pub(crate) fn split_convention(raw: &str) -> (Convention, String) {
    match raw.split_once(':') {
        Some((head, tail)) => {
            if let Some(convention) = convention_of(tail) {
                (convention, head.to_string())
            } else if let Some(convention) = convention_of(head) {
                (convention, tail.to_string())
            } else {
                (Convention::StdCall, raw.to_string())
            }
        }
        None => (Convention::StdCall, raw.to_string()),
    }
}

/// The type token with a convention spelling removed, in either order.
///
/// An argument type may carry one even though AutoIt only reads it on the
/// return type, so `"INT*:cdecl"` still names `INT*`.
pub(crate) fn strip_convention(raw: &str) -> &str {
    match raw.split_once(':') {
        Some((head, tail)) if convention_of(tail).is_some() => head,
        Some((head, tail)) if convention_of(head).is_some() => tail,
        _ => raw,
    }
}

/// The buffer a `str`/`wstr` argument is given, in characters.
///
/// AutoIt documents the type as "an ANSI string (a minimum of 65536 chars is
/// allocated)", and that minimum is load-bearing: the pointer a callee receives
/// does **not** stop at the string's own length. The standard UDFs read through
/// it, passing `""` as an output parameter and taking the updated string out of
/// the result array — `PathSearchAndQualifyW(path, "", 4096)`,
/// `SHGetPathFromIDListW(pidl, "")` — so handing such a call a buffer cut to
/// the input's own length is a heap overwrite, not a rounding detail.
pub(crate) const DLLCALL_STRING_CHARS: usize = 65_536;

/// The unit count to allocate for a `str`/`wstr` argument carrying `len`
/// characters — bytes for `str`, UTF-16 units for `wstr`: the string and its
/// terminator, never below [`DLLCALL_STRING_CHARS`].
pub(crate) fn string_arg_units(len: usize) -> usize {
    len.saturating_add(1).max(DLLCALL_STRING_CHARS)
}

/// A `str` argument's buffer: `text` up front, the rest zeroed, so a callee can
/// write more into it than it was given (see [`string_arg_units`]).
pub(crate) fn ansi_argument_buffer(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut buf = vec![0u8; string_arg_units(bytes.len())];
    buf[..bytes.len()].copy_from_slice(bytes);
    buf
}

/// The `wstr` counterpart, in UTF-16 units.
pub(crate) fn wstr_argument_buffer(text: &str) -> Vec<u16> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut buf = vec![0u16; string_arg_units(units.len())];
    buf[..units.len()].copy_from_slice(&units);
    buf
}

/// The little-endian bytes of a `wstr` argument buffer — the shape the native
/// backend's by-reference slot keeps its memory in.
pub(crate) fn units_to_bytes(units: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for u in units {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    bytes
}

/// The string an ANSI argument buffer now holds, after the callee has had it.
///
/// A `str`/`wstr` argument is how a script receives an output string even
/// without the `*` spelling — the shipped UDFs pass `""` and read the result
/// array — so the buffer is read back rather than assuming it is unchanged.
pub(crate) fn ansi_buffer_string(buf: &[u8]) -> Value {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Value::Str(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// The string a wide argument buffer now holds, from its little-endian bytes.
pub(crate) fn wstr_buffer_string(buf: &[u8]) -> Value {
    let mut units = Vec::with_capacity(buf.len() / 2);
    for pair in buf.chunks(2) {
        // A wide buffer is whole units; a trailing odd byte (not something this
        // module ever builds) is ignored rather than panicking.
        if let Ok(bytes) = <[u8; 2]>::try_from(pair) {
            units.push(u16::from_le_bytes(bytes));
        }
    }
    wstr_units_string(&units)
}

/// The string a UTF-16 argument buffer now holds.
pub(crate) fn wstr_units_string(units: &[u16]) -> Value {
    let end = units.iter().position(|u| *u == 0).unwrap_or(units.len());
    Value::Str(String::from_utf16_lossy(&units[..end]))
}

/// The convention a lone token names, if it names one at all.
fn convention_of(token: &str) -> Option<Convention> {
    let token = token.trim();
    if token.eq_ignore_ascii_case("cdecl") {
        Some(Convention::Cdecl)
    } else if token.eq_ignore_ascii_case("stdcall") {
        Some(Convention::StdCall)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_convention_follows_the_type() {
        // `DllCall($dll, "INT:cdecl", "sqlite3_open_v2", …)` — the spelling
        // every real script uses.
        assert_eq!(
            split_convention("INT:cdecl"),
            (Convention::Cdecl, "INT".to_string())
        );
        assert_eq!(
            split_convention("DOUBLE:cdecl"),
            (Convention::Cdecl, "DOUBLE".to_string())
        );
        assert_eq!(
            split_convention("WSTR:stdcall"),
            (Convention::StdCall, "WSTR".to_string())
        );
    }

    #[test]
    fn a_leading_convention_still_reads() {
        assert_eq!(
            split_convention("cdecl:INT"),
            (Convention::Cdecl, "INT".to_string())
        );
        assert_eq!(
            split_convention("stdcall:PTR"),
            (Convention::StdCall, "PTR".to_string())
        );
    }

    #[test]
    fn a_type_without_a_convention_is_stdcall() {
        assert_eq!(
            split_convention("INT"),
            (Convention::StdCall, "INT".to_string())
        );
        assert_eq!(
            split_convention("STRUCT*"),
            (Convention::StdCall, "STRUCT*".to_string())
        );
    }

    #[test]
    fn stripping_is_case_insensitive_and_keeps_the_type() {
        assert_eq!(strip_convention("INT:cdecl"), "INT");
        assert_eq!(strip_convention("cdecl:INT"), "INT");
        assert_eq!(strip_convention("INT*:CDecl"), "INT*");
        assert_eq!(strip_convention("PTR*"), "PTR*");
        assert_eq!(strip_convention("INT"), "INT");
    }

    #[test]
    fn a_string_argument_always_gets_the_documented_buffer() {
        // The standard UDFs pass `""` and let the callee write a path into it,
        // so an empty (or short) string must not shrink the buffer below what
        // AutoIt allocates.
        assert_eq!(string_arg_units(0), DLLCALL_STRING_CHARS);
        assert_eq!(string_arg_units(1), DLLCALL_STRING_CHARS);
        assert_eq!(
            string_arg_units(DLLCALL_STRING_CHARS - 1),
            DLLCALL_STRING_CHARS
        );
        // A string longer than the minimum keeps its own room, plus the NUL.
        assert_eq!(
            string_arg_units(DLLCALL_STRING_CHARS),
            DLLCALL_STRING_CHARS + 1
        );
        assert_eq!(
            string_arg_units(DLLCALL_STRING_CHARS + 100),
            DLLCALL_STRING_CHARS + 101
        );
    }

    #[test]
    fn a_string_argument_capacity_never_wraps() {
        // `len + 1` on a `usize` that is already maximal would wrap to 0 and
        // hand the callee a zero-length allocation.
        assert_eq!(string_arg_units(usize::MAX), usize::MAX);
    }

    #[test]
    fn a_string_argument_buffer_holds_the_string_and_room_to_spare() {
        let path = "C:\\dir\\file.txt";
        let ansi = ansi_argument_buffer(path);
        assert_eq!(ansi.len(), DLLCALL_STRING_CHARS);
        assert_eq!(&ansi[..path.len()], path.as_bytes());
        assert!(ansi[path.len()..].iter().all(|b| *b == 0));

        let wide = wstr_argument_buffer(path);
        assert_eq!(wide.len(), DLLCALL_STRING_CHARS);
        assert_eq!(text(wstr_units_string(&wide)), path);
        assert!(wide[path.len()..].iter().all(|u| *u == 0));
    }

    #[test]
    fn an_empty_string_argument_still_gets_a_full_path_buffer() {
        // `SHGetPathFromIDListW(pidl, "")` and
        // `PathSearchAndQualifyW(path, "", 4096)` both hand the call an empty
        // `wstr` and let the function write a `MAX_PATH`-sized path into it.
        let wide = wstr_argument_buffer("");
        assert!(wide.len() >= 260);
        assert_eq!(wide[0], 0);
        let ansi = ansi_argument_buffer("");
        assert!(ansi.len() >= 260);
        assert_eq!(ansi[0], 0);
    }

    /// The string a value carries, for comparisons (`Value` is not `PartialEq`).
    fn text(value: Value) -> String {
        value.to_autoit_string()
    }

    #[test]
    fn a_string_the_callee_wrote_back_is_read_out_of_the_buffer() {
        // What the standard UDFs do with the result array's argument slot.
        let mut wide = wstr_argument_buffer("");
        let path: Vec<u16> = "C:\\written by the callee".encode_utf16().collect();
        wide[..path.len()].copy_from_slice(&path);
        wide[path.len()] = 0;
        assert_eq!(text(wstr_units_string(&wide)), "C:\\written by the callee");
        assert_eq!(
            text(wstr_buffer_string(&units_to_bytes(&wide))),
            "C:\\written by the callee"
        );

        let mut ansi = ansi_argument_buffer("");
        ansi[..4].copy_from_slice(b"done");
        ansi[4] = 0;
        assert_eq!(text(ansi_buffer_string(&ansi)), "done");
    }

    #[test]
    fn an_untouched_argument_buffer_reads_back_as_its_string() {
        // Nothing was written, so the echo is what the script passed in.
        assert_eq!(
            text(wstr_units_string(&wstr_argument_buffer("hello"))),
            "hello"
        );
        assert_eq!(
            text(ansi_buffer_string(&ansi_argument_buffer("hello"))),
            "hello"
        );
    }

    #[test]
    fn reading_a_buffer_stops_at_its_terminator() {
        // The slack past the string is zeroed, so a read-back cannot wander
        // into it — nor past a buffer whose string was never terminated.
        assert_eq!(text(wstr_units_string(&[b'a' as u16, 0, b'z' as u16])), "a");
        assert_eq!(text(ansi_buffer_string(b"a\0z")), "a");
    }
}

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
}

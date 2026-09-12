//! AutoIt's keyword vocabulary, in the compiler's own numbering.
//!
//! The lexer in [`crate::lexer`] decides which bare words are keywords; this
//! module is that same vocabulary as a *table*, because a compiled script's
//! token stream numbers it: `aut2exe` may store a keyword as an index into
//! [`KEYWORDS`], and stores the rest as upper-cased names.
//!
//! Two properties are load-bearing, which is why this is not a set:
//!
//! * the **order** is AutoIt's, not alphabetical — an index read out of a
//!   compiled script has to land on the word the compiler put there;
//! * the **spelling** is AutoIt's (`ElseIf`, not `ELSEIF`) — a stream saying
//!   `ELSEIF` has to come back out printable.
//!
//! The list is AutoIt v3.3.x's, as recovered from the compiler by the
//! MIT-licensed AutoIt-Ripper. It agrees with what [`crate::lexer`] recognises;
//! the one entry that is not a word is `<Dummy>` at index 0, the compiler's
//! placeholder.

pub const KEYWORDS: [&str; 45] = [
    "<Dummy>",
    "And",
    "Or",
    "Not",
    "If",
    "Then",
    "Else",
    "ElseIf",
    "EndIf",
    "While",
    "WEnd",
    "Do",
    "Until",
    "For",
    "Next",
    "To",
    "Step",
    "In",
    "ExitLoop",
    "ContinueLoop",
    "Select",
    "Case",
    "EndSelect",
    "Switch",
    "EndSwitch",
    "ContinueCase",
    "Dim",
    "ReDim",
    "Local",
    "Global",
    "Const",
    "Static",
    "Func",
    "EndFunc",
    "Return",
    "Exit",
    "ByRef",
    "With",
    "EndWith",
    "True",
    "False",
    "Default",
    "Null",
    "Volatile",
    "Enum",
];

/// The canonical spelling of `name`, matched without case.
///
/// `None` means the word is not a keyword, which for a caller reading a token
/// stream is a real possibility: an identifier can look like one.
pub fn canonical_keyword(name: &str) -> Option<&'static str> {
    let upper = name.to_ascii_uppercase();
    KEYWORDS
        .iter()
        .copied()
        .find(|keyword| keyword.to_ascii_uppercase() == upper)
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../tests/unit/vocab.rs"]
mod tests;

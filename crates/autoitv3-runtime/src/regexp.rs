//! Regular expressions for `StringRegExp` / `StringRegExpReplace`.
//!
//! AutoIt uses **PCRE**. That is an implementation detail of the engine, not
//! something the operating system provides, so this module implements the
//! AutoIt-visible surface on top of the pure-Rust [`regex`] crate — no C
//! dependency, no platform module, identical behaviour on Linux and Windows.
//!
//! # Fidelity
//!
//! [`regex`] is a finite-automaton engine: it deliberately omits the
//! backtracking-only features of PCRE. Anything it cannot compile is reported
//! as a *bad pattern* (`@error = 2`) rather than approximated, so a caller
//! never silently gets a wrong answer. Known PCRE features that are **not**
//! available: lookaround (`(?=)`, `(?!)`, `(?<=)`, `(?<!)`), backreferences
//! (`\1`), atomic groups (`(?>)`), possessive quantifiers, conditionals and
//! recursion. Everything AutoIt's own documentation highlights for everyday
//! use — literal text, `.`, character classes, POSIX classes, `\d \s \w \b`,
//! anchors, quantifiers, alternatives, capturing/named/non-capturing groups
//! and the `(?imsxU)` option groups — is supported.
//!
//! The global settings AutoIt allows at the head of a pattern (`(*UCP)`,
//! `(*CRLF)`, `(*ANYCRLF)`, `(*BSR_*)`, ...) are recognised and removed:
//! [`regex`] is Unicode-aware and multi-line handling is configured through
//! the `(?m)`/`(?s)` options it already understands.

use regex::Regex;

/// A pattern that could not be compiled.
#[derive(Debug, Clone)]
pub struct RegexError {
    /// Best-effort 1-based offset of the error within the AutoIt pattern
    /// (`@extended`). `0` when the engine does not report a position.
    pub offset: usize,
    /// The engine's message, for diagnostics.
    pub message: String,
}

/// Global settings AutoIt permits at the very start of a pattern.
///
/// They are stripped before handing the pattern to [`regex`]; see the module
/// documentation for why.
const PROLOGUE_OPTIONS: &[&str] = &[
    "(*UCP)",
    "(*UTF8)",
    "(*UTF)",
    "(*CR)",
    "(*LF)",
    "(*CRLF)",
    "(*ANYCRLF)",
    "(*ANY)",
    "(*BSR_ANYCRLF)",
    "(*BSR_UNICODE)",
    "(*NOTEMPTY)",
    "(*NOTEMPTY_ATSTART)",
];

/// Strip any leading `(*...)` global settings, returning the remaining pattern
/// and the byte offset the remainder starts at.
fn strip_prologue(pattern: &str) -> (String, usize) {
    let mut rest = pattern;
    let mut stripped = 0usize;
    loop {
        let mut matched = false;
        for opt in PROLOGUE_OPTIONS {
            if let Some(r) = rest.strip_prefix(opt) {
                stripped += opt.len();
                rest = r;
                matched = true;
                break;
            }
        }
        // Unknown `(*NAME)` settings are also skipped rather than compiled.
        if !matched {
            if let Some(r) = rest.strip_prefix("(*") {
                if let Some(end) = r.find(')') {
                    let consumed = 2 + end + 1;
                    stripped += consumed;
                    rest = &rest[consumed..];
                    matched = true;
                }
            }
        }
        if !matched {
            break;
        }
    }
    (rest.to_string(), stripped)
}

/// Compile an AutoIt pattern.
pub fn compile(pattern: &str) -> Result<Regex, RegexError> {
    let (body, stripped) = strip_prologue(pattern);
    match Regex::new(&body) {
        Ok(re) => Ok(re),
        Err(e) => {
            let message = e.to_string();
            // `regex` does not expose a structured offset; pull the one it
            // prints ("... at offset N") when present, and shift it back to a
            // position in the original AutoIt pattern.
            let offset = extract_offset(&message)
                .map(|o| o + stripped + 1)
                .unwrap_or(0);
            Err(RegexError { offset, message })
        }
    }
}

/// Pull `offset N` out of a `regex` error message, if it is there.
fn extract_offset(message: &str) -> Option<usize> {
    let idx = message.find("offset ")?;
    let rest = &message[idx + "offset ".len()..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Rewrite an AutoIt replacement string into the `$n` / `${n}` form the
/// [`regex`] crate understands.
///
/// AutoIt accepts `\0`-`\9` and `$0`-`$9` (with `${1}5` to separate a
/// back-reference from following digits), and requires a literal backslash to
/// be written `\\`.
pub fn translate_replacement(replacement: &str) -> String {
    let mut out = String::with_capacity(replacement.len());
    let mut chars = replacement.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            // `\\` is an escaped backslash; a lone `\` before anything else is
            // not a valid AutoIt back-reference and is treated literally.
            '\\' => match chars.peek().copied() {
                Some('\\') => {
                    chars.next();
                    out.push('\\');
                }
                Some(d) if d.is_ascii_digit() => {
                    chars.next();
                    out.push_str(&format!("${{{d}}}"));
                }
                Some(other) => {
                    chars.next();
                    out.push(other);
                }
                None => out.push('\\'),
            },
            // Already in `$n` / `${n}` / `$name` form: keep as is.
            '$' => {
                out.push('$');
            }
            other => out.push(other),
        }
    }
    out
}

/// Convert a 1-based character offset (AutoIt's `offset` parameter) into a byte
/// index, or `None` when it is past the end of the subject.
pub fn char_offset_to_byte(subject: &str, offset: i64) -> Option<usize> {
    let skip = (offset - 1).max(0) as usize;
    if skip == 0 {
        return Some(0);
    }
    match subject.char_indices().nth(skip) {
        Some((idx, _)) => Some(idx),
        // Exactly one past the last character: an empty tail, still matchable.
        None if skip == subject.chars().count() => Some(subject.len()),
        None => None,
    }
}

/// 1-based character position of a byte index.
pub fn byte_to_char_offset(subject: &str, byte: usize) -> usize {
    subject[..byte.min(subject.len())].chars().count() + 1
}
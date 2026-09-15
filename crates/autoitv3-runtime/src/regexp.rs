//! Regular expressions for `StringRegExp` / `StringRegExpReplace`.
//!
//! AutoIt uses **PCRE**. That is an implementation detail of the engine, not
//! something the operating system provides, so this module implements the
//! AutoIt-visible surface on top of the pure-Rust [`fancy_regex`] crate — no C
//! dependency, no platform module, identical behaviour on Linux and Windows.
//!
//! # Fidelity
//!
//! [`fancy_regex`] keeps the `regex` crate's syntax and delegates patterns
//! without backtracking features to its linear-time engine, while adding a
//! backtracking VM for the constructs PCRE scripts actually use: lookaround,
//! backreferences, atomic groups and conditionals (including variable-length
//! lookbehind). A pattern it cannot compile is reported as a *bad pattern*
//! (`@error = 2`) rather than approximated, so a caller never silently gets a
//! wrong answer.
//!
//! The global settings AutoIt allows at the head of a pattern (`(*UCP)`,
//! `(*CRLF)`, `(*ANYCRLF)`, `(*BSR_*)`, ...) are PCRE directives, not engine
//! syntax, so [`compile`] recognises and removes them before handing the rest
//! to the engine.

use fancy_regex::Regex;

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
/// They are stripped before handing the pattern to the engine; see the module
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
///
/// A pattern the engine rejects is retried once with its assertion
/// conditionals rewritten (see [`rewrite_assertion_conditionals`]); the rest of
/// the pattern is untouched, and the reported error position always refers to
/// the pattern as the script wrote it.
pub fn compile(pattern: &str) -> Result<Regex, RegexError> {
    let (body, stripped) = strip_prologue(pattern);
    let expanded = expand_pcre_escapes(&body);
    let body = expanded.text.clone();
    let failed = match Regex::new(&body) {
        Ok(re) => return Ok(re),
        Err(e) => e,
    };
    let rewritten = rewrite_assertion_conditionals(&body);
    if rewritten != body {
        if let Ok(re) = Regex::new(&rewritten) {
            return Ok(re);
        }
    }
    // A parse error carries the byte position within the *compiled* pattern;
    // map it back through the expansions, then shift it past the prologue, to
    // get a 1-based offset in the AutoIt pattern the script wrote.
    let offset = match &failed {
        fancy_regex::Error::ParseError(pos, _) => expanded.map_offset(*pos) + stripped + 1,
        _ => 0,
    };
    Err(RegexError {
        offset,
        message: failed.to_string(),
    })
}

// ---------------------------------------------------------------------------
// PCRE escapes the engine spells differently
// ---------------------------------------------------------------------------

/// PCRE's horizontal white space, as an engine item and as a class.
const HORIZONTAL: &str = r"\t\p{Zs}";
/// PCRE's vertical white space (`\v` in the engine is only `\x0B`).
const VERTICAL: &str = r"\n\x{0B}\f\r\x{85}\p{Zl}\p{Zp}";

/// A pattern with the PCRE escapes that the engine disagrees about expanded,
/// plus the offset map needed to report errors in the original pattern.
struct ExpandedPattern {
    text: String,
    /// `(offset in text, offset in the original)` for each expansion.
    marks: Vec<(usize, usize)>,
}

impl ExpandedPattern {
    fn map_offset(&self, offset: usize) -> usize {
        match self
            .marks
            .iter()
            .rev()
            .find(|(expanded, _)| *expanded <= offset)
        {
            Some((expanded, original)) => original + (offset - expanded),
            None => offset,
        }
    }
}

/// Rewrite the PCRE escapes whose meaning the engine keeps differently.
///
/// `fancy_regex` follows **Oniguruma** for `\h`/`\H`, where they mean "hex
/// digit" and "not a hex digit"; PCRE — and therefore AutoIt — uses them for
/// horizontal white space. A pattern that starts `^\h*(...)` then eats the
/// first character of the subject whenever that character is a hex digit
/// (`C:\...` loses its drive letter!), so this has to be fixed even though the
/// pattern *compiles*: it is a wrong answer, not a rejected one.
///
/// `\v` differs the same way — PCRE means any vertical white space, the engine
/// means the C escape `\x0B`. `\R` already means what PCRE means.
///
/// The negated forms inside a character class (`[\H]`) cannot be expressed as
/// an item and are left alone; everywhere else they become a negated class.
fn expand_pcre_escapes(pattern: &str) -> ExpandedPattern {
    let mut out = ExpandedPattern {
        text: String::with_capacity(pattern.len()),
        marks: Vec::new(),
    };
    let mut in_class = false;
    let mut i = 0;
    while i < pattern.len() {
        let c = pattern[i..].chars().next().expect("char boundary");
        match c {
            '\\' => {
                let escape = pattern[i + 1..].chars().next();
                let expanded: Option<&str> = match escape {
                    // `\\h` is a literal backslash followed by `h`.
                    Some('\\') => None,
                    // PCRE and the engine agree about `\R`.
                    Some('h') if in_class => Some(HORIZONTAL),
                    Some('h') => Some(&format_horizontal()),
                    Some('H') if !in_class => Some(&format_horizontal_negated()),
                    Some('v') if in_class => Some(VERTICAL),
                    Some('v') => Some(&format_vertical()),
                    Some('V') if !in_class => Some(&format_vertical_negated()),
                    _ => None,
                };
                match (escape, expanded) {
                    (Some(next), Some(replacement)) => {
                        out.marks.push((out.text.len(), i));
                        out.text.push_str(replacement);
                        i += 1 + next.len_utf8();
                        continue;
                    }
                    (Some(next), None) => {
                        out.text.push(c);
                        out.text.push(next);
                        i += 1 + next.len_utf8();
                        continue;
                    }
                    (None, _) => {}
                }
            }
            '[' if !in_class => {
                in_class = true;
                out.text.push(c);
                i += 1;
                // `[^]` and a leading `]` are literals, not the end of the class.
                if pattern[i..].starts_with('^') {
                    out.text.push('^');
                    i += 1;
                }
                if pattern[i..].starts_with(']') {
                    out.text.push(']');
                    i += 1;
                }
                continue;
            }
            ']' if in_class => in_class = false,
            _ => {}
        }
        out.text.push(c);
        i += c.len_utf8();
    }
    out
}

fn format_horizontal() -> String {
    format!("[{HORIZONTAL}]")
}

fn format_horizontal_negated() -> String {
    format!("[^{HORIZONTAL}]")
}

fn format_vertical() -> String {
    format!("[{VERTICAL}]")
}

fn format_vertical_negated() -> String {
    format!("[^{VERTICAL}]")
}

// ---------------------------------------------------------------------------
// Assertion conditionals
// ---------------------------------------------------------------------------

/// One `(?(?=…)yes|no)` conditional found in a pattern.
struct AssertionConditional {
    /// The assertion kind, without its leading `?`: `=`, `!`, `<=` or `<!`.
    kind: &'static str,
    /// The assertion body, without the surrounding parentheses.
    condition: String,
    /// The branch taken when the assertion holds (empty when there is none).
    yes: String,
    /// The branch taken otherwise (empty when there is none).
    no: String,
    /// Offset just past the conditional's closing `)`.
    end: usize,
}

/// Rewrite PCRE's **assertion** conditionals into plain alternation.
///
/// `fancy_regex` implements the group form `(?(1)yes|no)` but not the assertion
/// form, which scripts use to branch on a lookaround. The two are equivalent:
///
/// ```text
/// (?(?=C)Y|Z)   ->   (?:(?=C)Y|(?!C)Z)
/// (?(?=C)Y)     ->   (?:(?=C)Y|(?!C))
/// ```
///
/// The rewrite is faithful because the condition consumes nothing: when it
/// holds, the first branch runs and a failure there fails the group — exactly
/// what PCRE does, since the negated guard cannot match either. The lookbehind
/// and negative kinds map onto each other the same way.
fn rewrite_assertion_conditionals(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut i = 0;
    while i < pattern.len() {
        let c = pattern[i..].chars().next().expect("char boundary");
        match c {
            '\\' => {
                out.push(c);
                i += 1;
                if let Some(next) = pattern[i..].chars().next() {
                    out.push(next);
                    i += next.len_utf8();
                }
            }
            '[' => match class_end(pattern, i) {
                Some(end) => {
                    out.push_str(&pattern[i..end]);
                    i = end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            '(' if pattern[i..].starts_with("(?(") => match find_assertion_conditional(pattern, i)
            {
                Some(cond) => {
                    out.push_str(&render_conditional(&cond));
                    i = cond.end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            _ => {
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// Render a conditional as the equivalent alternation, recursing into both
/// branches (they may hold conditionals of their own).
fn render_conditional(cond: &AssertionConditional) -> String {
    let negated = match cond.kind {
        "=" => "!",
        "!" => "=",
        "<=" => "<!",
        "<!" => "<=",
        _ => "!",
    };
    let yes = rewrite_assertion_conditionals(&cond.yes);
    let no = rewrite_assertion_conditionals(&cond.no);
    let (kind, body) = (cond.kind, cond.condition.as_str());
    format!("(?:(?{kind}{body}){yes}|(?{negated}{body}){no})")
}

/// Recognise `s[start..]` as `(?(?=…)yes|no)` and split it up.
fn find_assertion_conditional(s: &str, start: usize) -> Option<AssertionConditional> {
    if !s[start..].starts_with("(?(") {
        return None;
    }
    // `(?(?=…)`: the condition is an assertion group of its own, opened at
    // `start + 2`, and its kind (`=`, `!`, `<=`, `<!`) starts at `start + 4`.
    let kind = ["<=", "<!", "=", "!"]
        .into_iter()
        .find(|k| s[start + 4..].starts_with(k))?;
    let condition_end = group_end(s, start + 2)?;
    let end = group_end(s, start)?;
    let body_start = start + 4 + kind.len();
    let condition = s[body_start..condition_end - 1].to_string();
    let body = &s[condition_end..end - 1];
    let separator = top_level_separator(s, condition_end, end - 1);
    let (yes, no) = match separator {
        Some(at) => (&s[condition_end..at], &s[at + 1..end - 1]),
        None => (body, ""),
    };
    Some(AssertionConditional {
        kind,
        condition,
        yes: yes.to_string(),
        no: no.to_string(),
        end,
    })
}

/// Offset just past the `)` closing the group that opens at `open`.
fn group_end(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < s.len() {
        let c = s[i..].chars().next()?;
        match c {
            '\\' => {
                i += 1;
                if let Some(next) = s[i..].chars().next() {
                    i += next.len_utf8();
                }
            }
            '[' => i = class_end(s, i)?,
            '(' => {
                depth += 1;
                i += 1;
            }
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
                i += 1;
            }
            _ => i += c.len_utf8(),
        }
    }
    None
}

/// Offset just past the `]` closing the character class that opens at `open`.
fn class_end(s: &str, open: usize) -> Option<usize> {
    let mut i = open + 1;
    if s[i..].starts_with('^') {
        i += 1;
    }
    // A `]` in first position is a literal.
    if s[i..].starts_with(']') {
        i += 1;
    }
    while i < s.len() {
        let c = s[i..].chars().next()?;
        match c {
            '\\' => {
                i += 1;
                if let Some(next) = s[i..].chars().next() {
                    i += next.len_utf8();
                }
            }
            ']' => return Some(i + 1),
            _ => i += c.len_utf8(),
        }
    }
    None
}

/// Offset of the first `|` between `from` and `to` that is not nested inside a
/// group, a character class or an escape.
fn top_level_separator(s: &str, from: usize, to: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = from;
    while i < to {
        let c = s[i..].chars().next()?;
        match c {
            '\\' => {
                i += 1;
                if let Some(next) = s[i..].chars().next() {
                    i += next.len_utf8();
                }
            }
            '[' => i = class_end(s, i)?.min(to),
            '(' => {
                depth += 1;
                i += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            '|' if depth == 0 => return Some(i),
            _ => i += c.len_utf8(),
        }
    }
    None
}

/// Rewrite an AutoIt replacement string into the `$n` / `${n}` form the
/// [`fancy_regex`] crate understands.
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

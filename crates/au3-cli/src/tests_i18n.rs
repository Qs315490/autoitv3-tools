//! Coverage guards for the message catalog.
//!
//! Two things can silently stay English: a clap help string that nobody
//! translated, and a message key the catalog does not know. Both are checked
//! here — the first by walking the command tree clap derived (the same tree
//! [`crate::i18n_cli`] localises), the second by scanning the sources for
//! `tr("…")` / `msg!("…")` literals and looking each one up.
//!
//! `list_clap_strings` is the tool version: with `--ignored` it prints the
//! lists instead of asserting, which is how the translations were collected in
//! the first place.

use clap::CommandFactory;

use autoitv3_i18n::catalog;

use crate::cli::Cli;

/// Every localisable string in a command tree: its own texts, the help of each
/// of its arguments, and everything below it.
fn command_strings(cmd: &clap::Command, out: &mut Vec<String>) {
    for text in [
        cmd.get_about(),
        cmd.get_long_about(),
        cmd.get_before_help(),
        cmd.get_after_help(),
    ]
    .into_iter()
    .flatten()
    {
        out.push(text.to_string());
    }
    for arg in cmd.get_arguments() {
        for text in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
            out.push(text.to_string());
        }
    }
    for sub in cmd.get_subcommands() {
        command_strings(sub, out);
    }
}

/// The command tree clap will actually parse with: `build` expands the
/// auto-generated `help` subcommand, whose `about` is clap's own text.
fn built_command() -> clap::Command {
    let mut cmd = Cli::command();
    cmd.build();
    cmd
}

fn missing(strings: &[String]) -> Vec<&str> {
    let mut seen = std::collections::BTreeSet::new();
    strings
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .filter(|s| catalog::lookup(s).is_none())
        .filter(|s| seen.insert(*s))
        .collect()
}

#[test]
#[ignore = "lists strings instead of asserting; run with --ignored --nocapture"]
fn list_clap_strings() {
    let mut strings = Vec::new();
    command_strings(&built_command(), &mut strings);
    for text in &strings {
        println!("---\n{}", text.replace('\n', "\\n"));
    }
    println!("--- {} strings", strings.len());
}

#[test]
fn clap_scaffolding_fragments_are_translated() {
    // The headings and annotations clap writes itself: `[possible values: …]`
    // and `[default: …]` come from the argument's own definition, so they are
    // not in the command tree the walk above collects.
    for fragment in crate::i18n_cli::HEADINGS {
        assert!(
            catalog::lookup(fragment).is_some(),
            "{fragment:?} has no zh-CN translation"
        );
    }
}

#[test]
fn clap_strings_are_translated() {
    let mut strings = Vec::new();
    command_strings(&built_command(), &mut strings);
    let missing = missing(&strings);
    assert!(
        missing.is_empty(),
        "{} clap strings have no zh-CN translation:\n{}",
        missing.len(),
        missing
            .iter()
            .map(|s| format!("  {s:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn source_strings_are_translated() {
    // crates/au3-cli → crates → the workspace root.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    let mut keys = Vec::new();
    for dir in std::fs::read_dir(root.join("crates")).unwrap() {
        let crate_dir = dir.unwrap().path();
        // `autoitv3-i18n`'s own `tr`/`msg!` uses are documentation and tests.
        if crate_dir.file_name().map(|n| n == "autoitv3-i18n").unwrap_or(false) {
            continue;
        }
        let src = crate_dir.join("src");
        if src.is_dir() {
            collect_keys(&src, &mut keys);
        }
    }
    let keys: Vec<String> = keys.into_iter().map(|k| k.0).collect();
    let missing = missing(&keys);
    assert!(
        missing.is_empty(),
        "{} message keys have no zh-CN translation:\n{}",
        missing.len(),
        missing
            .iter()
            .map(|s| format!("  {s:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Recursively scan a source directory for `tr("…")` / `msg!("…")` literals.
fn collect_keys(dir: &std::path::Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_keys(&path, out);
            continue;
        }
        if path.extension().map(|e| e != "rs").unwrap_or(true) {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        // Doc comments and ordinary comments are not messages: the examples in
        // `autoitv3-i18n`'s own documentation use `tr("…")` to show the API.
        let text: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for needle in ["tr(\"", "msg!(\""] {
            let mut rest = text.as_str();
            while let Some(at) = rest.find(needle) {
                // `attr("x")` contains `tr("x")`: require a non-identifier
                // character before the name (a `::` path qualifier is fine).
                if rest[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
                {
                    rest = &rest[at + 1..];
                    continue;
                }
                rest = &rest[at + needle.len() - 1..];
                if let Some(literal) = literal_at(rest) {
                    out.push((literal, path.clone()));
                    rest = &rest[literal_len(rest)..];
                } else {
                    // Not a literal (a variable, for example): skip past it.
                    rest = &rest[1..];
                }
            }
        }
    }
}

/// The raw text of the string literal starting at `text[0] == '"'`.
fn literal_at(text: &str) -> Option<String> {
    if !text.starts_with('"') {
        return None;
    }
    let raw = &text[..literal_len(text)];
    unescape(raw)
}

fn literal_len(text: &str) -> usize {
    let mut chars = text.char_indices();
    assert_eq!(chars.next().map(|(_, c)| c), Some('"'));
    let mut escaped = false;
    for (i, c) in chars {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return i + 1;
        }
    }
    panic!("unterminated string literal: {text:?}");
}

/// Turn the source form of a normal string literal into its value, so the key
/// matches what a `&str` would be at runtime (`\n` continuations included).
fn unescape(raw: &str) -> Option<String> {
    let body = raw.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\n' => {
                // A line continuation: the newline and the next line's leading
                // whitespace disappear from the value.
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
            }
            other => panic!("unsupported escape \\{other} in {raw:?}"),
        }
    }
    Some(out)
}

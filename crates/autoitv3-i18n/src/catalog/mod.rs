//! The translation tables: English key → Simplified Chinese text.
//!
//! One table per area, so two people (or two agents) can translate different
//! parts of the tree without touching the same file. [`lookup`] walks them in
//! the order listed in [`TABLES`]; the first match wins, so a key must not be
//! given two *different* translations (a test enforces that).
//!
//! Adding a table: create the file, add `pub mod <name>;` and put its
//! `ENTRIES` in [`TABLES`].

pub mod cli_help;
pub mod cli_debug;
pub mod cli_run;
pub mod runtime;
pub mod platform;
pub mod misc;

/// Every table, in lookup order.
pub static TABLES: &[&[(&str, &str)]] = &[
    cli_help::ENTRIES,
    cli_debug::ENTRIES,
    cli_run::ENTRIES,
    runtime::ENTRIES,
    platform::ENTRIES,
    misc::ENTRIES,
];

/// Look up a key in every table.
///
/// The index is built once on the first Chinese lookup: tables are small, but
/// a translated `--trace` run looks up a message per statement, so the linear
/// scan is worth avoiding. English never gets here (`tr` short-circuits).
pub fn lookup(key: &str) -> Option<&'static str> {
    static INDEX: std::sync::OnceLock<std::collections::HashMap<&'static str, &'static str>> =
        std::sync::OnceLock::new();
    INDEX
        .get_or_init(|| {
            let mut map = std::collections::HashMap::new();
            for (en, zh) in entries() {
                map.entry(en).or_insert(zh);
            }
            map
        })
        .get(key)
        .copied()
}

/// Every entry, for tests and tooling.
pub fn entries() -> impl Iterator<Item = (&'static str, &'static str)> {
    TABLES.iter().flat_map(|table| table.iter().copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_has_two_different_translations() {
        let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for (en, zh) in entries() {
            assert!(!en.trim().is_empty(), "empty key in a table");
            assert!(!zh.trim().is_empty(), "empty translation for {en:?}");
            if let Some(previous) = seen.insert(en, zh) {
                assert_eq!(
                    previous, zh,
                    "{en:?} is translated twice with different text"
                );
            }
        }
    }

    #[test]
    fn keys_are_unique_within_a_table() {
        for table in TABLES {
            let mut seen = std::collections::HashSet::new();
            for (en, _) in table.iter() {
                assert!(seen.insert(*en), "{en:?} appears twice in one table");
            }
        }
    }

    #[test]
    fn lookup_finds_a_translation() {
        // The tables may legitimately be empty while a translation is being
        // written; the assertions above are the ones that always apply.
        if let Some((en, zh)) = entries().next() {
            assert_eq!(lookup(en), Some(zh));
        }
    }
}

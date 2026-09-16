//! Message localisation for the AutoIt v3 toolkit.
//!
//! The toolkit speaks English natively: every message is written in English at
//! the call site, and that English text **is** the message key. A translation is
//! a `(english, chinese)` entry in one of the tables under [`catalog`]; when the
//! current language is [`Lang::ZhCn`] and the key is found, the Chinese text is
//! used, otherwise the English text is used unchanged. Nothing is ever hidden
//! by a missing translation, and an English-only build is simply the default
//! language.
//!
//! ```no_run
//! use autoitv3_i18n::{msg, tr, Lang};
//!
//! autoitv3_i18n::set_lang(Lang::ZhCn);
//! println!("{}", tr("no such file"));
//! let path = "x.au3";
//! println!("{}", msg!("cannot read {path}", path = path));
//! ```
//!
//! # Keys with values
//!
//! Templated messages use **named** placeholders (`{path}`, `{line}`), so a
//! translation may reorder them freely. The name is the argument identifier at
//! the call site:
//!
//! ```no_run
//! # let (path, e) = ("x", "boom");
//! let text = autoitv3_i18n::msg!("cannot write {path}: {e}", path = path, e = e);
//! ```
//!
//! `msg!` is `format!`-like but the *key* is the placeholder-bearing English
//! text; `tr` is its placeholder-free counterpart and returns a `&'static str`.
//!
//! # What not to translate
//!
//! Only prose addressed to a human is in the catalog. Identifiers, flag names,
//! paths, type names, generated AutoIt source, disassembly text and
//! machine-readable output stay as they are.

use std::fmt::Display;
use std::sync::atomic::{AtomicU8, Ordering};

pub mod catalog;

/// The Windows UI language (`#[cfg(windows)]`; see the file's own docs).
#[cfg(windows)]
mod windows_locale;

/// A language the toolkit can print in.
///
/// Only English and Simplified Chinese exist; the enum is the place to add
/// more. English is the default so that a program that never selects a
/// language behaves exactly as it always has.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lang {
    /// English (the default).
    #[default]
    En,
    /// Simplified Chinese (`zh-CN`, `zh-Hans`, …).
    ZhCn,
}

impl Lang {
    /// The tag to print for `--lang`-style output (`en`, `zh-CN`).
    pub const fn tag(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::ZhCn => "zh-CN",
        }
    }

    /// Every language the toolkit knows, in a stable order.
    pub const ALL: &'static [Lang] = &[Lang::En, Lang::ZhCn];
}

impl Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tag())
    }
}

impl std::str::FromStr for Lang {
    type Err = ();

    /// Parse a language tag. Region, script, codeset and modifier are ignored,
    /// so `zh-CN`, `zh_CN.UTF-8`, `zh-Hans-CN` and plain `zh` are all
    /// [`Lang::ZhCn`]; anything starting with `en`, and `C`/`POSIX`, is
    /// [`Lang::En`]; an unknown language is an error.
    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        let base = locale_base(tag);
        let lang = base
            .split(['-', '_'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match lang.as_str() {
            "en" | "c" | "posix" => Ok(Lang::En),
            "zh" => Ok(Lang::ZhCn),
            _ => Err(()),
        }
    }
}

/// Strip the codeset and modifier, and turn `_` into `-`: `zh_CN.UTF-8@mod`
/// becomes `zh-CN`.
fn locale_base(tag: &str) -> &str {
    let tag = tag.split('.').next().unwrap_or(tag);
    let tag = tag.split('@').next().unwrap_or(tag);
    tag
}

static LANG: AtomicU8 = AtomicU8::new(0);

/// Set the language messages are printed in. Process-global, like a locale.
pub fn set_lang(lang: Lang) {
    LANG.store(lang as u8, Ordering::Relaxed);
}

/// The language messages are printed in.
pub fn lang() -> Lang {
    match LANG.load(Ordering::Relaxed) {
        1 => Lang::ZhCn,
        _ => Lang::En,
    }
}

/// The language named by the environment.
///
/// `AU3_LANG` wins; otherwise the usual locale variables are consulted in
/// order (`LC_ALL`, `LC_MESSAGES`, `LANG`) and mapped with
/// [`Lang::from_str`]. An unset or unrecognised value means English.
pub fn lang_from_env() -> Lang {
    if let Some(value) = non_empty(std::env::var("AU3_LANG").ok().as_deref()) {
        if !value.eq_ignore_ascii_case("auto") {
            if let Ok(lang) = value.parse() {
                return lang;
            }
            // An unknown tag is ignored in favour of the next source.
        }
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        // `LC_ALL` is allowed to be empty, which means "no override".
        if let Some(value) = non_empty(std::env::var(name).ok().as_deref()) {
            if let Ok(lang) = value.parse() {
                return lang;
            }
        }
    }
    // Windows has no `LANG`: ask the OS for the user's UI language instead.
    host_language().unwrap_or(Lang::En)
}

/// The language this *host* is set to, when the environment does not say.
///
/// On Windows that is `GetUserDefaultLocaleName()`; on every other host the
/// POSIX variables above are the whole story, so this is `None`.
fn host_language() -> Option<Lang> {
    #[cfg(windows)]
    {
        windows_locale::locale_tag()?.parse().ok()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// The language to use for an explicit `--lang VALUE`, if one was given.
///
/// `None`, an empty value and `auto` all fall back to [`lang_from_env`]; an
/// unknown tag does too, so a typo on the command line (`--lang zh-CNN`) is
/// reported in the language the environment asks for rather than in English.
/// The CLI still rejects unknown values with a usage error.
pub fn resolve(explicit: Option<&str>) -> Lang {
    match non_empty(explicit) {
        None => lang_from_env(),
        Some(value) if value.eq_ignore_ascii_case("auto") => lang_from_env(),
        Some(value) => value.parse().unwrap_or_else(|()| lang_from_env()),
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Look up the Chinese translation of an English key.
pub fn lookup(key: &str) -> Option<&'static str> {
    catalog::lookup(key)
}

/// Translate a placeholder-free message.
///
/// The key is the English text itself, so a call site always reads as English
/// and an untranslated key is simply printed as-is.
pub fn tr(key: &'static str) -> &'static str {
    if lang() == Lang::En {
        return key;
    }
    match catalog::lookup(key) {
        Some(text) => text,
        None => {
            note_missing(key);
            key
        }
    }
}

/// [`tr`] for a key that is not a `'static` literal (clap's help strings, for
/// example): the result is owned.
pub fn tr_owned(key: &str) -> String {
    if lang() == Lang::En {
        return key.to_string();
    }
    match catalog::lookup(key) {
        Some(text) => text.to_string(),
        None => {
            note_missing(key);
            key.to_string()
        }
    }
}

/// Render a templated message: look up the translation of `key` and substitute
/// the named placeholders from `args`. Unknown `{names}` are left alone, so a
/// translation that uses a placeholder the call site does not provide shows up
/// in the output instead of panicking.
pub fn render(key: &str, args: &[(&str, &dyn Display)]) -> String {
    let template = if lang() == Lang::En {
        key
    } else {
        match catalog::lookup(key) {
            Some(text) => text,
            None => {
                note_missing(key);
                key
            }
        }
    };
    if args.is_empty() {
        return template.to_string();
    }
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(arg, _)| *arg == name) {
                    Some((_, value)) => out.push_str(&value.to_string()),
                    // Not one of ours: keep the braces as written.
                    None => out.push_str(&rest[open..open + close + 2]),
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Report keys with no translation, once each, when `AU3_I18N_STRICT` is set.
///
/// Handy while checking a Chinese session by hand: every message that would
/// silently stay English is called out on stderr.
fn note_missing(key: &str) {
    if !strict() {
        return;
    }
    static SEEN: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
        std::sync::Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let set = seen.get_or_insert_with(std::collections::HashSet::new);
    if set.len() > 4096 {
        return;
    }
    if set.insert(key.to_string()) {
        eprintln!("[i18n] no zh-CN translation: {key}");
    }
}

fn strict() -> bool {
    static STRICT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *STRICT.get_or_init(|| match std::env::var("AU3_I18N_STRICT") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v.is_empty() || v == "0" || v == "off" || v == "false")
        }
        Err(_) => false,
    })
}

/// Format a message with named placeholders.
///
/// ```no_run
/// # use autoitv3_i18n::msg;
/// # let (file, line) = ("a.au3", 3);
/// let text = msg!("{file}:{line}: unexpected token");
/// ```
#[macro_export]
macro_rules! msg {
    ($key:literal) => {
        $crate::tr($key).to_string()
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::render(
            $key,
            &[$((::std::stringify!($name), &$value as &dyn ::std::fmt::Display)),+],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_parse_into_languages() {
        for tag in ["en", "en-US", "en_GB.UTF-8", "C", "POSIX", "C.UTF-8"] {
            assert_eq!(tag.parse::<Lang>(), Ok(Lang::En), "{tag}");
        }
        for tag in ["zh", "zh-CN", "zh_CN.UTF-8", "zh-Hans", "ZH_cn", "zh-SG@x"] {
            assert_eq!(tag.parse::<Lang>(), Ok(Lang::ZhCn), "{tag}");
        }
        assert_eq!("fr_FR.UTF-8".parse::<Lang>(), Err(()));
    }

    #[test]
    fn resolve_prefers_the_explicit_value_then_the_environment() {
        assert_eq!(resolve(Some("zh-CN")), Lang::ZhCn);
        assert_eq!(resolve(Some("en")), Lang::En);
        // `auto`/empty mean "ask the environment", and an unknown tag is English.
        assert_eq!(resolve(Some("fr")), Lang::En);
    }

    #[test]
    fn rendering_falls_back_to_english_and_keeps_unknown_placeholders() {
        set_lang(Lang::En);
        assert_eq!(render("a {b} c", &[("b", &1)]), "a 1 c");
        assert_eq!(render("a {b} c", &[]), "a {b} c");
        assert_eq!(render("a {b} c", &[("z", &1)]), "a {b} c");
        set_lang(Lang::En);
    }

    #[test]
    fn english_is_the_default() {
        let _ = resolve(None);
        set_lang(Lang::En);
        assert_eq!(tr("plain"), "plain");
        assert_eq!(msg!("plain {n}", n = 1), "plain 1");
    }
}

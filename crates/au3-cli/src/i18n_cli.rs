//! Language selection and clap localisation.
//!
//! The language has to be known *before* clap builds its help text and before
//! any message is printed, so this module does three things at startup:
//!
//! 1. [`lang_from_args`] scans `argv` for `--lang` (falling back to the
//!    environment) and the caller installs the result with
//!    `autoitv3_i18n::set_lang`.
//! 2. [`parse`] localises the command tree clap derived — every `about`,
//!    `long_about`, `help` and `long_help` string goes through the catalog —
//!    and then parses.
//! 3. Help and usage errors are printed from here too, because clap's own
//!    scaffolding (`Usage:`, `Options:`, `error:`, …) needs translating as
//!    well.
//!
//! Clap has no message catalog of its own, so `error: unexpected argument …`
//! style sentences are translated best-effort by [`localize_clap_error`]: the
//! scaffolding and the common sentences are Chinese, and anything not
//! recognised stays English rather than being mangled.

use std::ffi::OsString;

use clap::error::ErrorKind;
use clap::{Arg, Command, CommandFactory, FromArgMatches};

use autoitv3_i18n::{msg, resolve, tr, tr_owned, Lang};

use crate::cli::Cli;

/// The language named on the command line, or by the environment.
///
/// This runs before clap, so it only understands the two spellings `--lang V`
/// and `--lang=V`; anything else is clap's business.
pub fn lang_from_args() -> Lang {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let Some(text) = args[i].to_str() else {
            i += 1;
            continue;
        };
        if text == "--" {
            break;
        }
        if let Some(value) = text.strip_prefix("--lang=") {
            return resolve(Some(value));
        }
        if text == "--lang" {
            if let Some(value) = args.get(i + 1).and_then(|v| v.to_str()) {
                return resolve(Some(value));
            }
        }
        i += 1;
    }
    resolve(None)
}

/// Parse `argv` into [`Cli`], localising clap's output.
pub fn parse() -> Cli {
    // `build` expands the auto-generated `help` subcommand (and the help tree),
    // which is otherwise added lazily inside `try_get_matches` — after the
    // localiser has already walked the tree.
    let mut command = Cli::command();
    command.build();
    let matches = match localize(command).try_get_matches() {
        Ok(matches) => matches,
        Err(err) => finish(err),
    };
    match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => finish(err),
    }
}

/// Print a clap error (help, version, or a usage error) and exit.
fn finish(err: clap::Error) -> ! {
    let code = err.exit_code();
    match err.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
            print!("{}", localize_help(&err.render().to_string()));
        }
        ErrorKind::DisplayVersion => print!("{}", err),
        _ => eprint!("{}", localize_clap_error(&err.render().to_string())),
    }
    std::process::exit(code);
}

/// Translate every string clap derived into the command tree.
fn localize(cmd: Command) -> Command {
    localize_help_text(cmd)
        .mut_args(localize_arg)
        .mut_subcommands(localize)
}

/// `about` / `long_about` / `before_help` / `after_help` on one command.
fn localize_help_text(cmd: Command) -> Command {
    let mut cmd = cmd;
    if let Some(text) = cmd.get_about().map(|s| s.to_string()) {
        cmd = cmd.about(tr_owned(&text));
    }
    if let Some(text) = cmd.get_long_about().map(|s| s.to_string()) {
        cmd = cmd.long_about(tr_owned(&text));
    }
    if let Some(text) = cmd.get_before_help().map(|s| s.to_string()) {
        cmd = cmd.before_help(tr_owned(&text));
    }
    if let Some(text) = cmd.get_after_help().map(|s| s.to_string()) {
        cmd = cmd.after_help(tr_owned(&text));
    }
    cmd
}

/// The `help` / `long_help` of one argument (its doc comment).
fn localize_arg(arg: Arg) -> Arg {
    let mut arg = arg;
    if let Some(text) = arg.get_help().map(|s| s.to_string()) {
        arg = arg.help(tr_owned(&text));
    }
    if let Some(text) = arg.get_long_help().map(|s| s.to_string()) {
        arg = arg.long_help(tr_owned(&text));
    }
    arg
}

// ---------------------------------------------------------------------------
// clap's own output
// ---------------------------------------------------------------------------

/// Headings and trailing hints clap writes itself, keyed by their English text
/// in the catalog. Placeholder names (`<FILE>`, `--output <FILE>`) stay as they
/// are: they are identifiers, not prose.
pub(crate) const HEADINGS: &[&str] = &[
    // clap's own annotations: `--lang`'s value list, and every argument's
    // default.
    "[possible values: ",
    "[default: ",
    "Usage:",
    "Commands:",
    "Options:",
    "Arguments:",
    "For more information, try '--help'.",
];

/// Translate the headings in rendered help output.
pub fn localize_help(text: &str) -> String {
    let mut out = text.to_string();
    for heading in HEADINGS {
        out = out.replace(heading, tr(heading));
    }
    out
}

/// Translate a rendered clap error: the headings above, plus the message
/// sentences clap emits most often. Unknown text is left alone.
pub fn localize_clap_error(text: &str) -> String {
    let lines: Vec<String> = text.split('\n').map(localize_clap_line).collect();
    localize_help(&lines.join("\n"))
}

/// Best-effort translation of one error line: `error: …` / `tip: …` plus the
/// sentences clap builds by interpolation.
fn localize_clap_line(line: &str) -> String {
    // clap indents `tip:` and the argument list; keep the indentation.
    let indent_len = line.len() - line.trim_start().len();
    let (indent, trimmed) = line.split_at(indent_len);
    let (prefix, rest) = if let Some(rest) = trimmed.strip_prefix("error: ") {
        (tr("error: "), rest)
    } else if let Some(rest) = trimmed.strip_prefix("tip: ") {
        (tr("tip: "), rest)
    } else {
        return line.to_string();
    };
    let body = if let Some(rest) = rest.strip_prefix("unexpected argument ") {
        match rest.strip_suffix(" found") {
            Some(value) => msg!("unexpected argument {value} found", value = value),
            None => msg!("unexpected argument {rest}", rest = rest),
        }
    } else if let Some(rest) = rest.strip_prefix("unrecognized subcommand ") {
        msg!("unrecognized subcommand {rest}", rest = rest)
    } else if let Some(rest) = rest.strip_prefix("invalid value ") {
        // clap's shape: `invalid value 'V' for '--opt <VAL>': detail`.
        match rest
            .split_once(" for ")
            .and_then(|(value, tail)| tail.split_once(": ").map(|(arg, detail)| (value, arg, detail)))
        {
            Some((value, arg, detail)) => msg!(
                "invalid value {value} for {arg}: {detail}",
                value = value,
                arg = arg,
                detail = detail
            ),
            None => msg!("invalid value {rest}", rest = rest),
        }
    } else if let Some(rest) = rest.strip_prefix("a value is required for ") {
        msg!("a value is required for {rest}", rest = rest)
    } else if rest == "the following required arguments were not provided:" {
        msg!("the following required arguments were not provided:")
    } else if let Some(rest) = rest.strip_prefix("to pass ") {
        // clap's hint: `to pass 'X' as a value, use '-- X'`.
        match rest.split_once(" as a value, use ") {
            Some((value, hint)) => msg!(
                "to pass {value} as a value, use {hint}",
                value = value,
                hint = hint
            ),
            None => format!("{prefix}{rest}"),
        }
    } else {
        // Not a sentence we know: translate the prefix and leave the rest as
        // written rather than mangling it.
        format!("{indent}{prefix}{rest}")
    };
    format!("{indent}{prefix}{body}")
}

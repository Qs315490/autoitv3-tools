//! Where a source-emitting subcommand writes its result.
//!
//! `-o FILE` writes to a file, while `-o -` or no `-o` at all writes to stdout.
//! Nothing here ever opens the *input* file for writing, so the original source
//! is never modified.

use std::io::Write;

use autoitv3_i18n::msg;
use autoitv3_runtime::Value;

use crate::args::{CliError, CliResult};

/// Write `content` to `target`: `None`/`Some("-")` means stdout.
pub fn write_output(target: Option<&str>, content: &str) -> CliResult<()> {
    match target {
        None | Some("-") => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(content.as_bytes())
                .and_then(|()| lock.flush())
                .map_err(|e| CliError::io(msg!("cannot write to stdout: {e}", e = e)))
        }
        Some(path) => std::fs::write(path, content)
            .map_err(|e| CliError::io(msg!("cannot write {path}: {e}", path = path, e = e))),
    }
}
/// Render a runtime value for display.
///
/// Scalars print the way `au3 run` has always printed them (strings quoted, so
/// an empty string is visible). Arrays, maps and binaries are *summarised*: a
/// table with 28,000 entries is not something anyone wants to see at a prompt,
/// and neither is a 231 KB binary.
pub fn format_value(v: &Value) -> String {
    match v {
        Value::Array(a) => {
            let a = a.borrow();
            let preview: Vec<String> = a
                .iter()
                .take(6)
                .map(|x| format!("{:?}", x.to_autoit_string()))
                .collect();
            let more = if a.len() > preview.len() { ", ..." } else { "" };
            format!("Array[{}] {{{}{}}}", a.len(), preview.join(", "), more)
        }
        Value::Map(m) => {
            let m = m.borrow();
            let preview: Vec<String> = m
                .iter()
                .take(6)
                .map(|(k, v)| format!("{k:?}: {}", format_value(v)))
                .collect();
            let more = if m.len() > preview.len() { ", ..." } else { "" };
            format!("Map[{}] {{{}{}}}", m.len(), preview.join(", "), more)
        }
        Value::Binary(b) => format!("Binary[{}]", b.len()),
        other => format!("{other:?}"),
    }
}

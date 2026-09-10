//! Where a source-emitting subcommand writes its result.
//!
//! `-o FILE` writes to a file, while `-o -` or no `-o` at all writes to stdout.
//! Nothing here ever opens the *input* file for writing, so the original source
//! is never modified.

use std::io::Write;

use crate::args::{CliError, CliResult};

/// Write `content` to `target`: `None`/`Some("-")` means stdout.
pub fn write_output(target: Option<&str>, content: &str) -> CliResult<()> {
    match target {
        None | Some("-") => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(content.as_bytes())
                .and_then(|()| lock.flush())
                .map_err(|e| CliError::io(format!("cannot write to stdout: {e}")))
        }
        Some(path) => std::fs::write(path, content)
            .map_err(|e| CliError::io(format!("cannot write {path}: {e}"))),
    }
}
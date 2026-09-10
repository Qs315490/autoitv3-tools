//! Shared argument types, the CLI error type, and the input loader.
//!
//! Command-specific arguments live with their command; only the pieces more
//! than one command needs are here.

use autoitv3_ast::{parse, Program};
use clap::Args;

/// `-o FILE` output redirection, shared by the source-emitting commands.
///
/// `-o FILE` writes to a file; `-o -` (or omitting `-o`) writes to stdout, so
/// the *input* file is never modified.
#[derive(Args, Debug, Clone)]
pub struct OutputArgs {
    /// Write the result to FILE instead of stdout (use `-` for stdout)
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    pub output: Option<String>,
}

/// A CLI failure carrying the process exit code it should produce.
///
/// Usage errors never reach here — `clap` reports those itself with exit
/// code 2 — so this only covers input and IO failures.
#[derive(Debug)]
pub struct CliError {
    /// Process exit code: `1` for input failures, `2` for IO failures.
    pub code: i32,
    /// Message printed to stderr (without the `error:` prefix).
    pub message: String,
}

impl CliError {
    /// The input could not be processed (parse failure, runtime error, ...).
    pub fn failure(message: impl Into<String>) -> Self {
        Self { code: 1, message: message.into() }
    }

    /// The input could not be read, or the output could not be written.
    pub fn io(message: impl Into<String>) -> Self {
        Self { code: 2, message: message.into() }
    }
}

/// Convenience alias for the command entry points.
pub type CliResult<T> = Result<T, CliError>;

/// Read and parse the AutoIt program at `path`.
pub fn load_program(path: &str) -> CliResult<Program> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| CliError::io(format!("cannot read {path}: {e}")))?;
    parse(&src).map_err(|e| CliError::failure(format!("parse error in {path}: {e}")))
}

/// Parse a `--arg` value: integers (decimal or `0x` hex) become `Int`,
/// anything else stays a string.
pub fn parse_arg_value(raw: &str) -> autoitv3_runtime::Value {
    use autoitv3_runtime::Value;
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Int(i);
    }
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        if let Ok(i) = i64::from_str_radix(hex, 16) {
            return Value::Int(i);
        }
    }
    Value::Str(raw.to_string())
}
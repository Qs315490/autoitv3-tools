//! Shared argument-parsing helpers and the CLI error type.
//!
//! Every subcommand lives in its own module under [`crate::commands`] and
//! receives the raw arguments that follow its name. Helpers that more than one
//! command needs (reading + parsing the input program, the
//! `<input> [-o FILE]` shape) live here so the commands stay small.

use autoitv3_ast::{parse, Program};

/// A CLI failure carrying the process exit code it should produce.
#[derive(Debug)]
pub struct CliError {
    /// Process exit code: `2` for usage/IO problems, `1` for input failures.
    pub code: i32,
    /// Message printed to stderr (without the `error:` prefix).
    pub message: String,
}

impl CliError {
    /// A malformed invocation (unknown option, missing argument, ...).
    pub fn usage(message: impl Into<String>) -> Self {
        Self { code: 2, message: message.into() }
    }

    /// The input could not be processed (parse failure, runtime error, ...).
    pub fn failure(message: impl Into<String>) -> Self {
        Self { code: 1, message: message.into() }
    }

    /// The input could not be read, or the output could not be written.
    pub fn io(message: impl Into<String>) -> Self {
        Self { code: 2, message: message.into() }
    }
}

/// Convenience alias for the CLI entry points.
pub type CliResult<T> = Result<T, CliError>;

/// Read and parse the AutoIt program at `path`.
pub fn load_program(path: &str) -> CliResult<Program> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| CliError::io(format!("cannot read {path}: {e}")))?;
    parse(&src).map_err(|e| CliError::failure(format!("parse error in {path}: {e}")))
}

/// Accept exactly one positional argument (the input file).
pub fn single_input(command: &str, args: &[String]) -> CliResult<String> {
    let mut input: Option<String> = None;
    for a in args {
        if a.starts_with('-') && a != "-" {
            return Err(CliError::usage(format!(
                "{command}: unexpected option `{a}` (this command takes no options)"
            )));
        }
        if input.is_some() {
            return Err(CliError::usage(format!("{command}: multiple input files given")));
        }
        input = Some(a.clone());
    }
    input.ok_or_else(|| CliError::usage(format!("{command}: missing input file")))
}

/// The `<input> [-o FILE]` argument shape shared by the source-emitting
/// commands (`pretty`, `deobfuscate`).
#[derive(Debug)]
pub struct InOut {
    /// Input `.au3` path.
    pub input: String,
    /// Output target: `None` or `Some("-")` means stdout.
    pub output: Option<String>,
}

/// Parse the `<input> [-o FILE]` shape.
///
/// `-o FILE` writes to a file; `-o -` (or omitting `-o`) writes to stdout, so
/// the input file itself is never modified.
pub fn parse_in_out(command: &str, args: &[String]) -> CliResult<InOut> {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                let value = args.get(i).ok_or_else(|| {
                    CliError::usage(format!(
                        "{command}: -o requires a FILE argument (use `-o -` for stdout)"
                    ))
                })?;
                output = Some(value.clone());
            }
            s if s.starts_with('-') && s != "-" => {
                return Err(CliError::usage(format!("{command}: unknown option `{s}`")));
            }
            s => {
                if input.is_some() {
                    return Err(CliError::usage(format!("{command}: multiple input files given")));
                }
                input = Some(s.to_string());
            }
        }
        i += 1;
    }

    let input = input.ok_or_else(|| {
        CliError::usage(format!("{command}: missing input file (try `au3 help`)"))
    })?;
    Ok(InOut { input, output })
}

/// Parse a `--arg` value: integers (decimal or `0x` hex) become `Int`,
/// anything else a string.
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
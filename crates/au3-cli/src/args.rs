//! Shared argument types, the CLI error type, and the input loader.
//!
//! Command-specific arguments live with their command; only the pieces more
//! than one command needs are here.

use std::path::Path;

use autoitv3_ast::{parse, Program};
use autoitv3_platform::{find_resource_module, WindowsArch, WindowsEmulation, WindowsVersion};
use autoitv3_runtime::platform::Platform;
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

/// Windows-emulation selection, shared by the commands that install a platform.
///
/// Off Windows the platform stack starts with the emulation layer described in
/// `autoitv3_platform::winemu`; these flags choose what machine it presents.
/// When a flag is omitted the matching environment variable is consulted
/// (`AU3_WIN_VERSION`, `AU3_WIN_ARCH`, `AU3_WIN_EMU`, `AU3_RESOURCE_MODULE`),
/// and then the default — **Windows 10 x64**.
#[derive(Args, Debug, Clone, Default)]
pub struct WinEmuArgs {
    /// Emulated Windows version: xp, vista, 7, 8, 81, 10, 11
    #[arg(long = "win-version", value_name = "VER")]
    pub win_version: Option<String>,

    /// Emulated architecture: x86, x64, arm64
    #[arg(long = "win-arch", value_name = "ARCH")]
    pub win_arch: Option<String>,

    /// PE image whose resources `FindResourceW`/`LoadResource` answer from
    /// (usually found automatically next to the script)
    #[arg(long = "resource-module", value_name = "FILE")]
    pub resource_module: Option<String>,

    /// Do not install the emulation layer; Windows-only calls become
    /// undefined-function errors
    #[arg(long = "no-win-emu")]
    pub no_win_emu: bool,
}

impl WinEmuArgs {
    /// Build the platform stack these arguments select.
    ///
    /// The environment is read first so `AU3_WIN_VERSION` keeps working, then
    /// the flags override it, so an explicit `--win-version` always wins.
    ///
    /// `script` is the `.au3` being analysed. It seeds the search for the PE
    /// image that answers `FindResourceW`: a script compiled into an `.exe`
    /// reads its payload and its string table out of that image's resources,
    /// and the image normally sits next to the script, so the default is to
    /// look there and then in the working directory. Passing
    /// `--resource-module` (or `AU3_RESOURCE_MODULE`) skips the search.
    pub fn platform(&self, script: Option<&Path>) -> CliResult<Box<dyn Platform>> {
        let mut emu = WindowsEmulation::from_env();
        if self.no_win_emu {
            emu = emu.disabled();
        }
        if let Some(raw) = &self.win_version {
            let version = WindowsVersion::from_name(raw).ok_or_else(|| {
                CliError::failure(format!(
                    "unknown --win-version {raw:?} (try win7, win8, win81, win10, win11)"
                ))
            })?;
            emu = emu.with_version(version);
        }
        if let Some(raw) = &self.win_arch {
            let arch = WindowsArch::from_name(raw).ok_or_else(|| {
                CliError::failure(format!("unknown --win-arch {raw:?} (try x86, x64, arm64)"))
            })?;
            emu = emu.with_arch(arch);
        }
        if let Some(path) = &self.resource_module {
            if !Path::new(path).is_file() {
                return Err(CliError::failure(format!(
                    "--resource-module {path}: no such file"
                )));
            }
            emu = emu.with_module_file(path);
        } else if !self.no_win_emu && emu.module_path().is_none() {
            if let Some(found) = find_resource_module(script) {
                eprintln!(
                    "# resource module: {} (found next to the script; override with --resource-module)",
                    found.display()
                );
                emu = emu.with_module_file(found);
            }
        }
        Ok(autoitv3_platform::host_platform_with(emu))
    }
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
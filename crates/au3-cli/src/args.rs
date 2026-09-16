//! Shared argument types, the CLI error type, and the input loader.
//!
//! Command-specific arguments live with their command; only the pieces more
//! than one command needs are here.

use std::path::{Path, PathBuf};

use autoitv3_ast::{parse, Program};
use autoitv3_i18n::{msg, tr};
use autoitv3_platform::{
    find_resource_module, has_staged_resources, resource_search_dirs, PlatformOptions,
    WindowsArch, WindowsEmulation, WindowsVersion,
};
use autoitv3_runtime::interp::DEFAULT_MAX_STEPS;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::profile::{EffectKind, ExecutionProfile};
use autoitv3_runtime::BuildFacts;
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

/// How the emulated GUI is presented while the script runs.
///
/// The GUI *semantics* (165 functions) are answered by the emulation layer on
/// every host — every backend sees the same windows, controls, events and
/// return values. The mode only decides what draws them, and each value names
/// the backend it selects, so another one (gtk, qt, …) is a new value beside
/// these plus a build feature, not a new mode.
///
/// `auto` is the default because the right answer is the platform's: on Windows
/// the emulation runs the real Win32 controls, so the window a script creates is
/// an ordinary native window, hit-tested and redrawn by the OS. Elsewhere no
/// window system is assumed and nothing is drawn. `headless` forces that
/// no-drawing behaviour on every host, which is what an analysis run or a CI job
/// wants — nothing to open, nothing to leak, identical output. `native` asks for
/// the Win32 backend by name (the same one `auto` picks on Windows), and `egui`
/// replaces whatever the platform chose with the eframe-backed
/// `autoitv3_gui_egui::LiveBackend`, for seeing a script's GUI on a host that
/// has no native path to it.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuiMode {
    /// Whatever the platform does by itself: native Win32 controls on Windows,
    /// the in-memory model elsewhere (the default).
    #[default]
    Auto,
    /// Always the in-memory model: GUI calls return their real results and
    /// nothing is drawn, on every host.
    Headless,
    /// The Windows-native backend: real Win32 windows and controls, drawn and
    /// hit-tested by the OS (needs a Windows host).
    Native,
    /// A real window driven by `autoitv3_gui_egui::LiveBackend` (needs a build
    /// with the `gui-egui` feature).
    Egui,
}

/// The Windows-native GUI backend, or the error that says this host has none.
///
/// `--gui native` names the backend `auto` already picks on Windows: real Win32
/// windows and controls. Whether it exists is a host question, not a build one —
/// every Windows build has it (the platform's `native_gui_backend` returns it
/// there) and no other host does — so the message is about the host, not about
/// features to enable.
pub fn native_gui_backend() -> CliResult<Box<dyn autoitv3_platform::winemu::GuiBackend>> {
    autoitv3_platform::native_gui_backend().ok_or_else(|| {
        CliError::failure(tr(
            "--gui native needs a Windows host: the native backend is the real Win32 one",
        ))
    })
}

impl GuiMode {
    /// The mode a run actually uses, given what the profile is.
    ///
    /// `auto` means "the platform's own backend", which on Windows is the real
    /// Win32 one: real windows, and dialogs that wait for somebody to click
    /// them. A deterministic run is an analysis — it must not put either on
    /// somebody's desktop — so `auto` resolves to [`GuiMode::Headless`] there.
    /// An explicit `--gui` is never overridden.
    pub const fn resolve(self, deterministic: bool) -> Self {
        match self {
            GuiMode::Auto if deterministic => GuiMode::Headless,
            other => other,
        }
    }
}

/// Windows-emulation selection, shared by the commands that install a platform.
///
/// Off Windows the platform stack starts with the emulation layer described in
/// `autoitv3_platform::winemu`; these flags choose what machine it presents.
/// When a flag is omitted the matching environment variable is consulted
/// (`AU3_WIN_VERSION`, `AU3_WIN_ARCH`, `AU3_WIN_EMU`, `AU3_RESOURCE_MODULE`,
/// `AU3_WIN_DRIVE_MAP`),
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

    /// Host directory the emulated `C:` drive stands for (default: the host
    /// root, so `C:\home\me\a.dat` is `/home/me/a.dat`; empty disables the
    /// mapping)
    #[arg(long = "win-drive-map", value_name = "ROOT")]
    pub win_drive_map: Option<String>,

    /// Leave `C:` unmapped: every path the script builds is taken as a host
    /// path, exactly as the emulation did before the drive map existed
    #[arg(long = "no-win-drive-map")]
    pub no_win_drive_map: bool,

    /// Route a function area through the emulation layer even where a native
    /// implementation exists. Repeatable. Areas: `registry` (Reg*),
    /// `clipboard` (Clip*), or a raw function name (`RegWrite`).
    #[arg(long = "emulate", value_name = "AREA")]
    pub emulate: Vec<String>,
}

/// The side-effect knobs shared by the commands that set a profile.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct EffectArgs {
    /// Allow one class of side effect in the deterministic profile. Repeatable.
    /// Kinds: file, env, registry, clipboard, spawn, shutdown, net, process.
    #[arg(long = "allow", value_name = "KIND")]
    pub allow: Vec<String>,

    /// Deny one class of side effect, even in the faithful profile. Repeatable.
    #[arg(long = "deny", value_name = "KIND")]
    pub deny: Vec<String>,
}

impl EffectArgs {
    /// Layer the `--allow`/`--deny` decisions over a base profile.
    pub fn apply(&self, mut profile: ExecutionProfile) -> CliResult<ExecutionProfile> {
        for (list, allowed) in [(&self.allow, true), (&self.deny, false)] {
            for raw in list {
                let kind = EffectKind::from_name(raw).ok_or_else(|| {
                    let shown = format!("{raw:?}");
                    CliError::failure(msg!(
                        "unknown effect {shown} (try file, env, registry, clipboard, spawn, \
                         shutdown, net, process)",
                        shown = shown
                    ))
                })?;
                if profile.overrides.get(kind).is_some() {
                    let shown = format!("{raw:?}");
                    return Err(CliError::failure(msg!(
                        "effect {shown} given to both --allow and --deny",
                        shown = shown
                    )));
                }
                profile = profile.with_effect(kind, allowed);
            }
        }
        Ok(profile)
    }
}

/// `@Compiled` selection shared by the commands that run the script body.
///
/// By default the input decides: a compiled build (`.exe`/`.a3x`) answers 1 and
/// a `.au3` source answers 0. A script extracted from a build is still `.au3`
/// though — `deobf -o script.au3` writes exactly that — and real scripts branch
/// on the macro (e.g. to read their payload out of the image instead of a
/// sibling file), so both directions can be forced to compare like with like.
#[derive(Args, Debug, Clone, Copy, Default)]
pub struct CompiledArgs {
    /// Answer `@Compiled = 1` even when the input is `.au3` source
    #[arg(long, conflicts_with = "no_compiled")]
    pub compiled: bool,

    /// Answer `@Compiled = 0` even when the input is a compiled build
    #[arg(long)]
    pub no_compiled: bool,
}

impl CompiledArgs {
    /// The value `@Compiled` should answer: a flag wins, otherwise the input
    /// decides (`input_is_build`).
    pub fn resolve(&self, input_is_build: bool) -> bool {
        if self.compiled {
            true
        } else if self.no_compiled {
            false
        } else {
            input_is_build
        }
    }
}

/// The preset a command runs under when neither profile flag was given.
///
/// Running a script (`run`, `debug`) means [AutoIt semantics](Preset::Faithful):
/// it should do what the script says. Evaluating one for analysis (`evaluate`,
/// `deobfuscate --evaluate`) means the
/// [deterministic profile](Preset::Deterministic): fast, repeatable, and unable
/// to touch the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    /// Real delays, real entropy, real side effects.
    Faithful,
    /// `Sleep` skipped, `Random` from a fixed seed, side effects refused.
    Deterministic,
}

impl Preset {
    /// The profile this preset names.
    pub const fn profile(self) -> ExecutionProfile {
        match self {
            Preset::Faithful => ExecutionProfile::faithful(),
            Preset::Deterministic => ExecutionProfile::deterministic(),
        }
    }
}

/// Execution-semantics selection shared by the commands that run a script.
///
/// `run` and `debug` default to AutoIt's own semantics ([`Preset::Faithful`]);
/// the analysis commands (`evaluate`, `deobfuscate --evaluate`) default to the
/// deterministic profile ([`Preset::Deterministic`]). Either flag overrides the
/// command's default, and the two are mutually exclusive.
#[derive(Args, Debug, Clone, Default)]
pub struct ProfileArgs {
    /// Run with AutoIt's own semantics: real delays, real entropy and real side
    /// effects
    ///
    /// The default for `run` and `debug` — running a script should do what the
    /// script says. The analysis commands (`evaluate`, `deobfuscate
    /// --evaluate`) default to `--deterministic` instead.
    #[arg(long)]
    pub faithful: bool,

    /// Run with the deterministic analysis profile: `Sleep` skipped, `Random`
    /// from a fixed seed, every side effect refused
    ///
    /// A refused side effect returns its failure value with `@error = 1` and is
    /// reported once per kind on stderr. The default for `evaluate` and
    /// `deobfuscate --evaluate`, which analyse a sample without letting it
    /// touch the machine; `run` and `debug` default to `--faithful`.
    #[arg(long, conflicts_with = "faithful")]
    pub deterministic: bool,
}

impl ProfileArgs {
    /// Which preset these arguments select, given the command's own default.
    pub fn preset(&self, default: Preset) -> Preset {
        if self.faithful {
            Preset::Faithful
        } else if self.deterministic {
            Preset::Deterministic
        } else {
            default
        }
    }

    /// The profile these arguments select, given the command's own default.
    pub fn profile(&self, default: Preset) -> ExecutionProfile {
        self.preset(default).profile()
    }
}

/// Runaway-loop guard shared by every command that runs the interpreter.
#[derive(Args, Debug, Clone)]
pub struct StepArgs {
    /// Maximum interpreter steps before giving up (0 = no limit)
    #[arg(long = "max-steps", value_name = "N", default_value_t = DEFAULT_MAX_STEPS)]
    pub max_steps: u64,
}

/// Substitution knobs shared by the commands that inline runtime values.
#[derive(Args, Debug, Clone, Default)]
pub struct SubstituteArgs {
    /// Rewrite `Global Const $t = Build()` into the table's literal value
    #[arg(long)]
    pub inline_tables: bool,
}

/// Progress-heartbeat control shared by the commands that run a long
/// evaluation.
#[derive(Args, Debug, Clone, Default)]
pub struct ProgressArgs {
    /// Do not print the periodic progress heartbeat for long evaluations
    #[arg(long = "no-progress")]
    pub no_progress: bool,
}

/// `#include` search path, shared by the commands that run or analyse a script.
///
/// AutoIt inserts the contents of an included file at the point of the
/// directive, which is where a script's constants and helper functions come
/// from, so the commands that execute or rewrite a script expand them. The
/// search order is the help page's: `#include "file"` looks in the script's own
/// directory first, `#include <file>` in the standard library first, and both
/// then walk the directories named here (and in `AU3_INCLUDE_PATH`). Those stand
/// in for the registry value `HKEY_CURRENT_USER\Software\AutoIt
/// v3\AutoIt\Include`, which is where a normal AutoIt install keeps extra
/// library paths.
///
/// The standard library itself is the `Include` directory of an AutoIt install:
/// this tool has none next to its own binary, so `C:\Program Files
/// (x86)\AutoIt3\Include` and friends are searched instead. An install anywhere
/// else needs `-I` or `AU3_INCLUDE_PATH`; a run that cannot find an include says
/// which directories it tried.
#[derive(Args, Debug, Clone, Default)]
pub struct IncludeArgs {
    /// Add DIR to the `#include` search path (repeatable)
    #[arg(short = 'I', long = "include-path", value_name = "DIR")]
    pub include_paths: Vec<String>,

    /// Do not expand `#include` at all: run the script as if the directives
    /// were not there
    #[arg(long = "no-includes")]
    pub no_includes: bool,
}

/// Expand an `--emulate` area alias into the function names routed to the
/// emulation layer.
fn expand_emulate_area(raw: &str) -> CliResult<Vec<String>> {
    let names: &[&str] = match raw.to_ascii_lowercase().as_str() {
        "registry" | "reg" => &[
            "RegRead",
            "RegWrite",
            "RegDelete",
            "RegEnumKey",
            "RegEnumVal",
        ],
        "clipboard" | "clip" => &["ClipGet", "ClipPut"],
        other => return Ok(vec![other.to_string()]),
    };
    Ok(names.iter().map(|n| n.to_string()).collect())
}

/// Print one of the `#` environment notes, once per process.
///
/// Resolving the resource module happens every time a platform stack is built,
/// and a `debug` session builds one at startup and again on every `run` — so the
/// same note would repeat once per `run` even though the answer cannot change
/// while the process lives. Each distinct note is printed the first time it
/// comes up, which is also the only time it carries information.
fn note_once(text: impl std::fmt::Display) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let text = text.to_string();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let mut seen = seen.lock().unwrap_or_else(|e| e.into_inner());
    if seen.insert(text.clone()) {
        eprintln!("{text}");
    }
}

/// What `@Compiled`, `@Unicode` and `@AutoItX64` answer for this input.
///
/// A build's own stub is the ground truth for `@AutoItX64`, so a `.exe`'s PE
/// machine comes first; the wrapper directive that asked for x64 is the next
/// best witness, and `None` leaves the answer to the platform, which reports
/// the emulated machine (`--win-arch`).
///
/// `#AutoIt3Wrapper_UseAnsi` is deliberately not honoured: AutoIt dropped the
/// ANSI interpreter in 3.3.14 and a modern wrapper skips the directive, so every
/// build that still carries it is Unicode whatever the line says. It is still
/// worth seeing, which is what [`note_wrapper_directives`] is for.
pub fn build_facts(program: &Program, build_is_x64: Option<bool>, compiled: bool) -> BuildFacts {
    BuildFacts {
        compiled,
        unicode: true,
        autoit_x64: build_is_x64.or_else(|| wrapper_flag(program, "UseX64")),
    }
}

/// A `#AutoIt3Wrapper_<name>=Y|N` setting, when the script carries one.
fn wrapper_flag(program: &Program, wanted: &str) -> Option<bool> {
    for (directive, argument) in autoitv3_preproc::directives(program) {
        let Some((name, value)) = wrapper_setting(directive) else {
            continue;
        };
        if name.eq_ignore_ascii_case(wanted) {
            return truthy_flag(if value.is_empty() { argument.trim() } else { value });
        }
    }
    None
}

/// Split one `#AutoIt3Wrapper_<name>=<value>` directive.
///
/// The wrapper writes `Name=value` with no space, so the generic directive
/// split — which cuts on whitespace, because `#include <file>` needs it to —
/// leaves the value stuck to the name. Both spellings are accepted here.
fn wrapper_setting(directive: &str) -> Option<(&str, &str)> {
    const PREFIX: &str = "AutoIt3Wrapper_";
    if !directive.get(..PREFIX.len())?.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let rest = &directive[PREFIX.len()..];
    Some(match rest.split_once('=') {
        Some((name, value)) => (name.trim(), value.trim()),
        None => (rest.trim(), ""),
    })
}

/// The spellings the wrapper accepts for `Y`/`N`.
fn truthy_flag(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" | "1" | "true" | "on" => Some(true),
        "n" | "no" | "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// Note the `#AutoIt3Wrapper_*` settings a script carries, once per process.
///
/// The wrapper consumed them at build time — that is where a build's resources,
/// version info, x64 stub and UPX packing came from — so nothing here acts on
/// them. They are the build's *fingerprint*, which is what an analyst wants to
/// see: which packer knobs produced the thing in hand.
fn note_wrapper_directives(program: &Program) {
    /// Longest setting shown before the line would stop being readable.
    const SHOWN: usize = 6;
    /// Longest argument kept per setting (paths can be long).
    const ARG: usize = 40;

    let mut settings: Vec<String> = Vec::new();
    for (directive, argument) in autoitv3_preproc::directives(program) {
        let Some((name, value)) = wrapper_setting(directive) else {
            continue;
        };
        // The real spelling keeps the wrapper's own capitalisation.
        let mut shown = name.to_string();
        let value = if value.is_empty() { argument } else { value };
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !value.is_empty() {
            let value = if value.chars().count() > ARG {
                value.chars().take(ARG).collect::<String>() + "…"
            } else {
                value
            };
            shown.push('=');
            shown.push_str(&value);
        }
        if !settings.contains(&shown) {
            settings.push(shown);
        }
    }
    if settings.is_empty() {
        return;
    }
    let count = settings.len();
    let hidden = count.saturating_sub(SHOWN);
    settings.truncate(SHOWN);
    if hidden > 0 {
        settings.push(format!("…(+{hidden})"));
    }
    note_once(msg!(
        "# AutoIt3Wrapper settings ({count}): {list}",
        count = count,
        list = settings.join(", ")
    ));
}

impl WinEmuArgs {
    /// Build the platform stack these arguments select.
    ///
    /// The environment is read first so `AU3_WIN_VERSION` keeps working, then
    /// the flags override it, so an explicit `--win-version` always wins.
    ///
    /// `script` is the file being analysed. It seeds the search for the PE
    /// image that answers `FindResourceW`: a script compiled into an `.exe`
    /// reads its payload and its string table out of that image's resources,
    /// and the image normally sits next to the script, so the default is to
    /// look there and then in the working directory. Passing
    /// `--resource-module` (or `AU3_RESOURCE_MODULE`) skips the search, and so
    /// does `input_module` — the build the script was just unpacked from.
    /// `gui` is the GUI backend to install on the emulation layer. `None` keeps
    /// the emulation's own — the host's native one on Windows, the headless
    /// default elsewhere — which is what `--gui auto` (the default) wants;
    /// `Some` replaces it, which is how every other mode is implemented. They
    /// pass a backend here rather than letting the emulation choose one because
    /// the choice is the CLI's: `--gui headless` must not draw even on a Windows
    /// host, `--gui native` asks for that same Win32 backend explicitly, and
    /// `--gui egui` draws through eframe wherever it runs.
    ///
    /// [`GuiBackend`]: autoitv3_platform::winemu::GuiBackend
    pub fn platform(
        &self,
        script: Option<&Path>,
        input_module: Option<&Path>,
        gui: Option<Box<dyn autoitv3_platform::winemu::GuiBackend>>,
        assume_admin: bool,
    ) -> CliResult<Box<dyn Platform>> {
        let mut emu = WindowsEmulation::from_env();
        if self.no_win_emu {
            emu = emu.disabled();
        }
        if self.no_win_drive_map {
            emu = emu.without_path_map();
        }
        if let Some(root) = &self.win_drive_map {
            if root.trim().is_empty() {
                emu = emu.without_path_map();
            } else {
                emu = emu.with_drive_root(root.trim());
            }
        }
        if let Some(script) = script {
            // `@ScriptDir`/`@ScriptName`/`@ScriptFullPath` describe the file
            // being analysed; the interpreter never tells the platform, so the
            // CLI hands it over here.
            emu = emu.with_script_path(script);
        }
        if let Some(raw) = &self.win_version {
            let version = WindowsVersion::from_name(raw).ok_or_else(|| {
                let shown = format!("{raw:?}");
                CliError::failure(msg!(
                    "unknown --win-version {shown} (try win7, win8, win81, win10, win11)",
                    shown = shown
                ))
            })?;
            emu = emu.with_version(version);
        }
        if let Some(raw) = &self.win_arch {
            let arch = WindowsArch::from_name(raw).ok_or_else(|| {
                let shown = format!("{raw:?}");
                CliError::failure(msg!(
                    "unknown --win-arch {shown} (try x86, x64, arm64)",
                    shown = shown
                ))
            })?;
            emu = emu.with_arch(arch);
        }
        // The image answers the resource chain on both hosts — the emulation
        // parses it with `PeImage` off Windows, the native layer maps it as a
        // resource-only image on it — so it is resolved for whoever is in the
        // stack rather than for the emulation alone: `--no-win-emu` still leaves
        // the native layer reading it.
        if let Some(path) = &self.resource_module {
            if !Path::new(path).is_file() {
                return Err(CliError::failure(msg!(
                    "--resource-module {path}: no such file",
                    path = path
                )));
            }
            emu = emu.with_module_file(path);
        } else if emu.module_path().is_none()
            && (!self.no_win_emu || autoitv3_platform::native_reads_resource_image())
        {
            if let Some(path) = input_module {
                // The input *was* the build, so its resources are the ones the
                // script reads — no sibling search needed.
                note_once(msg!(
                    "# resource module: {path} (the input build; override with --resource-module)",
                    path = path.display().to_string()
                ));
                emu = emu.with_module_file(path);
            } else if let Some(found) = find_resource_module(script) {
                note_once(msg!(
                    "# resource module: {path} (found next to the script; override with --resource-module)",
                    path = found.display().to_string()
                ));
                emu = emu.with_module_file(found);
            }
        }
        if !self.no_win_emu {
            // Resources already extracted next to the script answer before the
            // image does, so the payload alone is enough to analyse a build.
            let dirs = resource_search_dirs(script);
            if emu.module_path().is_none() && !has_staged_resources(&dirs) {
                note_once(tr(
                    "# no resource image or __* resource files found: resource calls may fail",
                ));
            }
            emu = emu.with_resource_dirs(dirs);
        }
        if let Some(backend) = gui {
            emu = emu.with_gui_backend(backend);
        }
        // `--emulate` routes the named areas to the emulation layer even where
        // a native implementation exists; everything else keeps native
        // semantics.
        let mut force_emulated = Vec::new();
        for area in &self.emulate {
            force_emulated.extend(expand_emulate_area(area)?);
        }
        Ok(autoitv3_platform::host_platform_with_options(PlatformOptions {
            emulation: emu,
            force_emulated,
            assume_admin,
        }))
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

/// A loaded input: the script's source and AST, however they were obtained.
pub struct Input {
    /// The script's text: the `.au3` file, or the source read back out of a
    /// compiled build.
    pub source: String,
    /// The parsed program.
    pub program: Program,
    /// The PE image the script's resources live in — the build itself when the
    /// input *was* a build; `None` for a plain `.au3`.
    pub resource_module: Option<PathBuf>,
    /// Whether that build is a 64-bit one, read from its PE header.
    ///
    /// A build's stub is the ground truth for `@AutoItX64`: it was compiled for
    /// one machine, and the script branched on that. `None` for a `.au3` input,
    /// where only the wrapper directive or the emulated machine can say.
    pub build_is_x64: Option<bool>,
}

/// Read and parse the AutoIt program at `path`.
///
/// `path` is either a `.au3` source file or a compiled build. A build is
/// recognised by its header — `MZ` for an image, `AU3!EA…` for a bare compiled
/// chunk — and its embedded script is read back with `autoitv3-unpack`. The
/// build then serves as the resource module, so the script's own
/// `FindResourceW` calls resolve without `--resource-module`.
///
/// `#include` is *not* expanded here: the commands that only read or rewrite a
/// file want the file, directives and all. See [`load_input_included`].
pub fn load_input(path: &str) -> CliResult<Input> {
    let bytes = std::fs::read(path)
        .map_err(|e| CliError::io(msg!("cannot read {path}: {e}", path = path, e = e)))?;
    if is_compiled_build(&bytes) {
        let input = load_compiled(path)?;
        note_wrapper_directives(&input.program);
        return Ok(input);
    }
    let source = autoitv3_preproc::decode(&bytes).ok_or_else(|| {
        CliError::io(msg!(
            "cannot read {path}: not UTF-8 or UTF-16 source, and not a compiled build \
             (no MZ / AU3!EA header)",
            path = path
        ))
    })?;
    let program = parse(&source)
        .map_err(|e| CliError::failure(msg!("parse error in {path}: {e}", path = path, e = e)))?;
    note_wrapper_directives(&program);
    Ok(Input {
        source,
        program,
        resource_module: None,
        build_is_x64: None,
    })
}

/// As [`load_input`], with every `#include` expanded into the program.
///
/// Only the *program* grows: [`Input::source`] stays the script as written, so
/// the debugger's listings and every line number of the script itself keep
/// meaning what they meant. What an included file said is a warning on the way
/// (a file that could not be found) or an error (a file that could not be
/// parsed), reported on stderr with the `#` prefix the other run notes use.
pub fn load_input_included(path: &str, includes: &IncludeArgs) -> CliResult<Input> {
    let mut input = load_input(path)?;
    if includes.no_includes {
        return Ok(input);
    }
    let mut search = autoitv3_preproc::Includes::from_env();
    for dir in &includes.include_paths {
        search.push_user_dir(PathBuf::from(dir));
    }
    let expansion = autoitv3_preproc::expand(input.program, Path::new(path), &search)
        .map_err(|e| CliError::failure(e.to_string()))?;
    for warning in &expansion.warnings {
        eprintln!("# {warning}");
    }
    if expansion.files.len() > 1 {
        eprintln!(
            "{}",
            msg!(
                "# #include: read {files} files ({included} included)",
                files = expansion.files.len(),
                included = expansion.files.len() - 1
            )
        );
    }
    input.program = expansion.program;
    Ok(input)
}

/// Read and parse the program at `path`, discarding the rest of the input.
pub fn load_program(path: &str) -> CliResult<Program> {
    load_input(path).map(|input| input.program)
}

/// Whether `bytes` are a compiled build rather than `.au3` source text.
fn is_compiled_build(bytes: &[u8]) -> bool {
    bytes.starts_with(b"MZ") || bytes.starts_with(b"AU3!EA")
}

/// Read the script a build carries and parse it.
fn load_compiled(path: &str) -> CliResult<Input> {
    let compiled = autoitv3_unpack::script::from_image(path)
        .map_err(|e| CliError::failure(format!("{path}: {e}")))?;
    let source = compiled
        .source()
        .map_err(|e| CliError::failure(format!("{path}: {e}")))?;
    eprintln!(
        "{}",
        msg!(
            "# input build: {path} (compiled script {version}, {files} embedded file(s))",
            path = path,
            version = compiled.version,
            files = compiled.files.len()
        )
    );
    let program = parse(&source).map_err(|e| {
        CliError::failure(msg!(
            "parse error in the script unpacked from {path}: {e}",
            path = path,
            e = e
        ))
    })?;
    Ok(Input {
        source,
        program,
        resource_module: Some(PathBuf::from(path)),
        // The stub's machine type, which is what this build's `@AutoItX64`
        // answered. A build whose headers cannot be read falls back to the
        // directive, then to the emulated machine.
        build_is_x64: autoitv3_platform::PeImage::load(path)
            .ok()
            .map(|image| image.is_x64()),
    })
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

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private build-detection predicate.
#[cfg(test)]
#[path = "../tests/unit/args.rs"]
mod tests;

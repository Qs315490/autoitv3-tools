//! `au3 run <FILE> [FUNC] [--arg V]... [--init] [--trace] [--gui MODE]` —
//! interpret.
//!
//! Without `FUNC` the whole script body is executed; with it, one function is
//! called on [`Runtime`](autoitv3_runtime::Runtime). This is how the
//! obfuscator's table builders can be probed directly, and `--trace` wires in a
//! [`Debugger`](autoitv3_runtime::debug::Debugger) to show the statement stream
//! the future debug module consumes.
//!
//! `--gui` picks what draws the GUI the script creates. The default, `auto`,
//! leaves the choice to the platform: on Windows the emulation drives real Win32
//! controls, so the script's window is a native one, while elsewhere nothing is
//! drawn. `--gui headless` forces the in-memory model on every host — the mode
//! to use when the GUI is only there to be analysed. `--gui window` (build with
//! `--features gui-window`) hands the emulation a backend that owns an eframe
//! window instead, so a GUI script can be seen on a host with no native path to
//! it; winit insists on the main thread, so that mode runs the script on a
//! worker driven by `LiveBackend::run` and blocks here until the window closes.

use autoitv3_platform::winemu::HeadlessBackend;
use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger, StopReason};
use autoitv3_runtime::profile::EffectKind;
use autoitv3_runtime::{Flow, Runtime, Value};
use clap::Args;

use crate::args::{
    CliError, CliResult, CompiledArgs, EffectArgs, GuiMode, IncludeArgs, ProfileArgs, StepArgs,
    WinEmuArgs, load_input_included, parse_arg_value,
};

use crate::output::format_value;
use std::path::Path;

/// Arguments for `au3 run`.
#[derive(Args, Debug, Clone)]
pub struct RunArgs {
    /// Input AutoIt v3 script, or a compiled build (.exe/.a3x) to read it from
    #[arg(value_name = "FILE")]
    pub input: String,

    /// Function to call; omitted runs the whole script body
    #[arg(value_name = "FUNC")]
    pub function: Option<String>,

    /// Script command-line argument (`$CmdLine`); repeat for more. Available in
    /// every mode, so a function call can still hand the script body a command
    /// line (`--init`).
    #[arg(long = "cmdline", value_name = "VALUE")]
    pub cmdline: Vec<String>,

    /// Argument for the function; repeat for more. Without a FUNC it is a
    /// command-line argument instead (same as `--cmdline`). Integers (decimal,
    /// or `0x` hex) become numbers, anything else a string.
    #[arg(long = "arg", value_name = "VALUE")]
    pub args: Vec<String>,

    /// When a FUNC is named, run the top-level script first so global tables
    /// (`$fn_table`, ...) exist before the call
    #[arg(long)]
    pub init: bool,

    /// Print the executed statement stream to stderr
    #[arg(long)]
    pub trace: bool,

    /// GUI backend: `auto` (the default) uses the platform's own — real Win32
    /// controls on Windows, nothing drawn elsewhere; `headless` never draws;
    /// `window` opens an eframe window (needs a build with the `gui-window`
    /// feature)
    #[arg(long = "gui", value_name = "MODE", default_value = "auto")]
    pub gui: GuiMode,

    /// Execution semantics (see `ProfileArgs`).
    #[command(flatten)]
    pub profile: ProfileArgs,

    /// Per-effect allow/deny overrides (see `EffectArgs`).
    #[command(flatten)]
    pub effects: EffectArgs,

    /// Interpreter step budget (see `StepArgs`).
    #[command(flatten)]
    pub steps: StepArgs,

    /// `@Compiled` selection (see `CompiledArgs`).
    #[command(flatten)]
    pub compiled: CompiledArgs,

    /// Ignore `#RequireAdmin`: run the script in this, unelevated, process
    ///
    /// The default is to honour it the way the interpreter does — start an
    /// elevated copy through the shell's `runas` verb and let that copy run the
    /// script. Pass this to analyse a script that asks for rights it does not
    /// get, or to avoid the UAC prompt in an unattended run.
    #[arg(long)]
    pub no_elevate: bool,

    /// (internal) Run as the elevated copy an `#RequireAdmin` run started
    ///
    /// Our own launcher passes this together with `--attach-console`; it says
    /// "the elevation already happened", so the directive is not acted on again
    /// and not reported as skipped either. Not meant to be used by hand.
    #[arg(long, hide = true)]
    pub elevated_copy: bool,

    /// `#include` search path (see `IncludeArgs`).
    #[command(flatten)]
    pub includes: IncludeArgs,

    #[command(flatten)]
    pub win: WinEmuArgs,
}

/// Entry point for the `run` subcommand.
pub fn run(args: &RunArgs) -> CliResult<()> {
    match args.gui {
        GuiMode::Auto => execute(args, None),
        GuiMode::Headless => execute(args, Some(Box::new(HeadlessBackend::new()))),
        GuiMode::Window => run_windowed(args),
    }
}

/// `--gui window`: open a real window and run the script under it.
///
/// The window owns the main thread (winit insists on it), so the script runs on
/// the worker `LiveBackend::run` starts; it returns once the script is done or
/// the user closes the window.
#[cfg(feature = "gui-window")]
fn run_windowed(args: &RunArgs) -> CliResult<()> {
    let title = Path::new(&args.input)
        .file_name()
        .map(|name| format!("au3 run — {}", name.to_string_lossy()))
        .unwrap_or_else(|| "au3 run".to_string());
    let owned = args.clone();
    autoitv3_gui_egui::LiveBackend::new(title)
        .run(move |backend| {
            if let Err(e) = execute(&owned, Some(Box::new(backend))) {
                eprintln!("error: {}", e.message);
            }
        })
        .map_err(|e| CliError::failure(format!("opening the GUI window failed: {e}")))
}

/// `--gui window` without the feature: say how to get it.
#[cfg(not(feature = "gui-window"))]
fn run_windowed(_args: &RunArgs) -> CliResult<()> {
    Err(CliError::failure(
        "--gui window needs a build with the `gui-window` feature \
         (cargo build --release -p au3-cli --features gui-window)",
    ))
}

/// Build the runtime and execute the script, optionally with a GUI backend.
fn execute(
    args: &RunArgs,
    gui: Option<Box<dyn autoitv3_platform::winemu::GuiBackend>>,
) -> CliResult<()> {
    let input = load_input_included(&args.input, &args.includes)?;
    let prog = input.program;
    // Probing a script wants reproducibility and no side effects; `--faithful`
    // switches to AutoIt's own semantics instead. `--allow`/`--deny` then
    // fine-tune individual effects on top of either preset.
    let profile = args.effects.apply(args.profile.profile())?;
    // `#RequireAdmin` is about the *process*, not the script: an unelevated run
    // starts an elevated copy of this program and stops here, before the first
    // statement (see `crate::elevate`). The preset profiles do not enter into
    // it — refusing the script's own `Run()` calls is a different question from
    // which token it runs with — but a user who said `--deny spawn` does, as
    // does `--no-elevate`.
    //
    // `--gui window` is the exception: the window lives in this process, so
    // there is nothing to hand over, and the directive is only reported.
    let spawn_denied = args
        .effects
        .deny
        .iter()
        .any(|kind| EffectKind::from_name(kind) == Some(EffectKind::Spawn));
    if args.gui == GuiMode::Window {
        if crate::elevate::is_required(&prog) {
            eprintln!(
                "note: #RequireAdmin: --gui window keeps this process, \
                 so the script runs without administrator rights"
            );
        }
    } else if crate::elevate::relaunch_if_required(
        &prog,
        args.elevated_copy,
        args.no_elevate,
        spawn_denied,
    )? {
        return Ok(());
    }
    // Install the platform layer for this OS so OS-specific builtins can be
    // reached (see `autoitv3-platform`); off Windows the Windows emulation
    // layer answers first, with the version these arguments select.
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(args.win.platform(
        Some(Path::new(&args.input)),
        input.resource_module.as_deref(),
        gui,
    )?);
    // A `.exe`/`.a3x` input is a compiled build, so `@Compiled` answers 1 the
    // way it did for the program the script came out of; `--compiled` /
    // `--no-compiled` override that when comparing a source against a build.
    rt.set_compiled(args.compiled.resolve(input.resource_module.is_some()));
    rt.set_max_steps(args.steps.max_steps);
    rt.set_profile(profile);

    if args.trace {
        rt.set_debugger(Box::new(TracePrinter::new()));
    }

    // No FUNC: run the top-level script body. `--cmdline` is the command line,
    // and `--arg` joins it (with no function to take it, it names the script's
    // argument instead). `--init` is meaningless here (there is nothing to
    // prepare for), so it is ignored.
    let Some(function) = &args.function else {
        let mut cmdline = args.cmdline.clone();
        cmdline.extend(args.args.iter().cloned());
        rt.set_cmdline(&cmdline);
        let outcome = rt.run_script();
        match &outcome {
            Ok(Flow::Return(value)) => println!("script body returned {}", format_value(value)),
            Ok(Flow::Exit(code)) => println!("script body exited with code {code}"),
            Ok(_) => println!("script body ran to completion"),
            Err(_) => {}
        }
        if let Some(reason) = rt.take_pause() {
            eprintln!("(stopped: {reason:?})");
        }
        return outcome
            .map(|_| ())
            .map_err(|e| CliError::failure(format!("error while running script body: {e}")));
    };

    // With a FUNC, `--cmdline` is still the script's command line (visible to
    // the body `--init` runs) while `--arg` belongs to the function.
    rt.set_cmdline(&args.cmdline);

    if args.init {
        // Execute the top-level script so the tables the function may rely on
        // exist before it runs.
        if let Err(e) = rt.run_script() {
            return Err(CliError::failure(format!(
                "error while running script body: {e}"
            )));
        }
    }

    let call_args: Vec<Value> = args.args.iter().map(|a| parse_arg_value(a)).collect();

    match rt.call_function(function, call_args) {
        Ok(value) => {
            println!("{function}() = {}", format_value(&value));
            if let Some(reason) = rt.take_pause() {
                eprintln!("(stopped: {reason:?})");
            }
            Ok(())
        }
        Err(e) => Err(CliError::failure(format!(
            "runtime error in {function}(): {e}"
        ))),
    }
}

/// A debugger that prints the statement stream to stderr.
///
/// A deliberately thin demonstration of the [`Debugger`] interface: the
/// interpreter hands it every statement span and it decides what to surface.
struct TracePrinter {
    statements: u64,
    calls: Vec<String>,
}

impl TracePrinter {
    fn new() -> Self {
        Self { statements: 0, calls: Vec::new() }
    }
}

impl Debugger for TracePrinter {
    fn on_statement(
        &mut self,
        span: autoitv3_ast::span::Span,
        depth: usize,
        _host: &mut dyn DebugHost,
    ) -> DebugAction {
        self.statements += 1;
        // Keep the output usable on a 23k-line script.
        if self.statements <= 40 {
            eprintln!(
                "[trace] {:>5}:{:<4} depth={depth}",
                span.start.line, span.start.col
            );
        } else if self.statements == 41 {
            eprintln!("[trace] ... (further statements suppressed)");
        }
        DebugAction::Continue
    }

    fn on_call_enter(&mut self, name: &str, args: &[Value]) {
        self.calls.push(name.to_string());
        if self.calls.len() <= 40 {
            eprintln!("[trace] call {name}({} args)", args.len());
        }
    }

    fn on_stop(&mut self, reason: &StopReason, _host: &mut dyn DebugHost) {
        eprintln!("[trace] stop: {reason:?}");
    }
}
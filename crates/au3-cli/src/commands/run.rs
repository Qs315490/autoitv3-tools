//! `au3 run <FILE> [FUNC] [--arg V]... [--init] [--trace] [--gui MODE]` —
//! interpret.
//!
//! Without `FUNC` the whole script body is executed; with it, one function is
//! called on [`Runtime`](autoitv3_runtime::Runtime). This is how the
//! obfuscator's table builders can be probed directly, and `--trace` wires in a
//! [`Debugger`](autoitv3_runtime::debug::Debugger) to show the statement stream
//! the future debug module consumes.
//!
//! `--gui window` (build with `--features gui-window`) hands the emulation a
//! backend that owns a real window instead of the headless model, so a GUI
//! script can actually be seen. winit insists on the main thread, so that mode
//! runs the script on a worker driven by `LiveBackend::run` and blocks here
//! until the window closes.

use autoitv3_runtime::debug::{DebugAction, DebugHost, Debugger, StopReason};
use autoitv3_runtime::{Flow, Runtime, Value};
use clap::Args;

use crate::args::{
    load_input, parse_arg_value, CliError, CliResult, CompiledArgs, EffectArgs, ProfileArgs,
    StepArgs, WinEmuArgs,
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

    /// GUI backend: `headless` answers the GUI functions without drawing
    /// anything; `window` opens a real window (needs a build with the
    /// `gui-window` feature)
    #[arg(long = "gui", value_name = "MODE", default_value = "headless")]
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

    #[command(flatten)]
    pub win: WinEmuArgs,
}

/// How the emulated GUI is presented while the script runs.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GuiMode {
    /// The in-memory model: GUI calls return their real results and nothing is
    /// drawn (the default, and the only mode that works without a display).
    #[default]
    Headless,
    /// A real window driven by `autoitv3_gui_egui::LiveBackend`.
    Window,
}

/// Entry point for the `run` subcommand.
pub fn run(args: &RunArgs) -> CliResult<()> {
    match args.gui {
        GuiMode::Headless => execute(args, None),
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
    let input = load_input(&args.input)?;
    let prog = input.program;
    // Install the platform layer for this OS so OS-specific builtins can be
    // reached (see `autoitv3-platform`); off Windows the Windows emulation
    // layer answers first, with the version these arguments select.
    let mut rt = Runtime::with_program(&prog);
    rt.set_platform(args.win.platform_with_gui(
        Some(Path::new(&args.input)),
        input.resource_module.as_deref(),
        gui,
    )?);
    // A `.exe`/`.a3x` input is a compiled build, so `@Compiled` answers 1 the
    // way it did for the program the script came out of; `--compiled` /
    // `--no-compiled` override that when comparing a source against a build.
    rt.set_compiled(args.compiled.resolve(input.resource_module.is_some()));
    rt.set_max_steps(args.steps.max_steps);
    // Probing a script wants reproducibility and no side effects; `--faithful`
    // switches to AutoIt's own semantics instead. `--allow`/`--deny` then
    // fine-tune individual effects on top of either preset.
    rt.set_profile(args.effects.apply(args.profile.profile())?);

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
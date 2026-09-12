//! Live-window example: a Label, an Input and a Button, with clicks fed back
//! into `GUIGetMsg` and typed text read back by `GUICtrlRead`.
//!
//! Needs a display server. Three modes:
//!
//! ```bash
//! cargo run -p autoitv3-gui-egui --features window --example live
//! ```
//!
//! The default script is interactive: click **Greet** to print
//! `Hello, <typed name>!` on stdout, or **Minimise** to see what
//! `@SW_MINIMIZE` does. By default the window leaves the screen, the way
//! Windows hides it, and a strip along the bottom of the viewport offers the
//! way back; pass `--titlebar` to keep the title bar on screen instead (then
//! double-click it, or use its buttons, to restore).
//!
//! ```bash
//! cargo run -p autoitv3-gui-egui --features window --example live -- --states
//! ```
//!
//! `--states` runs a tour of the geometry and state a *script* can drive:
//! `WinMove` and `WinSetState(@SW_MAXIMIZE/@SW_RESTORE/@SW_MINIMIZE)`, printing
//! what `WinGetPos`/`WinGetState` report at each step. Maximising takes the
//! desktop's rectangle — the native window is the emulated desktop, so
//! `WinGetPos` then reports its size and `@SW_RESTORE` gives the window back. It then stays open so the
//! window controls and the double-click can be tried; the console prints what
//! the script hears when the user does that.
//!
//! ```bash
//! cargo run -p autoitv3-gui-egui --features window --example live -- --auto
//! ```
//!
//! `--auto` runs a script that creates the controls and returns at once: the
//! window then closes itself, which is handy for checking the plumbing without
//! a user.
//!
//! Note the shape of `main`: `LiveBackend::run` must own the main thread
//! because winit insists on creating the event loop there. It starts the script
//! on a worker thread and blocks until the window closes.

use autoitv3_gui_egui::{LiveBackend, MinimizeStyle};
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_runtime::Runtime;

/// `--auto`: the controls, then return immediately.
const CONTROLS: &str = r#"
GUICreate("AutoIt PoC", 380, 170)
GUICtrlCreateLabel("Type a name, then press Greet:", 12, 12)
$edit = GUICtrlCreateInput("world", 12, 36, 220, 24)
$btn = GUICtrlCreateButton("Greet", 12, 74, 110, 30)
GUICtrlCreateButton("Minimise", 132, 74, 110, 30)
GUISetState()
"#;

/// The default: interactive, with a message loop.
const SCRIPT: &str = r#"
GUICreate("AutoIt PoC", 380, 170)
GUICtrlCreateLabel("Type a name, then press Greet:", 12, 12)
$edit = GUICtrlCreateInput("world", 12, 36, 220, 24)
$btn = GUICtrlCreateButton("Greet", 12, 74, 110, 30)
$min = GUICtrlCreateButton("Minimise", 132, 74, 110, 30)
GUISetState()
While 1
    $msg = GUIGetMsg()
    If $msg = -3 Then ExitLoop
    If $msg = $btn Then ConsoleWrite("Hello, " & GUICtrlRead($edit) & "!" & @CRLF)
    If $msg = $min Then WinSetState("AutoIt PoC", "", @SW_MINIMIZE)
    If $msg = -5 Then ConsoleWrite("[the user restored the window]" & @CRLF)
    Sleep(10)
WEnd
"#;

/// `--states`: what a script can do to its own window, narrated on stdout.
const STATES: &str = r#"
Global $title = "AutoIt states"

Func Report($step)
    Local $p = WinGetPos($title)
    ConsoleWrite($step & ": pos " & $p[0] & "," & $p[1] & " size " & $p[2] & "x" & $p[3] & " state " & WinGetState($title) & @CRLF)
EndFunc

; The native window is the emulated desktop, so this is its size.
ConsoleWrite("desktop (the parent window): " & @DesktopWidth & "x" & @DesktopHeight & @CRLF)

GUICreate($title, 380, 170)
GUICtrlCreateLabel("The script is driving this window.", 12, 12)
GUICtrlCreateButton("Watch, then try the title bar", 12, 74, 220, 30)
GUISetState()
Report("created")

Sleep(900)
WinMove($title, "", 220, 140, 520, 300)
Report("WinMove")

Sleep(900)
WinSetState($title, "", @SW_MAXIMIZE)
Report("maximise")

Sleep(900)
WinSetState($title, "", @SW_RESTORE)
Report("restore size")

Sleep(900)
WinSetState($title, "", @SW_MINIMIZE)
Report("minimise")

Sleep(1500)
WinSetState($title, "", @SW_RESTORE)
Report("restore")

ConsoleWrite("now try the title bar: buttons, or a double-click" & @CRLF)
While 1
    $msg = GUIGetMsg()
    If $msg = -3 Then ExitLoop
    If $msg = -4 Then ConsoleWrite("[the user minimised the window]" & @CRLF)
    If $msg = -5 Then ConsoleWrite("[the user restored the window]" & @CRLF)
    If $msg = -6 Then ConsoleWrite("[the user maximised the window]" & @CRLF)
    Sleep(10)
WEnd
"#;

/// What the example was asked to run.
enum Mode {
    Interactive,
    Auto,
    States,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = if args.iter().any(|arg| arg == "--auto") {
        Mode::Auto
    } else if args.iter().any(|arg| arg == "--states") {
        Mode::States
    } else {
        Mode::Interactive
    };
    let title_bar_minimize = args.iter().any(|arg| arg == "--titlebar");
    let source = match mode {
        Mode::Auto => CONTROLS,
        Mode::States => STATES,
        Mode::Interactive => SCRIPT,
    };

    let mut backend = LiveBackend::new("AutoIt GUI PoC");
    if title_bar_minimize {
        // Keep the title bar on screen when the script minimises the window.
        backend = backend.with_minimize_style(MinimizeStyle::TitleBar);
    }
    backend
        .run(move |backend| {
            let emulation = WindowsEmulation::new().with_gui_backend(Box::new(backend));
            let program = autoitv3_ast::parse(source).expect("PoC script parses");
            let mut runtime = Runtime::with_program(&program);
            runtime.set_platform(autoitv3_platform::host_platform_with(emulation));
            if let Err(error) = runtime.run_script() {
                eprintln!("[live] script stopped: {error}");
            }
        })
        .expect("the window loop failed to start");

    println!("window closed; exiting");
}

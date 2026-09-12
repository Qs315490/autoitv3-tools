//! Live-window example: a Label, an Input and a Button, with clicks fed back
//! into `GUIGetMsg` and typed text read back by `GUICtrlRead`.
//!
//! Needs a display server. Run it with:
//!
//! ```bash
//! cargo run -p autoitv3-gui-egui --features window --example live
//! ```
//!
//! Click **Greet** to print `Hello, <typed name>!` on stdout; close the window
//! (or its X) to let the script's `GUIGetMsg` loop exit.
//!
//! Press **Minimise** to see what `@SW_MINIMIZE` does. By default the window
//! leaves the screen, the way Windows hides it, and a strip along the bottom of
//! the viewport offers the way back. Pass `--titlebar` to keep the title bar on
//! screen instead and double-click it to restore.
//!
//! Pass `--auto` to run a script that creates the controls and returns at once:
//! the window then closes itself, which is handy for checking the plumbing
//! without a user.
//!
//! Note the shape of `main`: `LiveBackend::run` must own the main thread
//! because winit insists on creating the event loop there. It starts the script
//! on a worker thread and blocks until the window closes.

use autoitv3_gui_egui::{LiveBackend, MinimizeStyle};
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_runtime::Runtime;

const CONTROLS: &str = r#"
GUICreate("AutoIt PoC", 380, 170)
GUICtrlCreateLabel("Type a name, then press Greet:", 12, 12)
$edit = GUICtrlCreateInput("world", 12, 36, 220, 24)
$btn = GUICtrlCreateButton("Greet", 12, 74, 110, 30)
GUICtrlCreateButton("Minimise", 132, 74, 110, 30)
GUISetState()
"#;

const SCRIPT: &str = r#"
GUICreate("AutoIt PoC", 380, 170)
GUICtrlCreateLabel("Type a name, then press Greet:", 12, 12)
$edit = GUICtrlCreateInput("world", 12, 36, 220, 24)
$btn = GUICtrlCreateButton("Greet", 12, 74, 110, 30)
GUISetState()
Sleep(1000)
WinMove("AutoIt PoC", "", 200, 150, 500, 300)   ; 窗口真的过去并变大
Sleep(1000)
WinSetState("AutoIt PoC", "", @SW_MAXIMIZE)     ; 铺满
Sleep(1000)
WinSetState("AutoIt PoC", "", @SW_MINIMIZE)     ; 消失（脚本仍持有）
Sleep(1000)
WinSetState("AutoIt PoC", "", @SW_RESTORE)      ; 回来
While 1
    $msg = GUIGetMsg()
    If $msg = -3 Then ExitLoop
    If $msg = $btn Then ConsoleWrite("Hello, " & GUICtrlRead($edit) & "!" & @CRLF)
    Sleep(10)
WEnd
"#;

fn main() {
    let mut args = std::env::args().skip(1);
    let auto = args.any(|arg| arg == "--auto");
    let title_bar_minimize = std::env::args().any(|arg| arg == "--titlebar");
    let source = if auto { CONTROLS } else { SCRIPT };

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

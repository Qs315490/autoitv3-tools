//! Live-window PoC: a Label, an Input and a Button, with clicks fed back into
//! `GUIGetMsg` and typed text read back by `GUICtrlRead`.
//!
//! Needs a display server. Run it with:
//!
//! ```bash
//! cargo run -p autoitv3-gui-egui --features window --example live
//! ```
//!
//! Click **Greet** to print `Hello, <typed name>!` on stdout; close the window
//! (or its X) to let the script's `GUIGetMsg` loop exit.

use autoitv3_gui_egui::LiveBackend;
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_runtime::Runtime;

const SCRIPT: &str = r#"
GUICreate("AutoIt PoC", 380, 170)
GUICtrlCreateLabel("Type a name, then press Greet:", 12, 12)
$edit = GUICtrlCreateInput("world", 12, 36, 220, 24)
$btn = GUICtrlCreateButton("Greet", 12, 74, 110, 30)
GUISetState()
While 1
    $msg = GUIGetMsg()
    If $msg = -3 Then ExitLoop
    If $msg = $btn Then ConsoleWrite("Hello, " & GUICtrlRead($edit) & "!" & @CRLF)
    Sleep(10)
WEnd
"#;

fn main() {
    let backend = LiveBackend::new("AutoIt GUI PoC");
    let emulation = WindowsEmulation::new().with_gui_backend(Box::new(backend));

    let program = autoitv3_ast::parse(SCRIPT).expect("PoC script parses");
    let mut runtime = Runtime::with_program(&program);
    runtime.set_platform(autoitv3_platform::host_platform_with(emulation));
    runtime.run_script().expect("PoC script runs");

    println!("window closed; exiting");
}

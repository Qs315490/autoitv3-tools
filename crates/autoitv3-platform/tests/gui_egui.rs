//! End-to-end: an AutoIt script's controls reach the egui renderer.
//!
//! Only built with the `gui-egui-offscreen` feature. The emulation owns its
//! backend, so the test wraps [`EguiBackend`] in a shared handle it can still
//! snapshot after the run.

#![cfg(all(not(windows), feature = "gui-egui-offscreen"))]

use std::cell::RefCell;
use std::rc::Rc;

use autoitv3_gui_model::{Control, GuiBackend, GuiImage, Window};
use autoitv3_gui_egui::EguiBackend;
use autoitv3_platform::host_platform_with;
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_runtime::Runtime;

/// A backend the test keeps a handle to after handing it to the emulation.
#[derive(Clone)]
struct Shared(Rc<RefCell<EguiBackend>>);

impl GuiBackend for Shared {
    fn on_window(&mut self, window: &Window) {
        self.0.borrow_mut().on_window(window);
    }
    fn on_window_removed(&mut self, handle: i64) {
        self.0.borrow_mut().on_window_removed(handle);
    }
    fn on_control(&mut self, control: &Control) {
        self.0.borrow_mut().on_control(control);
    }
    fn on_control_removed(&mut self, id: i64) {
        self.0.borrow_mut().on_control_removed(id);
    }
    fn present(&mut self) {
        self.0.borrow_mut().present();
    }
    fn snapshot(&mut self) -> Option<GuiImage> {
        self.0.borrow_mut().snapshot()
    }
    fn desktop_size(&self) -> Option<(i32, i32)> {
        self.0.borrow().desktop_size()
    }
}

/// One call per control kind the emulation implements, plus a menu.
const SCRIPT: &str = r#"
GUICreate("Full control set", 600, 960)
GUICtrlCreateLabel("Label", 10, 10, 200, 20)
$button = GUICtrlCreateButton("Button", 10, 40, 200, 28)
$input = GUICtrlCreateInput("typed", 10, 80, 200, 24)
$edit = GUICtrlCreateEdit("line", 10, 110, 200, 40)
$check = GUICtrlCreateCheckbox("Check", 10, 160, 200, 20)
$radio = GUICtrlCreateRadio("Radio", 10, 190, 200, 20)
$group = GUICtrlCreateGroup("Group", 10, 220, 200, 40)
$list = GUICtrlCreateList("", 10, 270, 200, 60)
GUICtrlSetData($list, "one|two|three")
$combo = GUICtrlCreateCombo("", 10, 340, 200, 100)
GUICtrlSetData($combo, "alpha|beta")
$listview = GUICtrlCreateListView("name|value", 10, 380, 240, 60)
GUICtrlCreateListViewItem("a|1", $listview)
$treeview = GUICtrlCreateTreeView(10, 450, 200, 60)
GUICtrlCreateTreeViewItem("root", $treeview)
$tab = GUICtrlCreateTab(10, 520, 240, 40)
GUICtrlCreateTabItem("First")
GUICtrlCreateMenu("File")
GUICtrlCreateMenuItem("Open")
GUICtrlCreateContextMenu()
GUICtrlCreatePic("splash.bmp", 10, 570, 60, 40)
GUICtrlCreateIcon("icon.ico", 80, 570, 32, 32)
$graphic = GUICtrlCreateGraphic(10, 620, 120, 60)
GUICtrlSetGraphic($graphic, 1, 0xFF0000)
GUICtrlSetGraphic($graphic, 0, 0, 0)
GUICtrlSetGraphic($graphic, 2, 100, 50)
GUICtrlSetGraphic($graphic, 6, 4, 4, 40, 20)
GUICtrlSetGraphic($graphic, 7, 30, 10, 20, 20)
GUICtrlCreateProgress(10, 690, 200, 20)
$slider = GUICtrlCreateSlider(10, 720, 200, 24)
$updown = GUICtrlCreateUpdown(10, 750, 60, 24)
$date = GUICtrlCreateDate("2024/01/02", 10, 780, 200, 24)
$monthcal = GUICtrlCreateMonthCal("2024/01/01", 230, 690, 200, 160)
GUICtrlCreateDummy()
GUICtrlCreateAvi("clip.avi", 0, 10, 820, 60, 30)
GUICtrlCreateObj(0, 80, 820, 60, 30)
GUISetState()
"#;

#[test]
fn a_scripts_whole_control_set_reaches_the_renderer() {
    let backend = Shared(Rc::new(RefCell::new(
        EguiBackend::new().with_size(640, 1020),
    )));
    let emulation = WindowsEmulation::new().with_gui_backend(Box::new(backend.clone()));

    let program = autoitv3_ast::parse(SCRIPT).expect("script parses");
    let mut runtime = Runtime::with_program(&program);
    runtime.set_platform(host_platform_with(emulation));
    runtime.run_script().expect("script runs");

    let mut backend = backend.0.borrow_mut();
    assert_eq!(backend.window_count(), 1, "one window reached the renderer");
    assert_eq!(
        backend.control_count(),
        29,
        "every control kind reached the renderer"
    );

    let image = backend.snapshot().expect("renders");
    let painted = image.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(
        painted > 100_000,
        "the rendered window is nearly empty: {painted} opaque px"
    );
}

/// Render the frame a script leaves behind.
fn render(script: &str) -> GuiImage {
    let backend = Shared(Rc::new(RefCell::new(
        EguiBackend::new().with_size(640, 480),
    )));
    let emulation = WindowsEmulation::new().with_gui_backend(Box::new(backend.clone()));
    let program = autoitv3_ast::parse(script).expect("script parses");
    let mut runtime = Runtime::with_program(&program);
    runtime.set_platform(host_platform_with(emulation));
    runtime.run_script().expect("script runs");
    let image = backend.0.borrow_mut().snapshot().expect("renders");
    image
}

fn ink(image: &GuiImage) -> usize {
    image.rgba.chunks_exact(4).filter(|p| p[3] > 0).count()
}

#[test]
fn window_state_and_moves_reach_the_pixels() {
    const OPEN: &str = r#"
GUICreate("State", 200, 120, 40, 30)
GUICtrlCreateLabel("hi", 10, 10)
GUISetState()
"#;

    let normal = ink(&render(OPEN));
    assert!(normal > 0, "a shown window draws something");

    // @SW_MINIMIZE: AutoIt keeps the window, the screen does not show it.
    let minimized = ink(&render(&format!("{OPEN}\nWinSetState(\"State\", \"\", @SW_MINIMIZE)")));
    assert_eq!(minimized, 0, "a minimised window is off screen");

    // @SW_MAXIMIZE fills the viewport; @SW_RESTORE brings the size back.
    let maximized = ink(&render(&format!("{OPEN}\nWinSetState(\"State\", \"\", @SW_MAXIMIZE)")));
    assert!(
        maximized > normal,
        "a maximised window covers more: {normal} -> {maximized}"
    );

    // WinMove moves it, so the same content lands somewhere else.
    let moved = render(&format!("{OPEN}\nWinMove(\"State\", \"\", 300, 200)"));
    assert_eq!(ink(&moved), normal, "moving does not change how much is drawn");
    assert_ne!(moved.rgba, render(OPEN).rgba, "WinMove changed nothing");
}

/// Run a script body and read back what it returns.
fn value(body: &str) -> String {
    let source = format!("Func F()\n{body}\nEndFunc\n");
    let program = autoitv3_ast::parse(&source).expect("script parses");
    let mut runtime = Runtime::with_program(&program);
    let backend = Shared(Rc::new(RefCell::new(
        EguiBackend::new().with_size(640, 480),
    )));
    let emulation = WindowsEmulation::new().with_gui_backend(Box::new(backend));
    runtime.set_platform(host_platform_with(emulation));
    runtime
        .call_function("F", vec![])
        .expect("script runs")
        .to_autoit_string()
}

#[test]
fn the_offscreen_canvas_is_the_desktop() {
    // The canvas stands in for the desktop here, exactly as the native viewport
    // does in a live window, so a maximised window takes the whole canvas.
    let answer = value(
        r#"
GUICreate("T", 200, 120)
GUISetState()
WinSetState("T", "", @SW_MAXIMIZE)
Local $p = WinGetPos("T")
Return $p[0] & "," & $p[1] & " " & $p[2] & "x" & $p[3] & " desktop " & @DesktopWidth & "x" & @DesktopHeight
"#,
    );
    assert_eq!(answer, "0,0 640x480 desktop 640x480");
}

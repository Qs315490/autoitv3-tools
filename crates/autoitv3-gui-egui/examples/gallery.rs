//! Render a window with every control kind and write it to a PNG.
//!
//! This is the visual companion to `tests/controls.rs`: it runs a real script
//! through the emulation and the offscreen egui renderer, so what you see is
//! exactly what the widget layer produces (no display server, no GPU).
//!
//! ```bash
//! cargo run -p autoitv3-gui-egui --features egui --example gallery
//! # writes /tmp/au3-gallery.png (override with the first argument)
//! ```

use autoitv3_gui_egui::EguiBackend;
use autoitv3_platform::winemu::WindowsEmulation;
use autoitv3_runtime::Runtime;

const SCRIPT: &str = r#"
GUICreate("AutoIt control gallery", 600, 960)
GUICtrlCreateLabel("Label: a plain text control", 10, 10, 260, 20)
GUICtrlCreateButton("Button", 10, 40, 200, 28)
GUICtrlCreateInput("Input text", 10, 80, 200, 24)
GUICtrlCreateEdit("Edit text, multiple lines" & @CRLF & "second line", 10, 110, 260, 44)
GUICtrlCreateCheckbox("Checkbox", 10, 164, 200, 20)
GUICtrlCreateRadio("Radio", 10, 190, 200, 20)
GUICtrlCreateGroup("Group", 10, 216, 200, 40)
$list = GUICtrlCreateList("", 10, 266, 200, 60)
GUICtrlSetData($list, "one|two|three")
$combo = GUICtrlCreateCombo("", 240, 266, 180, 100)
GUICtrlSetData($combo, "alpha|beta|gamma")
$listview = GUICtrlCreateListView("name|value", 10, 336, 260, 60)
GUICtrlCreateListViewItem("alpha|1", $listview)
GUICtrlCreateListViewItem("beta|2", $listview)
$treeview = GUICtrlCreateTreeView(10, 406, 200, 60)
GUICtrlCreateTreeViewItem("root", $treeview)
GUICtrlCreateTreeViewItem("  child", $treeview)
GUICtrlCreateTab(10, 476, 260, 30)
GUICtrlCreateTabItem("First tab")
$menu = GUICtrlCreateMenu("File")
GUICtrlCreateMenuItem("Open", $menu)
GUICtrlCreateContextMenu()
GUICtrlCreatePic("splash.bmp", 10, 516, 80, 48)
GUICtrlCreateIcon("icon.ico", 100, 516, 32, 32)
$graphic = GUICtrlCreateGraphic(10, 574, 180, 80)
GUICtrlSetGraphic($graphic, 1, 0x00A5FF)
GUICtrlSetGraphic($graphic, 0, 0, 0)
GUICtrlSetGraphic($graphic, 2, 170, 70)
GUICtrlSetGraphic($graphic, 6, 10, 10, 60, 30)
GUICtrlSetGraphic($graphic, 7, 60, 20, 40, 40)
GUICtrlCreateProgress(10, 664, 200, 20)
$slider = GUICtrlCreateSlider(10, 694, 200, 24)
$updown = GUICtrlCreateUpdown(10, 724, 60, 24)
GUICtrlCreateDate("2024/01/02", 10, 754, 200, 24)
GUICtrlCreateMonthCal("2024/01/01", 240, 450, 200, 160)
GUICtrlCreateDummy()
GUICtrlCreateAvi("clip.avi", 0, 10, 790, 80, 40)
GUICtrlCreateObj(0, 100, 790, 80, 40)
GUISetState()
"#;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/au3-gallery.png".to_string());

    // Drawing a 600×960 window on a slightly larger canvas.
    let backend = EguiBackend::new()
        .with_size(640, 1020)
        .with_screenshot(&path);
    let emulation = WindowsEmulation::new().with_gui_backend(Box::new(backend));

    let program = autoitv3_ast::parse(SCRIPT).expect("gallery script parses");
    let mut runtime = Runtime::with_program(&program);
    runtime.set_platform(autoitv3_platform::host_platform_with(emulation));
    runtime.run_script().expect("gallery script runs");

    println!("wrote {path}");
}

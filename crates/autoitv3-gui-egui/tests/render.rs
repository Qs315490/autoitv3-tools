//! Offscreen rendering tests. Only built with the `egui` feature.

#![cfg(feature = "egui")]

use autoitv3_gui_model::{Control, ControlKind, GuiBackend, Window};
use autoitv3_gui_egui::EguiBackend;

fn demo(backend: &mut EguiBackend) {
    let handle = 0x1_0000;
    let mut window = Window::new(handle, "Demo", 200, 100);
    window.controls.push(1);
    window.controls.push(2);
    backend.on_window(&window);

    let mut label = Control::new(1, handle, ControlKind::Label);
    label.text = "Hello".to_string();
    backend.on_control(&label);

    let mut button = Control::new(2, handle, ControlKind::Button);
    button.text = "Click me".to_string();
    backend.on_control(&button);
}

#[test]
fn renders_a_window_and_writes_a_png() {
    let mut backend = EguiBackend::new().with_size(320, 200);
    demo(&mut backend);

    let image = backend.snapshot().expect("renders");
    assert_eq!((image.width, image.height), (320, 200));
    assert_eq!(image.rgba.len(), 320 * 200 * 4);
    let opaque = image.rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
    assert!(
        opaque > 200,
        "expected a visible window, got {opaque} opaque px"
    );

    let path = std::env::temp_dir().join(format!("au3-gui-egui-{}.png", std::process::id()));
    backend.screenshot(&path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn hidden_windows_render_nothing_and_output_is_deterministic() {
    let mut a = EguiBackend::new().with_size(160, 120);
    let mut b = EguiBackend::new().with_size(160, 120);
    demo(&mut a);
    demo(&mut b);
    assert_eq!(a.snapshot().unwrap().rgba, b.snapshot().unwrap().rgba);

    // A window that has not been shown produces no opaque pixels.
    let mut hidden = EguiBackend::new().with_size(64, 64);
    let mut window = Window::new(0x1_0000, "Hidden", 50, 40);
    window.visible = false;
    hidden.on_window(&window);
    let image = hidden.snapshot().unwrap();
    assert!(
        image.rgba.chunks_exact(4).all(|p| p[3] == 0),
        "hidden window drew something"
    );
}

//! Live-backend plumbing tests. They never open a window: `present()` is what
//! starts eframe, and these exercise the mirror/queue the window relies on.

#![cfg(feature = "window")]

use autoitv3_gui::{Control, ControlKind, GuiBackend, GuiEvent, GuiUpdate, Window};
use autoitv3_gui_egui::LiveBackend;

#[test]
fn live_backend_mirrors_the_model_and_queues_events() {
    let mut backend = LiveBackend::new("PoC");

    let mut window = Window::new(1, "W", 100, 100);
    window.controls.push(7);
    backend.on_window(&window);
    assert_eq!(backend.window_count(), 1);

    let mut button = Control::new(7, 1, ControlKind::Button);
    button.text = "Greet".to_string();
    backend.on_control(&button);
    assert_eq!(backend.control_text(7).as_deref(), Some("Greet"));

    // What the window sends when the button is clicked.
    backend.queue(GuiEvent::Control(7));
    assert_eq!(backend.poll(), vec![GuiEvent::Control(7)]);
    assert!(backend.poll().is_empty(), "the event should be drained once");

    // And what it sends when the user types.
    backend
        .take_updates()
        .push(GuiUpdate::SetText { id: 7, text: "x".into() });
    // (no window, so nothing was queued by the app itself)
    assert_eq!(backend.poll(), Vec::new());
}

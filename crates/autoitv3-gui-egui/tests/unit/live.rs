//! Unit tests for `live`'s real-window backend (feature `window`).
//!
//! Kept out of `live.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;
use autoitv3_gui_model::ControlKind;

fn input(id: i64, text: &str) -> Control {
    let mut control = Control::new(id, 1, ControlKind::Input);
    control.text = text.to_string();
    control
}

#[test]
fn an_in_flight_edit_is_shown_until_the_script_echoes_it() {
    let mut controls = BTreeMap::new();
    controls.insert(1, input(1, "world"));

    let mut pending = Pending::new();
    pending.insert(1, ("world!".to_string(), PENDING_FRAMES));

    // The window draws what the user typed, not the model's stale text.
    let mut body = vec![input(1, "world")];
    overlay_pending(&mut body, &pending);
    assert_eq!(body[0].text, "world!");

    // The script has not applied it yet, so it stays pending.
    expire_pending(&mut pending, &controls);
    assert_eq!(pending.len(), 1);

    // Once GUICtrlRead's write-back reaches the model, the overlay is gone.
    controls.insert(1, input(1, "world!"));
    expire_pending(&mut pending, &controls);
    assert!(pending.is_empty());
}

#[test]
fn the_viewport_is_the_desktop() {
    let backend = LiveBackend::new("W");
    // Before the first frame it reports the size the viewport was asked to
    // open at, so a script that reads the desktop early sees no change.
    assert_eq!(
        backend.desktop_size(),
        Some(autoitv3_gui_model::DEFAULT_DESKTOP_SIZE)
    );

    backend.shared.set_desktop(egui::vec2(1024.0, 640.0));
    assert_eq!(backend.desktop_size(), Some((1024, 640)));

    // A degenerate size is not a desktop.
    backend.shared.set_desktop(egui::vec2(0.0, 0.0));
    assert_eq!(backend.desktop_size(), Some((1024, 640)));
}

#[test]
fn the_window_keeps_its_context_so_the_script_can_wake_it() {
    let backend = LiveBackend::new("W");
    // Before the first frame there is nothing to wake: a no-op on the
    // script thread, not a panic.
    backend.shared.wake();
    assert!(backend.shared.ctx.lock().unwrap().is_none());

    let ctx = egui::Context::default();
    backend.shared.set_context(&ctx);
    assert!(
        backend.shared.ctx.lock().unwrap().is_some(),
        "the frame must hand the script thread a context to wake"
    );
    // Every script → screen change goes through `on_window`, and waking a
    // stored context is what puts it on screen without waiting for the poll.
    backend.shared.wake();
    let mut script = backend.clone();
    script.on_window(&Window::new(1, "T", 100, 80));
    assert!(
        ctx.has_requested_repaint(),
        "the change should ask for a frame"
    );
}

#[test]
fn an_edit_the_script_never_echoes_expires() {
    let mut controls = BTreeMap::new();
    controls.insert(1, input(1, "world"));

    let mut pending = Pending::new();
    pending.insert(1, ("typed".to_string(), 2));
    expire_pending(&mut pending, &controls);
    assert_eq!(pending.len(), 1, "still fresh");
    expire_pending(&mut pending, &controls);
    assert!(
        pending.is_empty(),
        "the script rewrote it to something else"
    );
}

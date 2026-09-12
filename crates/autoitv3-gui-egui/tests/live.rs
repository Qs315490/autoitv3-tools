//! Live-backend plumbing tests. They never open a window: `present()` is what
//! starts eframe, and these exercise the mirror/queue the window relies on.

#![cfg(feature = "window")]

use autoitv3_gui_model::{Control, ControlKind, GuiBackend, GuiEvent, GuiUpdate, Window};
use autoitv3_gui_egui::{Action, Interaction, LiveBackend};

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
    assert!(
        backend.poll().is_empty(),
        "the event should be drained once"
    );

    // And what it sends when the user types.
    backend.take_updates().push(GuiUpdate::SetText {
        id: 7,
        text: "x".into(),
    });
    // (no window, so nothing was queued by the app itself)
    assert_eq!(backend.poll(), Vec::new());
}

#[test]
fn widget_interactions_become_events_and_updates() {
    let mut backend = LiveBackend::new("Interactions");

    // A click on a Button and a chosen MenuItem are both messages.
    backend.simulate(Interaction {
        id: 7,
        action: Action::Clicked,
    });
    backend.simulate(Interaction {
        id: 9,
        action: Action::Menu,
    });
    assert_eq!(
        backend.poll(),
        vec![GuiEvent::Control(7), GuiEvent::Menu(9)],
        "clicks and menu picks must reach GUIGetMsg in order"
    );

    // Typing, toggling and selecting are model changes.
    backend.simulate(Interaction {
        id: 7,
        action: Action::Text("hi".to_string()),
    });
    backend.simulate(Interaction {
        id: 8,
        action: Action::Checked(true),
    });
    backend.simulate(Interaction {
        id: 10,
        action: Action::Selected(2),
    });
    assert_eq!(
        backend.take_updates(),
        vec![
            GuiUpdate::SetText {
                id: 7,
                text: "hi".to_string(),
            },
            GuiUpdate::SetChecked {
                id: 8,
                checked: true
            },
            GuiUpdate::Select { id: 10, index: 2 },
        ]
    );
    assert!(backend.take_updates().is_empty(), "updates drain once");
}

//! Unit tests for `widgets`'s control → egui mapping.
//!
//! Kept out of `widgets.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;
use autoitv3_gui_model::{Control, ControlKind};
use egui::{vec2, Context, Event, Modifiers, PointerButton, RawInput};

fn frame() -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 200.0))),
        // A fixed time keeps the layout and the animations deterministic.
        time: Some(0.0),
        ..Default::default()
    }
}

/// One headless pass. `TexturesDelta` must be handled or dropped empty.
fn run_ui(ctx: &Context, raw: RawInput, run: impl FnMut(&mut egui::Ui)) {
    let mut output = ctx.run_ui(raw, run);
    output.textures_delta.clear();
}

/// The rectangle a control occupies, measured with a first pass.
fn probe(ctx: &Context, control: &Control) -> Rect {
    let mut rect = Rect::NOTHING;
    run_ui(ctx, frame(), |ui| {
        rect = ui.scope(|ui| draw_control(ui, control)).response.rect;
    });
    rect
}

/// Run one frame whose pointer clicks at `pos`, collecting interactions.
fn click(ctx: &Context, control: &Control, pos: Pos2) -> Vec<Interaction> {
    let mut raw = frame();
    raw.events = vec![
        Event::PointerMoved(pos),
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        },
        Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        },
    ];
    let mut seen = Vec::new();
    run_ui(ctx, raw, |ui| {
        seen.extend(ui.scope(|ui| draw_control(ui, control)).inner);
    });
    seen
}

#[test]
fn a_click_on_a_button_reports_clicked() {
    let ctx = Context::default();
    let mut button = Control::new(3, 1, ControlKind::Button);
    button.text = "Click".to_string();

    let rect = probe(&ctx, &button);
    assert!(rect.width() > 0.0, "the button was laid out");

    let seen = click(&ctx, &button, rect.center());
    assert!(
        seen.contains(&Interaction {
            id: 3,
            action: Action::Clicked
        }),
        "a click on the button produced {seen:?}"
    );
}

#[test]
fn a_click_on_a_checkbox_reports_checked() {
    let ctx = Context::default();
    let mut checkbox = Control::new(4, 1, ControlKind::Checkbox);
    checkbox.text = "Enable".to_string();

    let rect = probe(&ctx, &checkbox);
    let seen = click(&ctx, &checkbox, rect.center());
    assert!(
        seen.contains(&Interaction {
            id: 4,
            action: Action::Checked(true)
        }),
        "a click on the checkbox produced {seen:?}"
    );
}

#[test]
fn typing_in_an_input_reports_the_new_text() {
    let ctx = Context::default();
    let mut input = Control::new(6, 1, ControlKind::Input);
    input.text = String::new();

    // Focus it, then type.
    let rect = probe(&ctx, &input);
    let _ = click(&ctx, &input, rect.center());

    let mut raw = frame();
    raw.events = vec![Event::Text("hello".to_string())];
    let mut seen = Vec::new();
    run_ui(&ctx, raw, |ui| {
        seen.extend(ui.scope(|ui| draw_control(ui, &input)).inner);
    });
    assert!(
        seen.contains(&Interaction {
            id: 6,
            action: Action::Text("hello".to_string())
        }),
        "typing produced {seen:?}"
    );
}

#[test]
fn clicking_a_list_item_reports_the_selection() {
    let ctx = Context::default();
    let mut list = Control::new(7, 1, ControlKind::List);
    list.data = vec!["only".to_string()];

    let rect = probe(&ctx, &list);
    let seen = click(&ctx, &list, rect.center());
    assert!(
        seen.contains(&Interaction {
            id: 7,
            action: Action::Selected(0)
        }),
        "clicking the item produced {seen:?}"
    );
}

#[test]
fn a_hidden_or_disabled_control_reports_nothing() {
    let ctx = Context::default();
    let mut button = Control::new(5, 1, ControlKind::Button);
    button.text = "Click".to_string();
    let rect = probe(&ctx, &button);

    button.state = autoitv3_gui_model::GUI_HIDE;
    let hidden = click(&ctx, &button, rect.center());
    assert!(hidden.is_empty(), "a hidden control produced {hidden:?}");

    button.state = autoitv3_gui_model::GUI_DISABLE;
    let disabled = click(&ctx, &button, rect.center());
    assert!(
        disabled.is_empty(),
        "a disabled control produced {disabled:?}"
    );
}
#[test]
fn a_window_the_user_un_maximised_is_drawn_where_it_will_be_restored_to() {
    let mut window = Window::new(1, "W", 960, 540);
    window.x = 0;
    window.y = 0;
    window.state = WindowState::Maximized;
    window.restore = Some((220, 140, 520, 300));

    // While the script has not applied the state yet, draw the rectangle it
    // is about to restore to — not the maximised one at (0, 0), which would
    // both look wrong and be reported back as the window's place.
    let effective = effective_window(&window, Some((WindowState::Normal, 40)));
    assert_eq!((effective.x, effective.y), (220, 140));
    assert_eq!((effective.width, effective.height), (520, 300));
    assert_eq!(effective.state, WindowState::Normal);

    // A script that never applies the state cannot pin the window: the
    // frame budget expiring falls back to the model.
    let expired = effective_window(&window, Some((WindowState::Normal, 0)));
    assert_eq!((expired.x, expired.y), (0, 0));
    assert_eq!(expired.state, WindowState::Maximized);

    // Nothing requested: the model is the truth.
    let plain = effective_window(&window, None);
    assert_eq!((plain.x, plain.y), (0, 0));
    assert_eq!(plain.state, WindowState::Maximized);
}

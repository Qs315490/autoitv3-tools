//! The AutoIt window must be resizable on every edge, in both axes.
//!
//! `egui::Window` takes its size from its content, so a window whose body does
//! not fill it snaps back on the frame after a drag. This test drags the real
//! widget layer (the same entry point `LiveBackend` uses) and pins the fix
//! down: before it, only the horizontal edges moved.

#![cfg(feature = "egui")]

use autoitv3_gui::{Control, ControlKind, Window};
use autoitv3_gui_egui::show_autoit_window;
use egui::{vec2, Context, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};

const WIDTH: i32 = 380;
const HEIGHT: i32 = 170;

fn controls() -> Vec<Control> {
    let mut label = Control::new(1, 1, ControlKind::Label);
    label.text = "Type a name, then press Greet:".to_string();
    let mut input = Control::new(2, 1, ControlKind::Input);
    input.text = "world".to_string();
    input.width = 220;
    input.height = 24;
    let mut button = Control::new(3, 1, ControlKind::Button);
    button.text = "Greet".to_string();
    vec![label, input, button]
}

fn raw(events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
        time: Some(0.0),
        events,
        ..Default::default()
    }
}

fn press(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Drives the window the way `LiveBackend` does: the client size we drew last
/// frame is fed back in, exactly like the live window's `clients` map.
struct Harness {
    ctx: Context,
    last_client: Option<Vec2>,
}

impl Harness {
    fn new() -> Self {
        Self {
            ctx: Context::default(),
            last_client: None,
        }
    }

    /// One frame: returns the window's outer rect and the client size.
    fn frame(&mut self, events: Vec<Event>) -> (Rect, Vec2) {
        let mut window = Window::new(1, "AutoIt PoC", 20, 20);
        window.width = WIDTH;
        window.height = HEIGHT;
        let last = self.last_client;
        let mut client = Vec2::ZERO;
        let mut output = self.ctx.run_ui(raw(events), |ui| {
            let mut open = true;
            let mut actions = Vec::new();
            if let Some(size) = show_autoit_window(
                ui.ctx(),
                &window,
                &controls(),
                &mut open,
                last,
                &mut actions,
            ) {
                client = size;
            }
        });
        output.textures_delta.clear();
        self.last_client = Some(client);
        let rect = self
            .ctx
            .memory(|memory| memory.area_rect(egui::Id::new(("autoit-window", 1))))
            .unwrap_or(Rect::NOTHING);
        (rect, client)
    }

    /// Hover, press, drag in steps, release — the way a pointer actually arrives.
    fn drag(&mut self, target: impl Fn(Rect) -> Pos2, delta: Vec2) -> Rect {
        let start = self.frame(vec![]).0;
        let p = target(start);
        for _ in 0..3 {
            self.frame(vec![Event::PointerMoved(p)]);
        }
        self.frame(vec![press(p, true)]);
        for step in 1..=4 {
            self.frame(vec![Event::PointerMoved(p + delta * (step as f32 / 4.0))]);
        }
        self.frame(vec![press(p + delta, false)]);
        self.frame(vec![]).0
    }
}

#[test]
fn the_window_starts_at_the_size_the_script_asked_for() {
    let mut harness = Harness::new();
    let (rect, client) = harness.frame(vec![]);
    // The client area is the size the script asked for, to the pixel: a live
    // window reports any change back, so a settled frame must not drift.
    assert!(
        (client - vec2(WIDTH as f32, HEIGHT as f32)).length() < 1.0,
        "client area is {client:?}, wanted {WIDTH}x{HEIGHT}"
    );
    // And it stays there: later frames must not resize anything (a settled
    // frame that changed size would be reported as a user drag).
    for _ in 0..4 {
        let (again, client) = harness.frame(vec![]);
        assert!(
            (again.size() - rect.size()).length() < 0.5,
            "the window drifted from {rect:?} to {again:?}"
        );
        assert!(
            (client - vec2(WIDTH as f32, HEIGHT as f32)).length() < 1.0,
            "client area drifted to {client:?}"
        );
    }
    // Only the client area belongs to the script; the frame adds its margins.
    assert!(rect.width() > WIDTH as f32, "outer must wrap the client");
    assert!(
        rect.width() < WIDTH as f32 + 40.0,
        "outer {rect:?} too wide"
    );
    assert!(
        rect.height() > HEIGHT as f32,
        "outer must include the title bar: {rect:?}"
    );
    assert!(
        rect.height() < HEIGHT as f32 + 60.0,
        "the window must not shrink to fit its controls: {rect:?}"
    );
}

/// Where to grab a window edge, how the pointer moves, what should happen.
type ResizeCase = (&'static str, fn(Rect) -> Pos2, Vec2, Vec2);

#[test]
fn every_edge_resizes_the_window() {
    let cases: [ResizeCase; 5] = [
        (
            "right",
            |r| Pos2::new(r.right() - 1.0, r.center().y),
            vec2(120.0, 0.0),
            vec2(120.0, 0.0),
        ),
        (
            "left",
            |r| Pos2::new(r.left() + 1.0, r.center().y),
            vec2(-120.0, 0.0),
            vec2(120.0, 0.0),
        ),
        (
            "bottom",
            |r| Pos2::new(r.center().x, r.bottom() - 1.0),
            vec2(0.0, 120.0),
            vec2(0.0, 120.0),
        ),
        (
            "top",
            |r| Pos2::new(r.center().x, r.top() + 1.0),
            vec2(0.0, -120.0),
            vec2(0.0, 120.0),
        ),
        (
            "corner",
            |r| Pos2::new(r.right() - 3.0, r.bottom() - 3.0),
            vec2(120.0, 120.0),
            vec2(120.0, 120.0),
        ),
    ];

    for (name, target, delta, expected) in cases {
        let mut harness = Harness::new();
        let start = harness.frame(vec![]).0;
        let end = harness.drag(target, delta);
        let dw = end.width() - start.width();
        let dh = end.height() - start.height();
        assert!(
            (dw - expected.x).abs() < 8.0,
            "dragging the {name} edge changed the width by {dw}, wanted {}",
            expected.x
        );
        assert!(
            (dh - expected.y).abs() < 8.0,
            "dragging the {name} edge changed the height by {dh}, wanted {}. \
             (A window that cannot grow downwards is the bug this guards.)",
            expected.y
        );
    }
}

#[test]
fn a_shrinking_drag_sticks() {
    let mut harness = Harness::new();
    let start = harness.frame(vec![]).0;
    // Pull the bottom edge up: the window must actually shrink and stay there
    // (raising the floor to what was last drawn is what keeps it from snapping
    // back while the script catches up).
    let end = harness.drag(
        |r| Pos2::new(r.center().x, r.bottom() - 1.0),
        vec2(0.0, -40.0),
    );
    // egui measures the drag against the frame before it was detected, so a
    // short drag loses a few pixels; the direction and rough size must hold.
    let shrunk = start.height() - end.height();
    assert!(
        (20.0..60.0).contains(&shrunk),
        "a 40 px drag up should shrink the window by about that: {start:?} -> {end:?}"
    );
    for _ in 0..4 {
        let (after, _) = harness.frame(vec![]);
        assert!(
            (after.height() - end.height()).abs() < 0.5,
            "the window snapped back to {after:?} after being shrunk to {end:?}"
        );
    }
}

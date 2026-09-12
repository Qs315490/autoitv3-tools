//! The AutoIt window must be resizable on every edge, in both axes.
//!
//! `egui::Window` takes its size from its content, so a window whose body does
//! not fill it snaps back on the frame after a drag. This test drags the real
//! widget layer (the same entry point `LiveBackend` uses) and pins the fix
//! down: before it, only the horizontal edges moved.

#![cfg(feature = "egui")]

use autoitv3_gui::{Control, ControlKind, Window};
use autoitv3_gui_egui::{
    show_autoit_window, window_area_id, LastWindow, MinimizeStyle, WindowGeometry,
};
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

fn raw(time: f64, events: Vec<Event>) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900.0, 700.0))),
        time: Some(time),
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

/// Drives the window the way `LiveBackend` does: the record of what was drawn
/// last frame is fed back in, exactly like the live window's `seen` map.
struct Harness {
    ctx: Context,
    window: Window,
    last: LastWindow,
    /// What the last frame drew; `None` when the window was not on screen.
    drawn: Option<Vec2>,
    /// A state change the user asked for (double-clicking the title bar).
    requested: Option<autoitv3_gui::WindowState>,
    minimize: MinimizeStyle,
    /// Seconds; egui tells a double-click from a triple one by how long ago the
    /// previous click was, so a frozen clock turns the second gesture into a
    /// triple click.
    time: f64,
}

impl Harness {
    fn new() -> Self {
        let mut window = Window::new(1, "AutoIt PoC", 20, 20);
        window.width = WIDTH;
        window.height = HEIGHT;
        Self {
            ctx: Context::default(),
            window,
            last: LastWindow::default(),
            drawn: None,
            requested: None,
            minimize: MinimizeStyle::Hidden,
            time: 0.0,
        }
    }

    /// One frame: returns the window's outer rect and the client size.
    fn frame(&mut self, events: Vec<Event>) -> (Rect, Vec2) {
        let last = self.last;
        let minimize = self.minimize;
        self.time += 0.05;
        let mut drawn = autoitv3_gui_egui::DrawnWindow::default();
        let mut output = self.ctx.run_ui(raw(self.time, events), |ui| {
            let mut open = true;
            let mut actions = Vec::new();
            drawn = show_autoit_window(
                ui.ctx(),
                &self.window,
                &controls(),
                &mut open,
                last,
                minimize,
                &mut actions,
            );
        });
        output.textures_delta.clear();
        let client = drawn.client;
        self.requested = drawn.state_request;
        self.drawn = client;
        self.last = LastWindow {
            client: client.or(last.client),
            geometry: Some(WindowGeometry::of(&self.window)),
        };
        let rect = self
            .ctx
            .memory(|memory| memory.area_rect(window_area_id(1)))
            .unwrap_or(Rect::NOTHING);
        (rect, client.unwrap_or(Vec2::ZERO))
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

#[test]
fn a_script_side_move_resizes_the_window() {
    // What `WinMove` does: the model changes, and the window must follow
    // instead of keeping the size the script originally asked for.
    let mut harness = Harness::new();
    let start = harness.frame(vec![]).0;

    harness.window.x = 120;
    harness.window.y = 90;
    harness.window.width = 300;
    harness.window.height = 260;
    let (moved, client) = harness.frame(vec![]);

    assert!(
        (client - vec2(300.0, 260.0)).length() < 1.0,
        "the client area followed WinMove to {client:?}, wanted 300x260"
    );
    assert!(
        (moved.left() - 120.0).abs() < 8.0 && (moved.top() - 90.0).abs() < 8.0,
        "the window moved to {moved:?}, wanted about (120, 90)"
    );
    assert!(
        moved.width() > start.width() || moved.height() > start.height(),
        "the window grew with the script: {start:?} -> {moved:?}"
    );

    // And it stays there on later frames.
    let (again, _) = harness.frame(vec![]);
    assert!(
        (again.size() - moved.size()).length() < 0.5,
        "the window drifted after the script moved it: {moved:?} -> {again:?}"
    );
}

#[test]
fn a_minimised_window_is_not_drawn_and_comes_back_where_it_was() {
    let mut harness = Harness::new();
    let (before, _) = harness.frame(vec![]);

    // The faithful style: @SW_MINIMIZE takes the window off screen.
    harness.window.state = autoitv3_gui::WindowState::Minimized;
    let _ = harness.frame(vec![]);
    assert!(
        harness.drawn.is_none(),
        "a minimised window drew {:?}",
        harness.drawn
    );

    // @SW_RESTORE: it comes back at the size it had, not at a default.
    harness.window.state = autoitv3_gui::WindowState::Normal;
    let (after, client) = harness.frame(vec![]);
    assert!(
        (after.size() - before.size()).length() < 0.5,
        "restore changed the size: {before:?} -> {after:?}"
    );
    assert!(
        (client - vec2(WIDTH as f32, HEIGHT as f32)).length() < 1.0,
        "restored client is {client:?}"
    );
}

#[test]
fn the_title_bar_style_keeps_the_title_and_hides_the_body() {
    let mut harness = Harness::new();
    harness.minimize = MinimizeStyle::TitleBar;
    let (normal, _) = harness.frame(vec![]);

    harness.window.state = autoitv3_gui::WindowState::Minimized;
    let (collapsed, client) = harness.frame(vec![]);
    assert_eq!(
        client,
        vec2(WIDTH as f32, 0.0),
        "a title-bar-style window keeps no body"
    );
    assert!(
        collapsed.height() < normal.height() - 100.0,
        "the body is gone but the title bar stays: {normal:?} -> {collapsed:?}"
    );
    assert!(
        (collapsed.width() - normal.width()).abs() < 1.0,
        "the title bar keeps the width: {normal:?} -> {collapsed:?}"
    );

    // Double-clicking that title bar is the way back.
    let title = Pos2::new(collapsed.center().x, collapsed.top() + 8.0);
    double_click(&mut harness, title);
    assert_eq!(
        harness.requested,
        Some(autoitv3_gui::WindowState::Normal),
        "double-clicking a minimised title bar should restore"
    );
}

/// Two clicks in quick succession on the same spot.
fn double_click(harness: &mut Harness, pos: Pos2) {
    harness.frame(vec![Event::PointerMoved(pos)]);
    harness.frame(vec![press(pos, true)]);
    harness.frame(vec![press(pos, false)]);
    harness.frame(vec![press(pos, true)]);
    harness.frame(vec![press(pos, false)]);
}

#[test]
fn a_maximised_window_fills_the_viewport() {
    let mut harness = Harness::new();
    let (normal, _) = harness.frame(vec![]);

    harness.window.state = autoitv3_gui::WindowState::Maximized;
    let (maximized, _) = harness.frame(vec![]);
    let screen = harness.ctx.content_rect();
    assert!(
        maximized.width() > normal.width() && maximized.height() > normal.height(),
        "a maximised window should be bigger: {normal:?} -> {maximized:?}"
    );
    assert!(
        maximized.width() <= screen.width() + 1.0 && maximized.height() <= screen.height() + 1.0,
        "a maximised window left the viewport: {maximized:?} in {screen:?}"
    );
    assert!(
        maximized.width() > screen.width() * 0.9,
        "a maximised window should nearly fill the viewport: {maximized:?}"
    );
}

#[test]
fn double_clicking_the_title_bar_maximises_and_restores() {
    let mut harness = Harness::new();
    let (start, _) = harness.frame(vec![]);
    let title = Pos2::new(start.center().x, start.top() + 8.0);

    // Windows maximises on a title-bar double-click; egui would collapse the
    // window instead, which is what this guards against.
    double_click(&mut harness, title);
    assert_eq!(
        harness.requested,
        Some(autoitv3_gui::WindowState::Maximized),
        "the first double-click should ask to maximise"
    );
    let (after, _) = harness.frame(vec![]);
    assert!(
        after.height() >= start.height(),
        "a double-click must not collapse the window: {start:?} -> {after:?}"
    );

    // Obey it the way the semantics layer would, then double-click again —
    // after a gap, so egui counts a fresh double-click rather than a triple.
    harness.time += 1.0;
    harness.window.state = autoitv3_gui::WindowState::Maximized;
    let (maximized, _) = harness.frame(vec![]);
    assert!(maximized.height() > start.height(), "maximised is taller");
    let title = Pos2::new(maximized.center().x, maximized.top() + 8.0);
    double_click(&mut harness, title);
    assert_eq!(
        harness.requested,
        Some(autoitv3_gui::WindowState::Normal),
        "double-clicking a maximised title bar should restore"
    );
}

#[test]
fn a_collapsed_window_still_reports_the_real_client_size() {
    // The caller must not mistake "collapsed to a title bar" for a resize.
    let mut harness = Harness::new();
    harness.minimize = MinimizeStyle::TitleBar;
    let (_, client) = harness.frame(vec![]);
    assert!((client - vec2(WIDTH as f32, HEIGHT as f32)).length() < 1.0);

    harness.window.state = autoitv3_gui::WindowState::Minimized;
    let (_, collapsed) = harness.frame(vec![]);
    assert_eq!(collapsed.y, 0.0);
}

//! The AutoIt window must be resizable on every edge, in both axes.
//!
//! `egui::Window` takes its size from its content, so a window whose body does
//! not fill it snaps back on the frame after a drag. This test drags the real
//! widget layer (the same entry point `LiveBackend` uses) and pins the fix
//! down: before it, only the horizontal edges moved.

#![cfg(feature = "egui")]

use autoitv3_gui::{Control, ControlKind, Window};
use autoitv3_gui_egui::{
    apply_textures, rasterize, record_drawn, show_autoit_window, window_area_id, LastWindow,
    MinimizeStyle, Texture, WindowGeometry,
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
    /// A state change the user asked for (double-clicking the title bar, or a
    /// title-bar control).
    requested: Option<autoitv3_gui::WindowState>,
    /// Where the title-bar controls were drawn.
    controls: Option<Rect>,
    /// A resize the frame asked to report to the model.
    reported: Option<autoitv3_gui::GuiUpdate>,
    /// The font atlas, kept across frames the way the offscreen renderer keeps
    /// it (egui patches the atlas as new glyphs appear).
    textures: std::collections::HashMap<egui::TextureId, Texture>,
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
            controls: None,
            reported: None,
            textures: std::collections::HashMap::new(),
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
        self.controls = drawn.controls;
        self.drawn = client;
        // Exactly what LiveBackend does with a frame, so the two cannot drift.
        let geometry = WindowGeometry::of(&self.window);
        let (next, updates) = record_drawn(&self.window, geometry, last, drawn, false);
        self.reported = updates.into_iter().next();
        self.last = next;
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
fn restoring_a_maximised_window_brings_back_both_axes() {
    let mut harness = Harness::new();
    let (normal, _) = harness.frame(vec![]);
    harness.frame(vec![]);

    harness.window.state = autoitv3_gui::WindowState::Maximized;
    let (maximized, _) = harness.frame(vec![]);
    assert!(
        maximized.width() > normal.width() && maximized.height() > normal.height(),
        "maximised should be bigger: {normal:?} -> {maximized:?}"
    );

    // @SW_RESTORE: the width has to come back too. It follows egui's stored
    // desired size, which only `Window::max_size` can shrink.
    harness.time += 1.0;
    harness.window.state = autoitv3_gui::WindowState::Normal;
    let (restored, client) = harness.frame(vec![]);
    assert!(
        (restored.width() - normal.width()).abs() < 0.5,
        "the width did not come back: {normal:?} -> {restored:?}"
    );
    assert!(
        (restored.height() - normal.height()).abs() < 0.5,
        "the height did not come back: {normal:?} -> {restored:?}"
    );
    assert!(
        (restored.left() - normal.left()).abs() < 0.5
            && (restored.top() - normal.top()).abs() < 0.5,
        "the position did not come back: {normal:?} -> {restored:?}"
    );
    assert!(
        harness.reported.is_none(),
        "restoring is not a user resize: {:?}",
        harness.reported
    );
    // And it stays there.
    for _ in 0..3 {
        let (again, _) = harness.frame(vec![]);
        assert!(
            (again.size() - restored.size()).length() < 0.5,
            "the restored window drifted: {restored:?} -> {again:?}"
        );
    }
    assert!(
        (client - vec2(WIDTH as f32, HEIGHT as f32)).length() < 1.0,
        "the restored client is {client:?}"
    );
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

/// Click once at `pos` and report what the window asked for.
fn click_at(harness: &mut Harness, pos: Pos2) -> Option<autoitv3_gui::WindowState> {
    harness.frame(vec![Event::PointerMoved(pos)]);
    harness.frame(vec![press(pos, true)]);
    harness.frame(vec![press(pos, false)]);
    harness.requested
}

/// The two window controls, (minimise, maximise), as `DrawnWindow` laid them out:
/// square glyphs with the maximiser on the right.
///
/// They only appear from the second frame: egui reports a widget's response on
/// the frame after it was created.
fn window_controls(harness: &Harness) -> (Pos2, Pos2) {
    let rect = harness.controls.expect("the window controls were drawn");
    let side = rect.height();
    let maximise =
        Rect::from_min_size(Pos2::new(rect.right() - side, rect.top()), vec2(side, side));
    let minimise = Rect::from_min_size(Pos2::new(rect.left(), rect.top()), vec2(side, side));
    (minimise.center(), maximise.center())
}

#[test]
fn the_title_bar_has_minimise_and_maximise_buttons() {
    let mut harness = Harness::new();
    harness.frame(vec![]);
    harness.frame(vec![]);
    let (_, maximise) = window_controls(&harness);

    assert_eq!(
        click_at(&mut harness, maximise),
        Some(autoitv3_gui::WindowState::Maximized),
        "the maximise button should ask to maximise"
    );

    // Obey it the way the semantics layer would: the same button restores.
    harness.time += 1.0;
    harness.window.state = autoitv3_gui::WindowState::Maximized;
    harness.frame(vec![]);
    harness.frame(vec![]);
    let (_, restore) = window_controls(&harness);
    assert_ne!(restore, maximise, "the maximised window moved its controls");
    assert_eq!(
        click_at(&mut harness, restore),
        Some(autoitv3_gui::WindowState::Normal),
        "the same button restores a maximised window"
    );

    harness.time += 1.0;
    harness.window.state = autoitv3_gui::WindowState::Normal;
    harness.frame(vec![]);
    harness.frame(vec![]);
    let (minimise, _) = window_controls(&harness);
    assert_eq!(
        click_at(&mut harness, minimise),
        Some(autoitv3_gui::WindowState::Minimized),
        "the minimise button should ask to minimise"
    );
}

#[test]
fn clicking_a_window_control_is_not_a_title_double_click() {
    // A double-click on the minimise button must not minimise and then
    // immediately restore it.
    let mut harness = Harness::new();
    harness.frame(vec![]);
    harness.frame(vec![]);
    let (minimise, _) = window_controls(&harness);
    double_click(&mut harness, minimise);
    assert_eq!(
        harness.requested,
        Some(autoitv3_gui::WindowState::Minimized),
        "the button wins over the title-bar double-click"
    );
}

#[test]
fn a_maximised_window_does_not_oscillate() {
    // A maximised window is sized by the desktop, not by the size it had
    // before: if a frame forgets what it drew, the next one measures a chrome
    // out of the stale size and the window flips between the two every frame.
    let mut harness = Harness::new();
    harness.frame(vec![]);
    harness.frame(vec![]);
    harness.window.state = autoitv3_gui::WindowState::Maximized;

    let (first, _) = harness.frame(vec![]);
    for frame in 0..6 {
        let (again, client) = harness.frame(vec![]);
        assert!(
            (again.size() - first.size()).length() < 0.5,
            "frame {frame}: the maximised window changed size: {first:?} -> {again:?}"
        );
        assert!(
            harness.reported.is_none(),
            "frame {frame}: a maximised window's size is not a user resize: {:?}",
            harness.reported
        );
        let _ = client;
    }
}

/// One frame's pixels, for checking what the title bar actually looks like.
fn rasterize_frame(harness: &mut Harness, events: Vec<Event>) -> (Vec<u8>, u32, u32) {
    let (width, height) = (900u32, 700u32);
    let last = harness.last;
    let minimize = harness.minimize;
    harness.time += 0.05;
    let mut drawn = autoitv3_gui_egui::DrawnWindow::default();
    let mut output = harness.ctx.run_ui(raw(harness.time, events), |ui| {
        let mut open = true;
        let mut actions = Vec::new();
        drawn = show_autoit_window(
            ui.ctx(),
            &harness.window,
            &controls(),
            &mut open,
            last,
            minimize,
            &mut actions,
        );
    });
    harness.requested = drawn.state_request;
    harness.controls = drawn.controls;
    let geometry = WindowGeometry::of(&harness.window);
    let (next, _) = record_drawn(&harness.window, geometry, last, drawn, false);
    harness.last = next;

    // What EguiBackend does to turn shapes into pixels.
    let pixels_per_point = output.pixels_per_point;
    apply_textures(&mut harness.textures, &output.textures_delta);
    output.textures_delta.clear();
    let primitives = harness.ctx.tessellate(output.shapes, pixels_per_point);
    let image = rasterize(
        &primitives,
        &harness.textures,
        width,
        height,
        pixels_per_point,
    );
    (image.rgba, image.width, image.height)
}

/// How many pixels of `rect` differ from the title bar behind it.
///
/// The reference colour is taken from just *above* the glyph: sampling inside
/// it would call a filled box "background" and see no ink at all.
fn ink_in(image: &[u8], width: u32, rect: Rect) -> (usize, usize) {
    let pixel = |x: i32, y: i32| {
        let i = ((y as u32 * width + x as u32) * 4) as usize;
        [image[i], image[i + 1], image[i + 2], image[i + 3]]
    };
    let corner = pixel(rect.center().x as i32, rect.top() as i32 - 4);
    let mut ink = 0;
    let mut total = 0;
    for y in rect.top() as i32..rect.bottom() as i32 {
        for x in rect.left() as i32..rect.right() as i32 {
            let here = pixel(x, y);
            total += 1;
            let far = here
                .iter()
                .zip(corner.iter())
                .any(|(a, b)| a.abs_diff(*b) > 12);
            if far {
                ink += 1;
            }
        }
    }
    (ink, total)
}

#[test]
fn the_window_controls_are_line_art_like_the_close_button() {
    // egui's own close button is a stroked cross with no background; ours have
    // to look like it, not like little filled boxes.
    let mut harness = Harness::new();
    let _ = rasterize_frame(&mut harness, vec![]);
    let (image, width, _) = rasterize_frame(&mut harness, vec![]);
    let (minimise, maximise) = window_controls(&harness);
    let side = harness.controls.expect("controls drawn").height();
    let square = |centre: Pos2| Rect::from_center_size(centre, vec2(side, side));

    for (name, centre) in [("minimise", minimise), ("maximise", maximise)] {
        let (ink, total) = ink_in(&image, width, square(centre));
        assert!(ink > 0, "the {name} glyph painted nothing");
        // Line art lands around 4-10% of the square; a filled button box was
        // measured at 33%, so a quarter is the line between the two.
        assert!(
            ink * 4 < total,
            "the {name} glyph covers {ink}/{total} pixels: it should be line art, \
             not a filled box"
        );
    }
}

#[test]
fn dragging_a_maximised_window_leaves_the_maximised_state() {
    // Windows un-maximises a window when its border is dragged, and lets the
    // pointer take it from there; a maximised window here is pinned to the
    // viewport, so without this the drag would be undone on the next frame.
    let mut harness = Harness::new();
    harness.frame(vec![]);
    harness.frame(vec![]);
    harness.window.state = autoitv3_gui::WindowState::Maximized;
    let (maximized, _) = harness.frame(vec![]);
    assert!(
        maximized.width() > 800.0,
        "the viewport is 900 wide, so maximised should fill it: {maximized:?}"
    );

    // Drag the right edge inwards.
    let start = harness.frame(vec![]).0;
    let from = Pos2::new(start.right() - 1.0, start.center().y);
    for _ in 0..3 {
        harness.frame(vec![Event::PointerMoved(from)]);
    }
    harness.frame(vec![press(from, true)]);
    for step in 1..=4 {
        let to = from - vec2(160.0 * step as f32 / 4.0, 0.0);
        let (rect, _) = harness.frame(vec![Event::PointerMoved(to)]);
        assert!(
            rect.width() < maximized.width() - 20.0,
            "the drag should shrink the window: {rect:?}"
        );
    }
    assert_eq!(
        harness.requested,
        Some(autoitv3_gui::WindowState::Normal),
        "the drag should ask to leave the maximised state"
    );

    // Once the script applies it (the model says Normal), the new size stays.
    harness.time += 1.0;
    harness.window.state = autoitv3_gui::WindowState::Normal;
    harness.window.width = (start.width() - 160.0) as i32;
    let (after, client) = harness.frame(vec![]);
    assert!(
        (after.width() - start.width()).abs() > 100.0,
        "the window kept the dragged size: {start:?} -> {after:?}"
    );
    assert!(client.x > 0.0 && client.x < 900.0);
}

#[test]
fn dragging_the_title_bar_moves_the_window_and_tells_the_model() {
    let mut harness = Harness::new();
    harness.frame(vec![]);
    harness.frame(vec![]);
    let start = harness.frame(vec![]).0;
    let from = Pos2::new(start.center().x, start.top() + 8.0);
    let to = from + vec2(60.0, 40.0);

    harness.frame(vec![Event::PointerMoved(from)]);
    harness.frame(vec![press(from, true)]);
    for step in 1..=4 {
        harness.frame(vec![Event::PointerMoved(
            from + (to - from) * (step as f32 / 4.0),
        )]);
    }
    let (moved, _) = harness.frame(vec![]);
    assert!(
        (moved.left() - start.left() - 60.0).abs() < 12.0
            && (moved.top() - start.top() - 40.0).abs() < 12.0,
        "the window followed the pointer: {start:?} -> {moved:?}"
    );
    match harness.reported {
        Some(autoitv3_gui::GuiUpdate::Move { x, y, .. }) => {
            assert!(
                (x as f32 - moved.left()).abs() < 2.0 && (y as f32 - moved.top()).abs() < 2.0,
                "the reported place {x},{y} should be where it was drawn: {moved:?}"
            );
        }
        other => panic!("a user drag should report a move, got {other:?}"),
    }
}

#[test]
fn a_drag_that_pulls_a_window_out_of_a_state_is_reported() {
    // The script has not applied anything yet, so the window's numbers are the
    // ones from the maximised frame; only the state differs. The size and place
    // the user dragged to still have to reach the model, or the frame after
    // this one snaps back to the desktop rectangle.
    let mut window = Window::new(1, "W", 0, 0);
    // While maximised the model holds the desktop rectangle; the user's drag is
    // what the state override lets through.
    window.width = 900;
    window.height = 700;
    let mut effective = window.clone();
    effective.state = autoitv3_gui::WindowState::Normal;

    let last = LastWindow {
        client: Some(vec2(900.0, 700.0)),
        geometry: Some(WindowGeometry {
            x: 0,
            y: 0,
            width: 900,
            height: 700,
            state: autoitv3_gui::WindowState::Maximized,
        }),
        chrome: Some(vec2(14.0, 48.0)),
    };
    let drawn = autoitv3_gui_egui::DrawnWindow {
        client: Some(vec2(700.0, 500.0)),
        pos: Some(Pos2::new(20.0, 30.0)),
        ..Default::default()
    };
    let (_next, updates) = record_drawn(
        &effective,
        WindowGeometry::of(&effective),
        last,
        drawn,
        true,
    );
    assert_eq!(
        updates,
        vec![
            autoitv3_gui::GuiUpdate::Resize {
                handle: 1,
                width: 700,
                height: 500
            },
            autoitv3_gui::GuiUpdate::Move {
                handle: 1,
                x: 20,
                y: 30
            },
        ]
    );
}

//! Every `ControlKind` must survive a render.
//!
//! These tests drive the offscreen backend with one control of each kind and
//! check that the shared widget layer actually paints something (except for a
//! `Dummy`, which is intentionally invisible), plus a full gallery that
//! exercises the whole set in one window.

#![cfg(feature = "egui")]

use autoitv3_gui_model::{Control, ControlKind, DrawCmd, GuiBackend, Window};
use autoitv3_gui_egui::EguiBackend;

const WINDOW: i64 = 0x1_0000;

/// Every kind `ControlKind::from_create` can produce.
fn all_kinds() -> Vec<ControlKind> {
    let keys = [
        "guictrlcreatelabel",
        "guictrlcreatebutton",
        "guictrlcreatecheckbox",
        "guictrlcreateradio",
        "guictrlcreategroup",
        "guictrlcreateinput",
        "guictrlcreateedit",
        "guictrlcreatelist",
        "guictrlcreatecombo",
        "guictrlcreatelistview",
        "guictrlcreatelistviewitem",
        "guictrlcreatetreeview",
        "guictrlcreatetreeviewitem",
        "guictrlcreatetab",
        "guictrlcreatetabitem",
        "guictrlcreatemenu",
        "guictrlcreatemenuitem",
        "guictrlcreatecontextmenu",
        "guictrlcreatepic",
        "guictrlcreateicon",
        "guictrlcreategraphic",
        "guictrlcreateprogress",
        "guictrlcreateslider",
        "guictrlcreateupdown",
        "guictrlcreatedate",
        "guictrlcreatemonthcal",
        "guictrlcreatedummy",
        "guictrlcreateavi",
        "guictrlcreateobj",
    ];
    keys.iter()
        .map(|key| ControlKind::from_create(key).expect("known create function"))
        .collect()
}

/// A control of `kind` with every field the renderer looks at populated.
fn sample(id: i64, kind: ControlKind) -> Control {
    let mut control = Control::new(id, WINDOW, kind);
    control.text = "sample".to_string();
    control.width = 180;
    control.height = 28;
    control.data = vec!["alpha".to_string(), "beta".to_string()];
    control.selection = Some(1);
    control.tip = "a tooltip".to_string();
    control.color = Some(0x00_00_FF);
    control.bk_color = Some(0x20_20_20);
    control.limit = Some((0, 10));
    control.image = Some("splash.bmp".to_string());
    control.state = autoitv3_gui_model::GUI_CHECKED;
    control.draw = vec![
        DrawCmd::SetColor(0x00_FF_00),
        DrawCmd::SetWidth(2),
        DrawCmd::Line {
            x1: 0,
            y1: 0,
            x2: 60,
            y2: 40,
        },
        DrawCmd::Rect {
            x: 4,
            y: 4,
            w: 40,
            h: 20,
        },
        DrawCmd::Ellipse {
            x: 30,
            y: 10,
            w: 20,
            h: 20,
        },
        DrawCmd::Text {
            x: 4,
            y: 4,
            text: "g".to_string(),
        },
        DrawCmd::Clear,
    ];
    control
}

fn ink(image: &autoitv3_gui_model::GuiImage) -> usize {
    image.rgba.chunks_exact(4).filter(|p| p[3] > 0).count()
}

/// Pixels that differ from a baseline render (deterministic, so this is exact).
fn differences(image: &autoitv3_gui_model::GuiImage, baseline: &autoitv3_gui_model::GuiImage) -> usize {
    image
        .rgba
        .chunks_exact(4)
        .zip(baseline.rgba.chunks_exact(4))
        .filter(|(a, b)| a != b)
        .count()
}

fn render_one(control: &Control) -> autoitv3_gui_model::GuiImage {
    let mut backend = EguiBackend::new().with_size(320, 200);
    let mut window = Window::new(WINDOW, "One", 0, 0);
    window.width = 300;
    window.height = 180;
    window.controls.push(control.id);
    backend.on_window(&window);
    backend.on_control(control);
    backend.snapshot().expect("renders")
}

#[test]
fn there_are_twenty_nine_kinds_and_all_are_covered() {
    assert_eq!(all_kinds().len(), 29);
}

#[test]
fn every_control_kind_paints_something() {
    // The window frame alone is the floor; a visible control has to change it.
    let baseline = render_one(&sample(0, ControlKind::Dummy));
    for (index, kind) in all_kinds().into_iter().enumerate() {
        let control = sample(index as i64 + 1, kind);
        let changed = differences(&render_one(&control), &baseline);
        if kind == ControlKind::Dummy {
            assert_eq!(changed, 0, "Dummy should paint nothing on its own");
        } else {
            assert!(
                changed > 50,
                "{kind:?} changed {changed} px; the widget layer does not cover it"
            );
        }
    }
}

#[test]
fn a_window_with_every_control_renders_and_is_deterministic() {
    let build = |backend: &mut EguiBackend| {
        let mut window = Window::new(WINDOW, "Gallery", 0, 0);
        window.width = 700;
        window.height = 1380;
        backend.on_window(&window);
        for (index, kind) in all_kinds().into_iter().enumerate() {
            let control = sample(index as i64 + 1, kind);
            window.controls.push(control.id);
            backend.on_control(&control);
        }
        backend.on_window(&window);
    };

    let mut first = EguiBackend::new().with_size(720, 1400);
    build(&mut first);
    let image = first.snapshot().expect("renders");
    assert_eq!((image.width, image.height), (720, 1400));
    let painted = ink(&image);
    // Every non-Dummy control contributes; require a comfortably large area.
    assert!(
        painted > 20_000,
        "gallery rendered too little: {painted} px"
    );

    let mut second = EguiBackend::new().with_size(720, 1400);
    build(&mut second);
    assert_eq!(
        image.rgba,
        second.snapshot().unwrap().rgba,
        "offscreen output must be deterministic"
    );
}

#[test]
fn window_state_decides_what_offscreen_renders() {
    let render = |state: autoitv3_gui_model::WindowState| {
        let mut backend = EguiBackend::new().with_size(400, 300);
        let mut window = Window::new(WINDOW, "State", 40, 30);
        window.width = 200;
        window.height = 120;
        window.state = state;
        window.visible = true;
        window.controls.push(1);
        backend.on_window(&window);
        let mut label = Control::new(1, WINDOW, ControlKind::Label);
        label.text = "hello".to_string();
        backend.on_control(&label);
        backend.snapshot().expect("renders")
    };

    let normal = ink(&render(autoitv3_gui_model::WindowState::Normal));
    let maximized = ink(&render(autoitv3_gui_model::WindowState::Maximized));
    let minimized = ink(&render(autoitv3_gui_model::WindowState::Minimized));

    assert!(
        maximized > normal,
        "maximised should cover more: {normal} -> {maximized}"
    );
    assert_eq!(minimized, 0, "a minimised window is not on screen");
}

//! Chinese text needs a font egui does not ship.
//!
//! egui bundles Latin and emoji fonts, so a control whose text is Chinese draws
//! as empty boxes until a system font is added as a fallback.

#![cfg(feature = "egui")]

use autoitv3_gui::{Control, ControlKind, GuiBackend, Window};
use autoitv3_gui_egui::{fonts, EguiBackend};
use egui::{Context, RawInput};

/// egui only builds its fonts on the first `Context::run`.
fn draw_a_frame(ctx: &Context) {
    let mut output = ctx.run_ui(RawInput::default(), |_| {});
    output.textures_delta.clear();
}

fn ink(image: &autoitv3_gui::GuiImage) -> usize {
    image.rgba.chunks_exact(4).filter(|p| p[3] > 0).count()
}

fn render(label: &str) -> autoitv3_gui::GuiImage {
    let mut backend = EguiBackend::new().with_size(240, 120);
    let mut window = Window::new(1, "Fonts", 0, 0);
    window.width = 220;
    window.height = 100;
    window.controls.push(1);
    backend.on_window(&window);
    let mut control = Control::new(1, 1, ControlKind::Label);
    control.text = label.to_string();
    backend.on_control(&control);
    backend.snapshot().expect("renders")
}

/// Whether this machine has a CJK font at all; the tests skip without one.
fn has_cjk_font() -> bool {
    let ctx = Context::default();
    draw_a_frame(&ctx);
    fonts::install_cjk_font(&ctx).is_some()
}

#[test]
fn a_system_font_covers_chinese() {
    let ctx = Context::default();
    draw_a_frame(&ctx);
    assert!(
        !fonts::has_glyphs(&ctx, "中文"),
        "egui now ships CJK, so this fallback is no longer needed"
    );

    let Some(path) = fonts::install_cjk_font(&ctx) else {
        eprintln!("no CJK font on this machine; nothing to check");
        return;
    };
    draw_a_frame(&ctx);
    assert!(
        fonts::has_glyphs(&ctx, "中文标签"),
        "{} did not provide the glyphs",
        path.display()
    );
    // Latin keeps egui's own font, which is why the fallback is last.
    assert!(fonts::has_glyphs(&ctx, "Hello"));
}

#[test]
fn chinese_labels_render_as_glyphs_not_boxes() {
    if !has_cjk_font() {
        eprintln!("no CJK font on this machine; nothing to check");
        return;
    }
    // Two different labels have to look different: missing glyphs draw nothing
    // (or the same box), so the frames would be identical.
    let first = render("中文标签");
    let second = render("汉字渲染");
    assert!(ink(&first) > 0, "the labelled window drew nothing");
    assert_ne!(
        first.rgba, second.rgba,
        "different Chinese text rendered identically: the glyphs are missing"
    );
}

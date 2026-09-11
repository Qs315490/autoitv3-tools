//! The egui backend: mirrors the widget model and renders it offscreen.

use std::collections::{BTreeMap, HashMap};

use autoitv3_gui::{Control, ControlKind, GuiBackend, GuiImage, Window};
use egui::{Pos2, Rect, TextureId, vec2};

use crate::raster::{Texture, rasterize};

/// An offscreen egui renderer for the AutoIt GUI model.
///
/// It mirrors whatever the semantics layer reports through [`GuiBackend`], and
/// [`snapshot`](GuiBackend::snapshot) lays that model out with egui and
/// rasterizes the result on the CPU. There is no window and no GPU: the same
/// input always produces the same pixels, which is what tests and headless
/// runs need.
pub struct EguiBackend {
    windows: BTreeMap<i64, Window>,
    controls: BTreeMap<i64, Control>,
    ctx: egui::Context,
    /// Texture cache: egui only sends the font atlas when it changes, so the
    /// last whole-texture update is kept across frames.
    textures: HashMap<TextureId, Texture>,
    /// When set, every `present()` writes a PNG here.
    screenshot_path: Option<std::path::PathBuf>,
    width: u32,
    height: u32,
}

impl Default for EguiBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl EguiBackend {
    /// A 640×480 offscreen renderer.
    pub fn new() -> Self {
        Self {
            windows: BTreeMap::new(),
            controls: BTreeMap::new(),
            ctx: egui::Context::default(),
            textures: HashMap::new(),
            screenshot_path: None,
            width: 640,
            height: 480,
        }
    }

    /// Change the offscreen surface size.
    pub fn with_size(mut self, width: u32, height: u32) -> Self {
        self.width = width.max(1);
        self.height = height.max(1);
        self
    }

    /// Write a PNG on every `present()` (which `GUISetState` triggers), so a
    /// run can be captured without the caller holding the emulation.
    pub fn with_screenshot(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.screenshot_path = Some(path.into());
        self
    }

    /// Lay the current model out and return the pixels.
    pub fn render(&mut self) -> GuiImage {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(self.width as f32, self.height as f32),
            )),
            // A fixed time keeps the frame deterministic.
            time: Some(0.0),
            ..Default::default()
        };
        // Snapshot the model first so the closure borrows nothing from `self`.
        let frames: Vec<(Window, Vec<Control>)> = self
            .windows
            .values()
            // AutoIt windows start hidden and only appear on `GUISetState`.
            .filter(|window| window.visible)
            .map(|window| {
                let controls = window
                    .controls
                    .iter()
                    .filter_map(|id| self.controls.get(id).cloned())
                    .collect();
                (window.clone(), controls)
            })
            .collect();

        // `run_ui` loops the (multi-pass) layout for us; `begin_pass`/`end_pass`
        // would hand back the first, no-op pass.
        // Place each AutoIt window at its own rectangle. (`egui::Window`/`Area`
        // come back as no-op shapes in this headless pass, while a scoped Ui at
        // an explicit rect tessellates normally.)
        let output = self.ctx.run_ui(raw, |ui| {
            for (window, controls) in &frames {
                let rect = Rect::from_min_size(
                    Pos2::new(window.x as f32, window.y as f32),
                    vec2(
                        window.width.max(1) as f32,
                        window.height.max(1) as f32,
                    ),
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    egui::Frame::window(ui.style()).show(ui, |ui| {
                        ui.set_min_size(rect.size());
                        let title = if window.title.is_empty() {
                            "AutoIt"
                        } else {
                            window.title.as_str()
                        };
                        ui.heading(title);
                        if controls.is_empty() {
                            ui.label("(empty window)");
                        }
                        for control in controls {
                            draw_control(ui, control);
                        }
                    });
                });
            }
        });
        let pixels_per_point = output.pixels_per_point;
        // egui panics if a `TexturesDelta` is dropped unapplied, so copy what
        // we need into the cache and mark the deltas handled.
        let mut textures_delta = output.textures_delta;
        apply_textures(&mut self.textures, &textures_delta);
        textures_delta.clear();
        let primitives = self.ctx.tessellate(output.shapes, pixels_per_point);
        rasterize(
            &primitives,
            &self.textures,
            self.width,
            self.height,
            pixels_per_point,
        )
    }

    /// Render and write the frame to `path` as a PNG.
    pub fn screenshot(&mut self, path: impl AsRef<std::path::Path>) -> std::io::Result<()> {
        let image = self.render();
        crate::write_png(path, image.width, image.height, &image.rgba)
    }
}

impl GuiBackend for EguiBackend {
    fn on_window(&mut self, window: &Window) {
        self.windows.insert(window.handle, window.clone());
    }

    fn on_window_removed(&mut self, handle: i64) {
        self.windows.remove(&handle);
        self.controls.retain(|_, control| control.window != handle);
    }

    fn on_control(&mut self, control: &Control) {
        self.controls.insert(control.id, control.clone());
    }

    fn on_control_removed(&mut self, id: i64) {
        self.controls.remove(&id);
    }

    fn snapshot(&mut self) -> Option<GuiImage> {
        Some(self.render())
    }

    fn present(&mut self) {
        if let Some(path) = self.screenshot_path.clone() {
            let _ = self.screenshot(&path);
        }
    }
}

#[allow(irrefutable_let_patterns)]
fn apply_textures(
    textures: &mut HashMap<TextureId, Texture>,
    delta: &egui::TexturesDelta,
) {
    for (id, deltas) in &delta.set {
        for image_delta in deltas {
            // Whole-texture updates only; offscreen frames never patch the atlas.
            if image_delta.pos.is_some() {
                continue;
            }
            if let egui::ImageData::Color(image) = &image_delta.image {
                textures.insert(
                    *id,
                    Texture {
                        width: image.size[0],
                        height: image.size[1],
                        pixels: image.pixels.clone(),
                    },
                );
            }
        }
    }
}

/// Draw one control with the closest egui widget.
fn draw_control(ui: &mut egui::Ui, control: &Control) {
    match control.kind {
        ControlKind::Label => {
            ui.label(&control.text);
        }
        ControlKind::Button => {
            let _ = ui.button(&control.text);
        }
        ControlKind::Checkbox => {
            let mut checked = control.is_checked();
            ui.checkbox(&mut checked, &control.text);
        }
        ControlKind::Radio => {
            let checked = control.is_checked();
            let _ = ui.radio(checked, &control.text);
        }
        ControlKind::Input | ControlKind::Edit => {
            let mut text = control.text.clone();
            ui.text_edit_singleline(&mut text);
        }
        ControlKind::Group => {
            ui.group(|ui| {
                ui.label(&control.text);
            });
        }
        ControlKind::Progress => {
            let value = (control.text.parse::<f32>().unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
            ui.add(egui::ProgressBar::new(value));
        }
        ControlKind::Slider => {
            let mut value = 0.0f32;
            ui.add(egui::Slider::new(&mut value, 0.0..=100.0));
        }
        ControlKind::List | ControlKind::Combo => {
            if control.data.is_empty() {
                ui.label(&control.text);
            } else {
                ui.label(control.data.join(", "));
            }
        }
        ControlKind::ListView | ControlKind::TreeView => {
            for item in &control.data {
                ui.label(item);
            }
        }
        ControlKind::Pic | ControlKind::Icon | ControlKind::Graphic => {
            let label = if control.text.is_empty() {
                "[graphic]"
            } else {
                control.text.as_str()
            };
            ui.label(label);
        }
        ControlKind::Menu
        | ControlKind::MenuItem
        | ControlKind::ContextMenu
        | ControlKind::Dummy => {}
        _ => {
            if !control.text.is_empty() {
                ui.label(&control.text);
            }
        }
    }
}

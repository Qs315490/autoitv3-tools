//! Offscreen egui renderer for the AutoIt GUI model.
//!
//! The AutoIt GUI *semantics* live in `autoitv3-platform`'s `winemu::gui` and
//! the widget model plus [`autoitv3_gui::GuiBackend`] seam live in
//! `autoitv3-gui`. This crate implements that seam with egui, rendering the
//! model to an offscreen RGBA buffer (and PNG), so a run can be *seen* without
//! a display server.
//!
//! The renderer is behind the `egui` feature and **off by default**, so the
//! workspace's normal `cargo test` never pulls a GUI/font stack:
//!
//! ```toml
//! autoitv3-gui-egui = { path = "crates/autoitv3-gui-egui", features = ["egui"] }
//! ```
//!
//! With the feature enabled, `autoitv3-platform`'s `gui-egui` feature wires the
//! backend in through `WindowsEmulation::with_egui_backend()`.

#[cfg(feature = "egui")]
mod png;
#[cfg(feature = "egui")]
mod raster;
#[cfg(feature = "egui")]
mod render;

#[cfg(feature = "egui")]
pub use png::write_png;
#[cfg(feature = "egui")]
pub use raster::rasterize;
#[cfg(feature = "egui")]
pub use render::EguiBackend;

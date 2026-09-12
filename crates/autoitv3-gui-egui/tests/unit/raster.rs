//! Unit tests for `raster`'s CPU rasteriser.
//!
//! Kept out of `raster.rs` so the module reads as implementation; `#[path]`
//! pulls the file back in as a unit-test module, which is what lets it
//! reach private state the module does not expose.

use super::*;
use egui::{Color32, ColorImage, ImageData, TextureId};

/// A one-image delta. Callers must `clear()` it: egui panics if a
/// `TexturesDelta` is dropped with unapplied deltas.
fn delta(image: ColorImage, pos: Option<[usize; 2]>) -> (TextureId, egui::TexturesDelta) {
    let id = TextureId::Managed(0);
    let mut deltas = egui::TexturesDelta::default();
    deltas.set.insert(
        id,
        [egui::epaint::ImageDelta {
            image: ImageData::Color(std::sync::Arc::new(image)),
            pos,
            options: Default::default(),
        }]
        .into_iter()
        .collect(),
    );
    (id, deltas)
}

#[test]
fn a_patch_updates_only_its_rectangle() {
    // egui sends the atlas once and patches it as new glyphs appear; a cache
    // that ignores patches renders early text and drops everything later.
    let mut cache = HashMap::new();
    let (id, mut whole) = delta(
        ColorImage::filled([4, 4], Color32::from_rgb(10, 10, 10)),
        None,
    );
    apply_textures(&mut cache, &whole);
    whole.clear();
    assert_eq!(cache[&id].pixels.len(), 16);

    let (_, mut patch) = delta(
        ColorImage::filled([2, 1], Color32::from_rgb(200, 0, 0)),
        Some([1, 2]),
    );
    apply_textures(&mut cache, &patch);
    patch.clear();

    let texture = &cache[&id];
    let pixel = |x: usize, y: usize| texture.pixels[y * 4 + x];
    assert_eq!(pixel(1, 2), Color32::from_rgb(200, 0, 0), "patched");
    assert_eq!(pixel(2, 2), Color32::from_rgb(200, 0, 0), "patched");
    assert_eq!(pixel(0, 2), Color32::from_rgb(10, 10, 10), "left alone");
    assert_eq!(pixel(1, 3), Color32::from_rgb(10, 10, 10), "row below");
    assert_eq!(pixel(3, 2), Color32::from_rgb(10, 10, 10), "column right");
}

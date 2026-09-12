//! A small CPU rasterizer for egui's tessellated output.
//!
//! egui turns a frame into `ClippedPrimitive`s: triangle meshes with
//! premultiplied sRGBA vertex colours and texture coordinates, plus the font
//! atlas as a texture. Rendering that with a CPU loop keeps screenshots
//! **deterministic and GPU-free**, which is what a test (or a headless run)
//! wants. The result is a straight-alpha RGBA image.

use std::collections::HashMap;

use autoitv3_gui_model::GuiImage;
use egui::epaint::{ClippedPrimitive, Primitive};
use egui::{Color32, TextureId};

/// A CPU-side texture (the font atlas, and any image egui uploaded).
#[derive(Debug, Clone)]
pub struct Texture {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<Color32>,
}

impl Texture {
    fn texel(&self, u: f32, v: f32) -> [f32; 4] {
        if self.width == 0 || self.height == 0 {
            return [1.0, 1.0, 1.0, 1.0];
        }
        let x = (u * self.width as f32) as isize;
        let y = (v * self.height as f32) as isize;
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        let c = self.pixels[y * self.width + x];
        rgba01(c)
    }
}

fn rgba01(c: Color32) -> [f32; 4] {
    [
        f32::from(c.r()) / 255.0,
        f32::from(c.g()) / 255.0,
        f32::from(c.b()) / 255.0,
        f32::from(c.a()) / 255.0,
    ]
}

fn edge(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Rasterize `primitives` into a straight-alpha RGBA image.
/// Fold a frame's texture deltas into the cache [`rasterize`] reads.
///
/// egui sends the whole font atlas once and then *patches* it as new glyphs
/// appear, so a cache that only keeps whole updates renders text that was used
/// early on and silently drops anything typed later (a Cyrillic name, an emoji,
/// a character a script only reaches on page two).
pub fn apply_textures(cache: &mut HashMap<TextureId, Texture>, delta: &egui::TexturesDelta) {
    for (id, deltas) in &delta.set {
        for image_delta in deltas {
            let egui::ImageData::Color(image) = &image_delta.image;
            let (width, height) = (image.size[0], image.size[1]);
            match image_delta.pos {
                None => {
                    cache.insert(
                        *id,
                        Texture {
                            width,
                            height,
                            pixels: image.pixels.clone(),
                        },
                    );
                }
                Some([x, y]) => {
                    // A patch: copy it into the texture we already have.
                    let Some(texture) = cache.get_mut(id) else {
                        continue;
                    };
                    for row in 0..height {
                        let (dst_y, src_y) = (y + row, row);
                        if dst_y >= texture.height {
                            break;
                        }
                        let (dst_start, src_start) =
                            ((dst_y * texture.width + x) * 4, (src_y * width) * 4);
                        let columns = width.min(texture.width.saturating_sub(x));
                        let bytes = columns * 4;
                        // `pixels` is `Color32`, four bytes each.
                        let dst = &mut texture.pixels[dst_start / 4..(dst_start + bytes) / 4];
                        let src = &image.pixels[src_start / 4..(src_start + bytes) / 4];
                        dst.copy_from_slice(src);
                    }
                }
            }
        }
    }
}

pub fn rasterize(
    primitives: &[ClippedPrimitive],
    textures: &HashMap<TextureId, Texture>,
    width: u32,
    height: u32,
    pixels_per_point: f32,
) -> GuiImage {
    let ppp = if pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let w = width as usize;
    let h = height as usize;
    // Premultiplied RGBA accumulator.
    let mut buf = vec![0f32; w * h * 4];

    for prim in primitives {
        let Primitive::Mesh(mesh) = &prim.primitive else {
            continue;
        };
        if mesh.indices.len() < 3 {
            continue;
        }
        let texture = textures.get(&mesh.texture_id);
        // Clip rect, in points -> device pixels, intersected with the image.
        let clip = prim.clip_rect;
        let clip_x0 = (clip.min.x * ppp).floor().max(0.0);
        let clip_y0 = (clip.min.y * ppp).floor().max(0.0);
        let clip_x1 = (clip.max.x * ppp).ceil().min(w as f32);
        let clip_y1 = (clip.max.y * ppp).ceil().min(h as f32);

        for tri in mesh.indices.chunks_exact(3) {
            let (Some(&a), Some(&b), Some(&c)) = (
                mesh.vertices.get(tri[0] as usize),
                mesh.vertices.get(tri[1] as usize),
                mesh.vertices.get(tri[2] as usize),
            ) else {
                continue;
            };
            let pa = [a.pos.x * ppp, a.pos.y * ppp];
            let pb = [b.pos.x * ppp, b.pos.y * ppp];
            let pc = [c.pos.x * ppp, c.pos.y * ppp];
            let area = edge(pa, pb, pc);
            if area.abs() < 1e-6 {
                continue;
            }
            let min_x = pa[0].min(pb[0]).min(pc[0]).floor().max(clip_x0);
            let max_x = pa[0].max(pb[0]).max(pc[0]).ceil().min(clip_x1);
            let min_y = pa[1].min(pb[1]).min(pc[1]).floor().max(clip_y0);
            let max_y = pa[1].max(pb[1]).max(pc[1]).ceil().min(clip_y1);
            if max_x <= min_x || max_y <= min_y {
                continue;
            }
            let ca = rgba01(a.color);
            let cb = rgba01(b.color);
            let cc = rgba01(c.color);
            for py in min_y as usize..max_y as usize {
                for px in min_x as usize..max_x as usize {
                    let q = [px as f32 + 0.5, py as f32 + 0.5];
                    let w0 = edge(pb, pc, q) / area;
                    let w1 = edge(pc, pa, q) / area;
                    let w2 = edge(pa, pb, q) / area;
                    if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                        continue;
                    }
                    // Premultiplied linear interpolation (what egui's shader does).
                    let mut src = [0f32; 4];
                    for i in 0..4 {
                        src[i] = ca[i] * w0 + cb[i] * w1 + cc[i] * w2;
                    }
                    if let Some(texture) = texture {
                        let u = a.uv.x * w0 + b.uv.x * w1 + c.uv.x * w2;
                        let v = a.uv.y * w0 + b.uv.y * w1 + c.uv.y * w2;
                        let t = texture.texel(u, v);
                        for i in 0..4 {
                            src[i] *= t[i];
                        }
                    }
                    let idx = (py * w + px) * 4;
                    let keep = 1.0 - src[3];
                    for i in 0..4 {
                        buf[idx + i] = src[i] + buf[idx + i] * keep;
                    }
                }
            }
        }
    }

    // Un-premultiply for the output image.
    let mut rgba = vec![0u8; w * h * 4];
    for (pixel, out) in buf.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
        let a = pixel[3];
        if a <= 0.0 {
            continue;
        }
        for i in 0..3 {
            out[i] = ((pixel[i] / a).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        out[3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
    }

    GuiImage {
        width,
        height,
        rgba,
    }
}

// Unit tests live in `tests/unit/` so this file reads as implementation;
// `#[path]` pulls them back in as a test module, which is what keeps their
// access to the private state below.
#[cfg(test)]
#[path = "../tests/unit/raster.rs"]
mod tests;

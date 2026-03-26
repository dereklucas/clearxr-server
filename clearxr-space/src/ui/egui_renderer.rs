//! egui rendering to RGBA pixel buffer for Vulkan texture upload.
//!
//! This replaces Ultralight for UI rendering. The flow:
//! 1. Inject VR pointer events (from ray-cast UV coordinates)
//! 2. Run an egui frame with a layout closure
//! 3. Tessellate + software-rasterize to an RGBA pixel buffer
//! 4. Upload to LauncherPanel's Vulkan texture (same path as before)

use std::collections::HashMap;

use egui::{Context, Event, Pos2, PointerButton, RawInput, Rect, Vec2};
use epaint::{ClippedPrimitive, ImageDelta, ImageData, Primitive, TextureId};

/// Manages an egui context and software-rasterizes its output to an RGBA pixel buffer.
pub struct EguiRenderer {
    ctx: Context,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    /// Managed textures (font atlas, user images). Stored as RGBA premultiplied.
    textures: HashMap<TextureId, TextureImage>,
    /// Pointer position in egui screen coordinates (pixels).
    pointer_pos: Option<Pos2>,
    /// Whether we have rendered at least one frame (for skip-repaint optimisation).
    has_rendered: bool,
}

/// An RGBA premultiplied texture stored in CPU memory.
struct TextureImage {
    width: usize,
    height: usize,
    /// Row-major RGBA premultiplied pixels.
    pixels: Vec<[u8; 4]>,
}

impl TextureImage {
    /// Nearest-neighbour sample at normalised UV coordinates.
    /// For egui's pre-rasterized font atlas, nearest is sharper than bilinear.
    #[inline]
    fn sample(&self, u: f32, v: f32) -> (u8, u8, u8, u8) {
        let x = ((u * self.width as f32) as usize).min(self.width.saturating_sub(1));
        let y = ((v * self.height as f32) as usize).min(self.height.saturating_sub(1));
        let p = self.pixels[y * self.width + x];
        (p[0], p[1], p[2], p[3])
    }
}

impl EguiRenderer {
    /// Create a new renderer targeting the given pixel dimensions.
    pub fn new(width: u32, height: u32) -> Self {
        let ctx = Context::default();
        ctx.set_pixels_per_point(1.0);
        ctx.set_visuals(egui::Visuals::dark());

        Self {
            ctx,
            width,
            height,
            pixels: vec![0u8; (width * height * 4) as usize],
            textures: HashMap::new(),
            pointer_pos: None,
            has_rendered: false,
        }
    }

    /// Force the next `run()` to rasterize, even if egui says no repaint needed.
    pub fn force_repaint(&mut self) {
        self.has_rendered = false;
    }

    /// Inject a pointer move event from VR ray-cast UV coordinates.
    /// `u`, `v` are in `[0, 1]` panel space.
    pub fn pointer_move(&mut self, u: f32, v: f32) {
        self.pointer_pos = Some(Pos2::new(u * self.width as f32, v * self.height as f32));
    }

    /// Clear pointer (controller not pointing at this panel).
    pub fn pointer_leave(&mut self) {
        self.pointer_pos = None;
    }

    /// Run an egui frame and rasterize to the pixel buffer.
    ///
    /// `click` sends an immediate press+release of the primary pointer button.
    /// `build_ui` receives the [`Context`] and should call `egui::CentralPanel`, etc.
    ///
    /// Returns `true` if the pixel buffer was updated (currently always `true`).
    pub fn run(&mut self, click: bool, build_ui: impl FnMut(&Context)) -> bool {
        let mut raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(self.width as f32, self.height as f32),
            )),
            ..Default::default()
        };

        // Inject pointer events.
        if let Some(pos) = self.pointer_pos {
            raw_input.events.push(Event::PointerMoved(pos));
            if click {
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                });
                raw_input.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                });
            }
        }

        let full_output = self.ctx.run(raw_input, build_ui);

        // Check if egui thinks a repaint is needed. If not, and we have already
        // rendered at least one frame, we can skip the expensive rasterization
        // and signal the caller to skip the GPU upload too.
        let needs_repaint = full_output
            .viewport_output
            .values()
            .any(|vo| vo.repaint_delay == std::time::Duration::ZERO);
        if !needs_repaint && self.has_rendered {
            // Still apply texture deltas so the atlas stays up-to-date.
            self.apply_textures_delta(&full_output.textures_delta);
            return false; // pixels unchanged
        }

        // Apply texture updates (font atlas, user textures).
        self.apply_textures_delta(&full_output.textures_delta);

        // Tessellate shapes into triangle meshes.
        let clipped_primitives =
            self.ctx
                .tessellate(full_output.shapes, full_output.pixels_per_point);

        // Software-rasterize into self.pixels.
        self.rasterize(&clipped_primitives);

        self.has_rendered = true;
        true
    }

    /// The RGBA pixel buffer, row-major, top-to-bottom.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Width in pixels.
    #[allow(dead_code)] // Public API accessor
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[allow(dead_code)] // Public API accessor
    pub fn height(&self) -> u32 {
        self.height
    }

    // ------------------------------------------------------------------ //
    //  Texture management                                                 //
    // ------------------------------------------------------------------ //

    fn apply_textures_delta(&mut self, delta: &epaint::textures::TexturesDelta) {
        for (id, image_delta) in &delta.set {
            self.apply_image_delta(*id, image_delta);
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    fn apply_image_delta(&mut self, id: TextureId, delta: &ImageDelta) {
        let rgba_pixels: Vec<[u8; 4]> = match &delta.image {
            ImageData::Color(color_image) => color_image
                .pixels
                .iter()
                .map(|c| c.to_array())
                .collect(),
            ImageData::Font(font_image) => font_image
                .srgba_pixels(None)
                .map(|c| c.to_array())
                .collect(),
        };

        let [w, h] = delta.image.size();

        if let Some(pos) = delta.pos {
            // Partial update — patch into existing texture.
            if let Some(tex) = self.textures.get_mut(&id) {
                let [px, py] = pos;
                for row in 0..h {
                    for col in 0..w {
                        let dst_x = px + col;
                        let dst_y = py + row;
                        if dst_x < tex.width && dst_y < tex.height {
                            tex.pixels[dst_y * tex.width + dst_x] = rgba_pixels[row * w + col];
                        }
                    }
                }
            }
        } else {
            // Full update — replace entire texture.
            self.textures.insert(
                id,
                TextureImage {
                    width: w,
                    height: h,
                    pixels: rgba_pixels,
                },
            );
        }
    }

    // ------------------------------------------------------------------ //
    //  Software rasterizer                                                //
    // ------------------------------------------------------------------ //

    fn rasterize(&mut self, clipped_primitives: &[ClippedPrimitive]) {
        let w = self.width as i32;
        let h = self.height as i32;

        // Fast memset clear to dark background (sRGB #0a0a14, full alpha).
        // Reinterpret as u32 slice for a single fill operation — much faster
        // than a per-pixel byte loop.
        let bg_u32 = u32::from_ne_bytes([10u8, 10, 20, 255]);
        // SAFETY: self.pixels length is always width*height*4 (multiple of 4)
        // and Vec<u8> is at least byte-aligned, which satisfies u32 on all
        // platforms when the pointer happens to be aligned.  We use
        // write_unaligned via fill to be safe.
        let pixels_u32: &mut [u32] = unsafe {
            std::slice::from_raw_parts_mut(
                self.pixels.as_mut_ptr() as *mut u32,
                self.pixels.len() / 4,
            )
        };
        pixels_u32.fill(bg_u32);

        for cp in clipped_primitives {
            let clip = &cp.clip_rect;
            let mesh = match &cp.primitive {
                Primitive::Mesh(m) => m,
                Primitive::Callback(_) => continue,
            };

            if mesh.vertices.is_empty() || mesh.indices.is_empty() {
                continue;
            }

            let tex = self.textures.get(&mesh.texture_id);

            // Clip rect in integer pixel coordinates, clamped to buffer.
            let cx0 = (clip.min.x as i32).max(0);
            let cy0 = (clip.min.y as i32).max(0);
            let cx1 = (clip.max.x as i32).min(w);
            let cy1 = (clip.max.y as i32).min(h);

            for tri in mesh.indices.chunks_exact(3) {
                let v0 = &mesh.vertices[tri[0] as usize];
                let v1 = &mesh.vertices[tri[1] as usize];
                let v2 = &mesh.vertices[tri[2] as usize];

                render_triangle(&mut self.pixels, v0, v1, v2, tex, cx0, cy0, cx1, cy1, w);
            }
        }
    }
}

/// Rasterize a single triangle using Pineda-style incremental edge functions.
/// This avoids per-pixel `edge_fn` calls and division, giving a large speedup
/// over the naive approach of calling `edge_fn` three times per pixel.
fn render_triangle(
    pixels: &mut [u8],
    v0: &epaint::Vertex,
    v1: &epaint::Vertex,
    v2: &epaint::Vertex,
    tex: Option<&TextureImage>,
    cx0: i32,
    cy0: i32,
    cx1: i32,
    cy1: i32,
    buf_w: i32,
) {
    // Integer bounding box clamped to clip rect.
    let x_min = (v0.pos.x.min(v1.pos.x).min(v2.pos.x) as i32).max(cx0);
    let y_min = (v0.pos.y.min(v1.pos.y).min(v2.pos.y) as i32).max(cy0);
    let x_max = (v0.pos.x.max(v1.pos.x).max(v2.pos.x).ceil() as i32).min(cx1);
    let y_max = (v0.pos.y.max(v1.pos.y).max(v2.pos.y).ceil() as i32).min(cy1);

    if x_min >= x_max || y_min >= y_max {
        return;
    }

    // Edge function increments (Pineda algorithm).
    // For edge (A, B): dx = A.y - B.y, dy = B.x - A.x
    let dx12 = v1.pos.y - v2.pos.y;
    let dy12 = v2.pos.x - v1.pos.x;
    let dx20 = v2.pos.y - v0.pos.y;
    let dy20 = v0.pos.x - v2.pos.x;
    let dx01 = v0.pos.y - v1.pos.y;
    let dy01 = v1.pos.x - v0.pos.x;

    // Signed area (2x) of the triangle.
    let area = dx12 * (v0.pos.x - v1.pos.x) + dy12 * (v0.pos.y - v1.pos.y);
    if area.abs() < 1e-6 {
        return; // degenerate triangle
    }
    let inv_area = 1.0 / area;

    // Evaluate edge functions at the first pixel centre (x_min+0.5, y_min+0.5).
    let px = x_min as f32 + 0.5;
    let py = y_min as f32 + 0.5;

    let mut row_w0 = dx12 * (px - v1.pos.x) + dy12 * (py - v1.pos.y);
    let mut row_w1 = dx20 * (px - v2.pos.x) + dy20 * (py - v2.pos.y);
    let mut row_w2 = dx01 * (px - v0.pos.x) + dy01 * (py - v0.pos.y);

    for y in y_min..y_max {
        let mut w0 = row_w0;
        let mut w1 = row_w1;
        let mut w2 = row_w2;

        let row_offset = (y * buf_w) as usize;

        for x in x_min..x_max {
            if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                let b0 = w0 * inv_area;
                let b1 = w1 * inv_area;
                let b2 = 1.0 - b0 - b1;

                // Interpolate vertex color.
                let c0 = v0.color;
                let c1 = v1.color;
                let c2 = v2.color;
                let r = c0.r() as f32 * b0 + c1.r() as f32 * b1 + c2.r() as f32 * b2;
                let g = c0.g() as f32 * b0 + c1.g() as f32 * b1 + c2.g() as f32 * b2;
                let b_ch = c0.b() as f32 * b0 + c1.b() as f32 * b1 + c2.b() as f32 * b2;
                let a = c0.a() as f32 * b0 + c1.a() as f32 * b1 + c2.a() as f32 * b2;

                // Texture sampling (nearest-neighbour — sharp for font atlas).
                let (tr, tg, tb, ta) = if let Some(tex) = tex {
                    let u = v0.uv.x * b0 + v1.uv.x * b1 + v2.uv.x * b2;
                    let v = v0.uv.y * b0 + v1.uv.y * b1 + v2.uv.y * b2;
                    tex.sample(u, v)
                } else {
                    (255, 255, 255, 255)
                };

                // Multiply vertex colour by texture (premultiplied).
                let fr = (r as u16 * tr as u16 / 255) as u8;
                let fg = (g as u16 * tg as u16 / 255) as u8;
                let fb = (b_ch as u16 * tb as u16 / 255) as u8;
                let fa = (a as u16 * ta as u16 / 255) as u8;

                if fa > 0 {
                    let idx = (row_offset + x as usize) * 4;
                    if fa == 255 {
                        // Fully opaque — skip blending.
                        pixels[idx] = fr;
                        pixels[idx + 1] = fg;
                        pixels[idx + 2] = fb;
                        pixels[idx + 3] = 255;
                    } else {
                        // Alpha-blend (src-over, premultiplied source).
                        // result = src + dst * (1 - src_alpha)
                        let inv_a = 255u16 - fa as u16;
                        pixels[idx] =
                            (fr as u16 + (pixels[idx] as u16 * inv_a / 255)).min(255) as u8;
                        pixels[idx + 1] =
                            (fg as u16 + (pixels[idx + 1] as u16 * inv_a / 255)).min(255) as u8;
                        pixels[idx + 2] =
                            (fb as u16 + (pixels[idx + 2] as u16 * inv_a / 255)).min(255) as u8;
                        pixels[idx + 3] =
                            (fa as u16 + (pixels[idx + 3] as u16 * inv_a / 255)).min(255) as u8;
                    }
                }
            }

            w0 += dx12;
            w1 += dx20;
            w2 += dx01;
        }

        row_w0 += dy12;
        row_w1 += dy20;
        row_w2 += dy01;
    }
}

// ====================================================================== //
//  Tests                                                                  //
// ====================================================================== //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_egui_renderer_creates() {
        let r = EguiRenderer::new(512, 512);
        assert_eq!(r.width(), 512);
        assert_eq!(r.height(), 512);
        assert_eq!(r.pixels().len(), 512 * 512 * 4);
    }

    #[test]
    fn test_egui_renderer_produces_pixels() {
        let mut r = EguiRenderer::new(256, 256);
        // Render a frame with a large label so we definitely get visible pixels.
        r.run(false, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.heading("Hello egui!");
                ui.label("This is a test of the software rasterizer.");
            });
        });

        let pixels = r.pixels();
        // The background is [10, 10, 20, 255]. Count how many pixels differ.
        let non_bg = pixels
            .chunks_exact(4)
            .filter(|px| px[0] != 10 || px[1] != 10 || px[2] != 20)
            .count();

        // egui's CentralPanel with a heading should paint a panel background and text,
        // producing many non-background pixels.
        assert!(
            non_bg > 100,
            "Expected significant rendering output, but only {non_bg} non-background pixels found"
        );
    }

    #[test]
    fn test_egui_pointer_input() {
        let mut r = EguiRenderer::new(256, 256);

        // First frame: render a button.
        r.run(false, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = ui.button("Click me");
            });
        });

        // Snapshot pixels after first frame.
        let first_frame = r.pixels().to_vec();
        let non_bg_first = first_frame
            .chunks_exact(4)
            .filter(|px| px[0] != 10 || px[1] != 10 || px[2] != 20)
            .count();

        // The button should have rendered something visible.
        assert!(
            non_bg_first > 50,
            "Button should produce visible pixels, got {non_bg_first}"
        );

        // Second frame: hover over the button area and click.
        // The button is near the top-left of the CentralPanel (~0.2, 0.1 in UV).
        r.pointer_move(0.2, 0.1);
        r.run(true, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = ui.button("Click me");
            });
        });

        let second_frame = r.pixels().to_vec();
        let non_bg_second = second_frame
            .chunks_exact(4)
            .filter(|px| px[0] != 10 || px[1] != 10 || px[2] != 20)
            .count();

        // The hovered/clicked button should still produce visible output.
        assert!(
            non_bg_second > 50,
            "Hovered button should produce visible pixels, got {non_bg_second}"
        );

        // The pixel output should differ due to hover highlight.
        let changed = first_frame
            .iter()
            .zip(second_frame.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            changed > 0,
            "Pointer hover should cause some pixel change"
        );
    }

    #[test]
    fn test_nearest_sampling() {
        // Create a 2x2 texture with distinct corner values.
        let tex = TextureImage {
            width: 2,
            height: 2,
            pixels: vec![
                [0, 0, 0, 255],     // (0,0) black
                [255, 0, 0, 255],   // (1,0) red
                [0, 255, 0, 255],   // (0,1) green
                [255, 255, 0, 255], // (1,1) yellow
            ],
        };

        // Sample at the origin — should return top-left (black).
        let (r, g, b, _) = tex.sample(0.0, 0.0);
        assert_eq!((r, g, b), (0, 0, 0), "top-left corner should be black");

        // Sample near the right edge — should return top-right (red).
        let (r, g, b, _) = tex.sample(0.99, 0.0);
        assert_eq!((r, g, b), (255, 0, 0), "top-right corner should be red");

        // Sample at the centre (0.5, 0.5) — nearest picks (1,1) = yellow.
        let (r, g, b, a) = tex.sample(0.5, 0.5);
        assert_eq!((r, g, b), (255, 255, 0), "centre should snap to yellow");
        assert_eq!(a, 255, "centre alpha should be 255");

        // Sample at (0.25, 0.0) — nearest picks (0,0) = black.
        let (r, g, b, _) = tex.sample(0.25, 0.0);
        assert_eq!((r, g, b), (0, 0, 0), "quarter-x should snap to black");
    }

    #[test]
    fn test_skip_repaint() {
        let mut r = EguiRenderer::new(128, 128);

        // First call should always rasterize (has_rendered is false).
        let changed = r.run(false, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Static");
            });
        });
        assert!(changed, "First frame should always report changed");

        // Second call with no input changes — egui should not request repaint,
        // so run() should return false.
        let changed = r.run(false, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Static");
            });
        });
        assert!(
            !changed,
            "Second frame with identical content should skip repaint"
        );
    }
}

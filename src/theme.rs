use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Shape, Vec2};
use std::sync::Arc;

/// Artwork-derived colors and the animated background used behind the player controls.
#[derive(Clone, Debug)]
pub struct Theme {
    palette: [Color32; 4],
    color_field: Option<Arc<ColorField>>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            palette: [Color32::BLACK; 4],
            color_field: None,
        }
    }
}

impl Theme {
    pub fn from_image(image: &image::DynamicImage) -> Self {
        let color_field = Arc::new(ColorField::from_image(image));
        let small = image
            .resize_exact(64, 64, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut bins = vec![(0u32, [0u64; 3]); 32 * 32 * 32];
        let mut colorful = 0.0f32;
        for pixel in small.pixels() {
            let rgb = [pixel[0], pixel[1], pixel[2]];
            let key = ((rgb[0] as usize >> 3) << 10)
                | ((rgb[1] as usize >> 3) << 5)
                | (rgb[2] as usize >> 3);
            bins[key].0 += 1;
            for i in 0..3 {
                bins[key].1[i] += rgb[i] as u64;
            }
        }
        let total = (64 * 64) as f32;
        let mut candidates = Vec::new();
        for (count, sums) in bins.into_iter().filter(|b| b.0 > 0) {
            let rgb = [
                sums[0] as f32 / count as f32 / 255.0,
                sums[1] as f32 / count as f32 / 255.0,
                sums[2] as f32 / count as f32 / 255.0,
            ];
            let (h, s, l) = rgb_to_hsl(rgb);
            let pop = count as f32 / total;
            if pop >= 0.008 && (0.05..=0.94).contains(&l) && s >= 0.14 {
                colorful += pop;
            }
            if !(0.03..=0.96).contains(&l) {
                continue;
            }
            let sat_w = (s / 0.62).min(1.0);
            let lum_w = if (0.16..=0.76).contains(&l) {
                1.0
            } else {
                (1.0 - ((l - 0.16).abs().min((l - 0.76).abs()) * 2.4)).max(0.2)
            };
            let score = (pop.sqrt() * 0.44 + sat_w * 0.38 + lum_w * 0.18)
                * if s < 0.10 { 0.25 } else { 1.0 };
            candidates.push(Candidate {
                h,
                s,
                l,
                rgb,
                score,
            });
        }
        if candidates.is_empty() {
            return Self {
                color_field: Some(color_field),
                ..Self::default()
            };
        }
        candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
        if colorful < 0.025 && candidates.iter().map(|c| c.s).fold(0.0, f32::max) < 0.11 {
            let c = candidates[0];
            let s = (c.s * 0.8).clamp(0.0, 0.08);
            return Self {
                palette: [
                    hsl_color(c.h, s, 0.15),
                    hsl_color(c.h, s, 0.29),
                    hsl_color((c.h + 0.04) % 1.0, s, 0.21),
                    hsl_color(c.h, s, 0.36),
                ],
                color_field: Some(color_field),
            };
        }
        let mut chosen = vec![candidates[0]];
        while chosen.len() < 4 {
            let mut best = candidates[0];
            let mut best_score = f32::NEG_INFINITY;
            for c in &candidates {
                let distance = chosen
                    .iter()
                    .map(|p| {
                        let dh = (c.h - p.h).abs().min(1.0 - (c.h - p.h).abs()) * 2.0;
                        let drgb = ((c.rgb[0] - p.rgb[0]).powi(2)
                            + (c.rgb[1] - p.rgb[1]).powi(2)
                            + (c.rgb[2] - p.rgb[2]).powi(2))
                        .sqrt();
                        dh * 0.65 + drgb * 0.55 + (c.l - p.l).abs() * 0.35
                    })
                    .fold(f32::INFINITY, f32::min);
                let score = c.score * (0.28 + 0.72 * (distance / 0.42).min(1.0));
                if score > best_score {
                    best_score = score;
                    best = *c;
                }
            }
            chosen.push(best);
        }
        let (h0, s0, l0) = (chosen[0].h, chosen[0].s, chosen[0].l);
        let mut h1 = chosen[1].h;
        if hue_distance(h1, h0) < 0.035 && (chosen[1].l - l0).abs() < 0.08 {
            h1 = (h0 + 0.045) % 1.0;
        }
        let mut h2 = chosen[2].h;
        if hue_distance(h2, h0) < 0.04 && hue_distance(h2, h1) < 0.04 {
            h2 = (h0 + 0.945) % 1.0;
        }
        let mut h3 = chosen[3].h;
        if hue_distance(h3, h1) < 0.035 {
            h3 = (h1 + 0.06) % 1.0;
        }
        let mut palette = [
            hsl_color(
                h0,
                (s0 * 1.18).clamp(0.42, 0.78),
                (if l0 < 0.26 { l0 } else { 0.20 }).clamp(0.15, 0.22),
            ),
            hsl_color(
                h1,
                (chosen[1].s.max(s0) * 1.30).clamp(0.58, 0.90),
                (if (0.28..=0.38).contains(&chosen[1].l) {
                    chosen[1].l
                } else {
                    0.33
                })
                .clamp(0.27, 0.38),
            ),
            hsl_color(
                h2,
                (chosen[2].s.max(s0 * 0.9) * 1.25).clamp(0.52, 0.88),
                (if (0.23..=0.34).contains(&chosen[2].l) {
                    chosen[2].l
                } else {
                    0.27
                })
                .clamp(0.22, 0.34),
            ),
            hsl_color(
                h3,
                (chosen[3].s.max(s0) * 1.35).clamp(0.62, 0.94),
                (if (0.32..=0.42).contains(&chosen[3].l) {
                    chosen[3].l
                } else {
                    0.37
                })
                .clamp(0.31, 0.42),
            ),
        ];
        for color in &mut palette {
            *color = darken_for_contrast(*color, 0.18);
        }
        Self {
            palette,
            color_field: Some(color_field),
        }
    }

    /// Paint a slowly moving, soft artwork-colored field. The top edge stays flat and
    /// the lower corners are rounded; `opacity` supports track-change crossfades.
    /// Variant for layouts that animate their corner radius (for example, a compact mini notch).
    pub fn paint_with_radius(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        time: f32,
        opacity: f32,
        round_radius: f32,
    ) {
        let w = rect.width().max(0.0);
        let h = rect.height().max(0.0);
        if w <= 0.0 || h <= 0.0 || opacity <= 0.0 {
            return;
        }
        let radius = round_radius.max(0.0).min(h * 0.5).min(w * 0.25);
        let Some(field) = &self.color_field else {
            painter.rect_filled(
                rect,
                egui::CornerRadius {
                    sw: radius.round() as u8,
                    se: radius.round() as u8,
                    ..Default::default()
                },
                self.palette[0].linear_multiply(opacity.clamp(0.0, 1.0)),
            );
            return;
        };
        let columns = ((w / 14.0).ceil() as usize).clamp(8, 32);
        let body_rows = ((h - radius) / 9.0).ceil() as usize;
        let corner_rows = if radius > 0.0 { 4 } else { 0 };
        let rows = body_rows + corner_rows;
        let mut mesh = Mesh {
            vertices: Vec::with_capacity((columns + 1) * (rows + 1)),
            indices: Vec::with_capacity(6 * columns * rows),
            ..Mesh::default()
        };
        let phase = time * 0.22;
        let alpha = (opacity.clamp(0.0, 1.0) * 255.0) as u8;
        for y in 0..=rows {
            let yy = if y <= body_rows {
                (h - radius) * (y as f32 / body_rows as f32)
            } else {
                (h - radius) + radius * ((y - body_rows) as f32 / corner_rows as f32)
            };
            let fy = yy / h;
            let inset = if radius > 0.0 && yy > h - radius {
                let dy = yy - (h - radius);
                (radius - (radius * radius - dy * dy).max(0.0).sqrt()).min(radius)
            } else {
                0.0
            };
            let left = inset;
            let right = w - inset;
            let wave = 0.055 * (fy * 7.0 + phase).sin();
            let edge_shade = 0.93 - 0.11 * fy;
            for x in 0..=columns {
                let fx = x as f32 / columns as f32;
                let xx = left + (right - left) * fx;
                // Gently warp the blurred artwork field. A few low-frequency waves
                // make it feel fluid without the obvious pulsing of moving blobs.
                let u = fx + wave + 0.022 * (fx * 5.0 - fy * 3.0 - phase * 0.73).sin();
                let v = fy
                    + 0.105 * (fx * 5.2 - phase * 0.81).sin()
                    + 0.028 * (fy * 9.0 + fx * 2.5 + phase * 0.56).cos();
                let mut rgb = field.sample(u, v);
                for channel in &mut rgb {
                    *channel = (*channel * edge_shade).clamp(0.0, 1.0);
                }
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: rect.min + Vec2::new(xx, yy),
                    uv: Pos2::ZERO,
                    color: Color32::from_rgba_unmultiplied(
                        (rgb[0] * 255.0) as u8,
                        (rgb[1] * 255.0) as u8,
                        (rgb[2] * 255.0) as u8,
                        alpha,
                    ),
                });
            }
        }
        for y in 0..rows {
            for x in 0..columns {
                let a = (y * (columns + 1) + x) as u32;
                let b = a + 1;
                let c = a + (columns + 1) as u32;
                let d = c + 1;
                mesh.indices.extend_from_slice(&[a, b, c, b, d, c]);
            }
        }
        // Keep the mesh inside the silhouette, including the flat top edge.
        let clip = Rect::from_min_max(rect.min, rect.max);
        painter.with_clip_rect(clip).add(Shape::mesh(mesh));
    }
}

#[derive(Clone, Debug)]
struct ColorField {
    width: usize,
    height: usize,
    pixels: Arc<[[f32; 3]]>,
}

impl ColorField {
    const WIDTH: u32 = 256;
    const HEIGHT: u32 = 96;

    fn from_image(image: &image::DynamicImage) -> Self {
        let source = image
            .resize_exact(128, 128, image::imageops::FilterType::Lanczos3)
            .to_rgb8();
        let mut field =
            image::RgbImage::from_pixel(Self::WIDTH, Self::HEIGHT, image::Rgb([9, 10, 15]));

        // These overlapping, rotated copies retain the cover's actual color layout.
        // Blur then removes faces, text and sharp artwork edges before display.
        let layers = [
            (1.25, 0.50, 0.50, -0.10_f32, 1.00),
            (0.80, 0.27, 0.54, 0.18, 0.42),
            (0.50, 0.76, 0.43, -0.23, 0.34),
            (0.25, 0.57, 0.69, 0.36, 0.30),
        ];
        for (scale, center_x, center_y, angle, opacity) in layers {
            let side = Self::WIDTH as f32 * scale;
            let cx = Self::WIDTH as f32 * center_x;
            let cy = Self::HEIGHT as f32 * center_y;
            let (sin, cos) = angle.sin_cos();
            for y in 0..Self::HEIGHT {
                for x in 0..Self::WIDTH {
                    let dx = x as f32 + 0.5 - cx;
                    let dy = y as f32 + 0.5 - cy;
                    let u = (cos * dx + sin * dy) / side + 0.5;
                    let v = (-sin * dx + cos * dy) / side + 0.5;
                    if let Some(color) = sample_rgb(&source, u, v) {
                        let pixel = field.get_pixel_mut(x, y);
                        for channel in 0..3 {
                            pixel[channel] = (pixel[channel] as f32 * (1.0 - opacity)
                                + color[channel] * opacity)
                                .round() as u8;
                        }
                    }
                }
            }
        }

        let blurred = image::imageops::blur(&field, 22.0);
        let pixels = blurred
            .pixels()
            .map(|pixel| {
                let mut rgb = [
                    pixel[0] as f32 / 255.0,
                    pixel[1] as f32 / 255.0,
                    pixel[2] as f32 / 255.0,
                ];
                let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
                for channel in &mut rgb {
                    *channel = (luma + (*channel - luma) * 1.45).clamp(0.0, 1.0);
                    *channel *= 0.70;
                }
                darken_rgb_for_text(&mut rgb, 0.14);
                rgb
            })
            .collect::<Vec<_>>();

        Self {
            width: Self::WIDTH as usize,
            height: Self::HEIGHT as usize,
            pixels: Arc::from(pixels),
        }
    }

    fn sample(&self, u: f32, v: f32) -> [f32; 3] {
        let x = u.clamp(0.0, 1.0) * (self.width - 1) as f32;
        let y = v.clamp(0.0, 1.0) * (self.height - 1) as f32;
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);
        let tx = x - x0 as f32;
        let ty = y - y0 as f32;
        let p00 = self.pixels[y0 * self.width + x0];
        let p10 = self.pixels[y0 * self.width + x1];
        let p01 = self.pixels[y1 * self.width + x0];
        let p11 = self.pixels[y1 * self.width + x1];
        let mut out = [0.0; 3];
        for channel in 0..3 {
            let top = p00[channel] + (p10[channel] - p00[channel]) * tx;
            let bottom = p01[channel] + (p11[channel] - p01[channel]) * tx;
            out[channel] = top + (bottom - top) * ty;
        }
        out
    }
}

fn sample_rgb(image: &image::RgbImage, u: f32, v: f32) -> Option<[f32; 3]> {
    if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
        return None;
    }
    let x = u * (image.width() - 1) as f32;
    let y = v * (image.height() - 1) as f32;
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(image.width() - 1);
    let y1 = (y0 + 1).min(image.height() - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let p00 = image.get_pixel(x0, y0);
    let p10 = image.get_pixel(x1, y0);
    let p01 = image.get_pixel(x0, y1);
    let p11 = image.get_pixel(x1, y1);
    let mut out = [0.0; 3];
    for channel in 0..3 {
        let top = p00[channel] as f32 + (p10[channel] as f32 - p00[channel] as f32) * tx;
        let bottom = p01[channel] as f32 + (p11[channel] as f32 - p01[channel] as f32) * tx;
        out[channel] = top + (bottom - top) * ty;
    }
    Some(out)
}

fn darken_rgb_for_text(rgb: &mut [f32; 3], max_luminance: f32) {
    let luminance = 0.2126 * linear_channel(rgb[0])
        + 0.7152 * linear_channel(rgb[1])
        + 0.0722 * linear_channel(rgb[2]);
    if luminance <= max_luminance {
        return;
    }
    let scale = (max_luminance / luminance).sqrt();
    for channel in rgb {
        *channel *= scale;
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    h: f32,
    s: f32,
    l: f32,
    rgb: [f32; 3],
    score: f32,
}
fn hue_distance(a: f32, b: f32) -> f32 {
    let d = (a - b).abs();
    d.min(1.0 - d)
}
fn to_rgb(c: Color32) -> [f32; 3] {
    [
        c.r() as f32 / 255.0,
        c.g() as f32 / 255.0,
        c.b() as f32 / 255.0,
    ]
}
fn darken_for_contrast(color: Color32, max_luminance: f32) -> Color32 {
    let rgb = to_rgb(color);
    let luminance = 0.2126 * linear_channel(rgb[0])
        + 0.7152 * linear_channel(rgb[1])
        + 0.0722 * linear_channel(rgb[2]);
    if luminance <= max_luminance {
        return color;
    }
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..16 {
        let scale = (low + high) * 0.5;
        let lum = 0.2126 * linear_channel(rgb[0] * scale)
            + 0.7152 * linear_channel(rgb[1] * scale)
            + 0.0722 * linear_channel(rgb[2] * scale);
        if lum > max_luminance {
            high = scale;
        } else {
            low = scale;
        }
    }
    Color32::from_rgb(
        (rgb[0] * low * 255.0).round() as u8,
        (rgb[1] * low * 255.0).round() as u8,
        (rgb[2] * low * 255.0).round() as u8,
    )
}
fn linear_channel(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn rgb_to_hsl(c: [f32; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0], c[1], c[2]);
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let mut h = if mx == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    h = (h / 6.0).rem_euclid(1.0);
    (h, s, l)
}
fn hsl_color(h: f32, s: f32, l: f32) -> Color32 {
    let a = s * l.min(1.0 - l);
    let f = |n: f32| {
        let k = (n + h * 12.0).rem_euclid(12.0);
        l - a * (-1.0f32).max((k - 3.0).min(9.0 - k).min(1.0))
    };
    Color32::from_rgb(
        (f(0.0) * 255.0).round() as u8,
        (f(8.0) * 255.0).round() as u8,
        (f(4.0) * 255.0).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, Rgb};

    fn solid(width: u32, height: u32, color: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(ImageBuffer::from_pixel(width, height, Rgb(color)))
    }

    fn relative_luminance(color: Color32) -> f32 {
        fn linear(v: u8) -> f32 {
            let c = v as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
    }

    #[test]
    fn palette_is_deterministic_for_tiny_and_solid_artwork() {
        for image in [
            solid(1, 1, [240, 28, 70]),
            solid(7, 3, [24, 154, 212]),
            solid(5, 9, [135, 135, 135]),
        ] {
            let first = Theme::from_image(&image);
            let second = Theme::from_image(&image);
            assert_eq!(first.palette, second.palette);
        }
    }

    #[test]
    fn grayscale_artwork_keeps_a_subtle_neutral_palette() {
        let gray = Theme::from_image(&solid(8, 5, [135, 135, 135]));
        for color in gray.palette {
            let spread =
                color.r().max(color.g()).max(color.b()) - color.r().min(color.g()).min(color.b());
            assert!(
                spread <= 8,
                "grayscale artwork introduced strong color: {color:?}"
            );
            assert!(
                relative_luminance(color) < 0.20,
                "palette is too bright for white foregrounds: {color:?}"
            );
        }
    }

    #[test]
    fn extracted_colors_stay_dark_enough_for_white_text() {
        let artwork = solid(3, 2, [245, 176, 36]);
        let theme = Theme::from_image(&artwork);
        for color in theme.palette {
            assert!(
                relative_luminance(color) < 0.20,
                "palette is too bright for white foregrounds: {color:?}"
            );
        }
    }

    #[test]
    fn mesh_has_finite_vertices_valid_indices_and_rounded_bottom() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(300.0, 80.0));
        let theme = Theme::from_image(&solid(5, 5, [240, 28, 70]));
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            theme.paint_with_radius(&painter, rect, 2.4, 0.75, 6.0);
        });
        let mesh = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                Shape::Mesh(mesh) => Some(mesh),
                _ => None,
            })
            .expect("theme should paint a mesh");
        assert!(!mesh.vertices.is_empty());
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.pos.x.is_finite() && v.pos.y.is_finite())
        );
        assert!(
            mesh.indices
                .iter()
                .all(|&index| (index as usize) < mesh.vertices.len())
        );

        let top = mesh
            .vertices
            .iter()
            .filter(|v| (v.pos.y - rect.top()).abs() < 0.001);
        let (top_left, top_right) = top.fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v.pos.x), hi.max(v.pos.x))
        });
        assert!(
            (top_left - rect.left()).abs() < 0.001,
            "top edge should be flat to the left corner"
        );
        assert!(
            (top_right - rect.right()).abs() < 0.001,
            "top edge should be flat to the right corner"
        );

        let bottom = mesh
            .vertices
            .iter()
            .filter(|v| (v.pos.y - rect.bottom()).abs() < 0.001);
        let (bottom_left, bottom_right) = bottom
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.pos.x), hi.max(v.pos.x))
            });
        assert!(
            bottom_left > rect.left() + 5.9,
            "left bottom corner should be rounded"
        );
        assert!(
            bottom_right < rect.right() - 5.9,
            "right bottom corner should be rounded"
        );
    }

    #[test]
    fn background_without_artwork_is_solid_black() {
        let ctx = egui::Context::default();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(152.0, 12.0));
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background());
            Theme::default().paint_with_radius(&painter, rect, 2.4, 1.0, 6.0);
        });
        assert!(output.shapes.iter().any(|clipped| matches!(
            &clipped.shape,
            Shape::Rect(shape) if shape.rect == rect && shape.fill == Color32::BLACK
        )));
    }
}

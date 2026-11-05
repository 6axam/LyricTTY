use crate::theme::Palette;
use anyhow::{Context, Result};
use fontdue::{Font, FontSettings};
use image::{Rgba, RgbaImage};
use ratatui::style::Color;
use std::{fs, process::Command};

pub struct PanelRenderer {
    font: Font,
}
pub struct PanelData<'a> {
    pub title: &'a str,
    pub artist: &'a str,
    pub position: f32,
    pub duration: f32,
    pub playing: bool,
    pub volume: f32,
    pub palette: Palette,
}
impl PanelRenderer {
    pub fn new() -> Result<Self> {
        let output = Command::new("fc-match")
            .args(["-f", "%{file}", "sans-serif:weight=bold"])
            .output()?;
        anyhow::ensure!(output.status.success(), "Font discovery failed");
        let bytes =
            fs::read(String::from_utf8(output.stdout)?.trim()).context("Load panel font")?;
        Ok(Self {
            font: Font::from_bytes(bytes, FontSettings::default()).map_err(anyhow::Error::msg)?,
        })
    }
    pub fn render(&self, width: u32, height: u32, data: &PanelData<'_>) -> RgbaImage {
        let mut image = RgbaImage::new(width, height);
        let w = width as f32;
        let h = height as f32;
        let radius = (h * 0.12).min(30.0);
        let panel = rgb(data.palette.panel);
        let fg = rgb(data.palette.fg);
        let muted = rgb(data.palette.muted);
        // Signed distance gives a subpixel edge and a quiet one-pixel outline.
        for (x, y, pixel) in image.enumerate_pixels_mut() {
            let dx = ((x as f32 + 0.5 - w / 2.0).abs() - (w / 2.0 - radius)).max(0.0);
            let dy = ((y as f32 + 0.5 - h / 2.0).abs() - (h / 2.0 - radius)).max(0.0);
            let distance = (dx * dx + dy * dy).sqrt() - radius;
            let alpha = (0.5 - distance).clamp(0.0, 1.0);
            let border = (distance + 1.5).clamp(0.0, 1.0) * 0.18;
            let tint = 0.025 * (1.0 - y as f32 / h) + border;
            *pixel = Rgba([
                blend(panel[0], fg[0], tint),
                blend(panel[1], fg[1], tint),
                blend(panel[2], fg[2], tint),
                (alpha * 255.0) as u8,
            ]);
        }
        let pad = w * 0.043;
        self.text(
            &mut image,
            data.title,
            pad,
            h * 0.25,
            (h * 0.14).clamp(20.0, 38.0),
            fg,
            w - 2.0 * pad,
        );
        self.text(
            &mut image,
            data.artist,
            pad,
            h * 0.38,
            (h * 0.095).clamp(15.0, 25.0),
            muted,
            w - 2.0 * pad,
        );
        let time_px = (h * 0.073).clamp(12.0, 19.0);
        let time_y = h * 0.57;
        self.text(
            &mut image,
            &time(data.position),
            pad,
            time_y,
            time_px,
            fg,
            w * 0.11,
        );
        self.text(
            &mut image,
            &format!("−{}", time((data.duration - data.position).max(0.0))),
            w * 0.88,
            time_y,
            time_px,
            muted,
            w * 0.11,
        );
        let bar_y = h * 0.54;
        let bar_x = w * 0.12;
        let bar_end = w * 0.86;
        let ratio = if data.duration > 0.0 {
            (data.position / data.duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let dot = bar_x + (bar_end - bar_x) * ratio;
        segment(&mut image, bar_x, bar_y, bar_end, bar_y, 2.0, muted, 0.3);
        segment(&mut image, bar_x, bar_y, dot, bar_y, 2.0, fg, 1.0);
        disc(&mut image, dot, bar_y, (h * 0.027).max(4.0), fg, 1.0);
        let cy = h * 0.80;
        let size = (h * 0.055).max(7.0);
        // Previous, play/pause and next share the same visual weight.
        for (center, direction) in [(w * 0.42, -1.0), (w * 0.58, 1.0)] {
            triangle(
                &mut image,
                center - direction * size * 0.45,
                cy,
                size * 0.8,
                direction,
                fg,
            );
            triangle(
                &mut image,
                center + direction * size * 0.55,
                cy,
                size * 0.8,
                direction,
                fg,
            );
        }
        if data.playing {
            for dx in [-size * 0.45, size * 0.45] {
                segment(
                    &mut image,
                    w * 0.50 + dx,
                    cy - size,
                    w * 0.50 + dx,
                    cy + size,
                    size * 0.36,
                    fg,
                    1.0,
                );
            }
        } else {
            triangle(&mut image, w * 0.50, cy, size * 1.15, 1.0, fg);
        }
        // Secondary controls sit outside the main transport group.
        let sx = w * 0.34;
        for sign in [-1.0, 1.0] {
            segment(
                &mut image,
                sx - size * 0.6,
                cy - sign * size * 0.4,
                sx + size * 0.6,
                cy + sign * size * 0.4,
                1.0,
                muted,
                0.9,
            );
        }
        let rx = w * 0.66;
        segment(
            &mut image,
            rx - size * 0.6,
            cy - size * 0.4,
            rx + size * 0.6,
            cy - size * 0.4,
            1.0,
            muted,
            0.9,
        );
        segment(
            &mut image,
            rx + size * 0.6,
            cy - size * 0.4,
            rx + size * 0.6,
            cy + size * 0.4,
            1.0,
            muted,
            0.9,
        );
        segment(
            &mut image,
            rx + size * 0.6,
            cy + size * 0.4,
            rx - size * 0.6,
            cy + size * 0.4,
            1.0,
            muted,
            0.9,
        );
        triangle(
            &mut image,
            rx - size * 0.6,
            cy + size * 0.4,
            size * 0.27,
            -1.0,
            muted,
        );
        let vx = w * 0.075;
        triangle(&mut image, vx, cy, size * 0.45, -1.0, fg);
        segment(&mut image, w * 0.10, cy, w * 0.22, cy, 1.5, muted, 0.4);
        segment(
            &mut image,
            w * 0.10,
            cy,
            w * (0.10 + 0.12 * data.volume.clamp(0.0, 1.0)),
            cy,
            1.5,
            fg,
            1.0,
        );
        image
    }
    fn text(
        &self,
        image: &mut RgbaImage,
        text: &str,
        mut x: f32,
        baseline: f32,
        px: f32,
        color: [u8; 3],
        max_width: f32,
    ) {
        let end = x + max_width;
        for c in text.chars() {
            let (m, mask) = self.font.rasterize(c, px);
            if x + m.advance_width > end {
                break;
            }
            for gy in 0..m.height {
                for gx in 0..m.width {
                    let dx = x as i32 + m.xmin + gx as i32;
                    let dy = baseline as i32 - m.ymin - m.height as i32 + gy as i32;
                    put(image, dx, dy, color, mask[gy * m.width + gx] as f32 / 255.0);
                }
            }
            x += m.advance_width;
        }
    }
}
fn rgb(color: Color) -> [u8; 3] {
    if let Color::Rgb(r, g, b) = color {
        [r, g, b]
    } else {
        [230; 3]
    }
}
fn blend(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t) as u8
}
fn put(image: &mut RgbaImage, x: i32, y: i32, color: [u8; 3], alpha: f32) {
    if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
        return;
    }
    let p = image.get_pixel_mut(x as u32, y as u32);
    for i in 0..3 {
        p.0[i] = blend(p.0[i], color[i], alpha);
    }
}
fn segment(
    image: &mut RgbaImage,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    radius: f32,
    color: [u8; 3],
    alpha: f32,
) {
    let length = (x2 - x1).powi(2) + (y2 - y1).powi(2);
    for y in (y1.min(y2) - radius - 1.0) as i32..=(y1.max(y2) + radius + 1.0) as i32 {
        for x in (x1.min(x2) - radius - 1.0) as i32..=(x1.max(x2) + radius + 1.0) as i32 {
            let t = if length > 0.0 {
                (((x as f32 - x1) * (x2 - x1) + (y as f32 - y1) * (y2 - y1)) / length)
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = ((x as f32 - x1 - t * (x2 - x1)).powi(2)
                + (y as f32 - y1 - t * (y2 - y1)).powi(2))
            .sqrt();
            put(
                image,
                x,
                y,
                color,
                (radius + 0.5 - d).clamp(0.0, 1.0) * alpha,
            );
        }
    }
}
fn disc(image: &mut RgbaImage, x: f32, y: f32, radius: f32, color: [u8; 3], alpha: f32) {
    segment(image, x, y, x, y, radius, color, alpha);
}
fn triangle(image: &mut RgbaImage, cx: f32, cy: f32, size: f32, direction: f32, color: [u8; 3]) {
    for y in (cy - size) as i32..=(cy + size) as i32 {
        for x in (cx - size) as i32..=(cx + size) as i32 {
            let dx = (x as f32 - cx) * direction;
            let edge = ((size - dx) * 0.66 - (y as f32 - cy).abs()).min(dx + size * 0.55);
            put(image, x, y, color, (edge + 0.5).clamp(0.0, 1.0));
        }
    }
}
fn time(t: f32) -> String {
    let s = t.max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

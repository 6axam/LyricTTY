//! Kitty-only pixel placements. Glyphs are rasterized once and line images are
//! retransmitted only when their colors/fill change; movement is placement-only.
use crate::{lyric_view::Hit, lyrics::LyricLine, theme::Palette};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use fontdue::{Font, FontSettings, Metrics};
use image::{
    codecs::png::{CompressionType, FilterType, PngEncoder},
    ImageEncoder,
};
use ratatui::{layout::Rect, style::Color};
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    process::Command,
    sync::Arc,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone)]
struct Glyph {
    metrics: Metrics,
    mask: Vec<u8>,
}
struct PlacedGlyph {
    glyph: Arc<Glyph>,
    x: i32,
    y: i32,
    row_y: f32,
    word: Option<usize>,
}
#[derive(Clone, Copy)]
struct WordBox {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    index: usize,
}
struct RasterLine {
    glyphs: Vec<PlacedGlyph>,
    words: Vec<WordBox>,
    height: u32,
    offset: f32,
    style: Vec<u8>,
    uploaded: bool,
    visible: bool,
}
pub struct PixelLyrics {
    font: Font,
    cell: (u16, u16),
    font_px: f32,
    glyphs: HashMap<char, Arc<Glyph>>,
    content: Vec<LyricLine>,
    lines: Vec<RasterLine>,
    width: u32,
    karaoke: bool,
    id_base: u32,
    pending: Vec<u8>,
}
impl PixelLyrics {
    pub fn system_font(cell: (u16, u16)) -> Result<Self> {
        let path = if let Some(path) = std::env::var_os("LYRICTTY_LYRICS_FONT") {
            std::path::PathBuf::from(path)
        } else {
            let output = Command::new("fc-match")
                .args(["-f", "%{file}", "sans-serif:weight=bold"])
                .output()
                .context("Find lyric font")?;
            anyhow::ensure!(output.status.success(), "Font discovery failed");
            std::path::PathBuf::from(String::from_utf8(output.stdout)?.trim())
        };
        let bytes = fs::read(path)?;
        anyhow::ensure!(bytes.len() <= 32 * 1024 * 1024, "Font too large");
        let font = Font::from_bytes(bytes, FontSettings::default()).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(cell.0 > 0 && cell.1 > 0, "Invalid terminal cell size");
        Ok(Self {
            font,
            cell,
            font_px: 0.0,
            glyphs: HashMap::new(),
            content: Vec::new(),
            lines: Vec::new(),
            width: 0,
            karaoke: false,
            id_base: 0x6000_0000 | (std::process::id() & 0x000f_ffff) << 10,
            pending: Vec::new(),
        })
    }
    /// Fontdue is intentionally used only for simple LTR text. Preserve terminal
    /// shaping/fallback for emoji, combining graphemes and complex scripts.
    pub fn supports(&self, lines: &[LyricLine]) -> bool {
        lines.len() <= 4096 && lines.iter().all(|line| {
            line.text.graphemes(true).all(|grapheme| {
                if grapheme=="\n" || grapheme=="\r" { return true; }
                let mut chars=grapheme.chars();
                let Some(c)=chars.next() else { return true; };
                let complex=matches!(c as u32,0x0590..=0x109f|0x1780..=0x18af|0x200e..=0x200f|0x202a..=0x202e|0x2066..=0x2069);
                chars.next().is_none() && !complex && self.font.has_glyph(c)
            })
        })
    }
    pub fn cell_size(&self) -> (u16, u16) {
        self.cell
    }
    pub fn set_cell_size(&mut self, cell: (u16, u16)) {
        if cell.0 > 0 && cell.1 > 0 && cell != self.cell {
            self.cell = cell;
            self.width = 0;
        }
    }
    pub fn begin_frame(&mut self) {
        self.pending.clear();
    }
    fn id(&self, index: usize) -> u32 {
        self.id_base.wrapping_add(index as u32 + 1)
    }
    fn clear_images(&mut self) {
        for (i, line) in self.lines.iter().enumerate() {
            if line.uploaded {
                let _ = write!(self.pending, "\x1b_Ga=d,d=I,i={},q=2\x1b\\", self.id(i));
            }
        }
    }
    pub fn hide(&mut self) {
        for i in 0..self.lines.len() {
            if self.lines[i].visible {
                let _ = write!(self.pending, "\x1b_Ga=d,d=i,i={},q=2\x1b\\", self.id(i));
                self.lines[i].visible = false;
            }
        }
    }
    fn glyph(&mut self, c: char) -> Arc<Glyph> {
        self.glyphs
            .entry(c)
            .or_insert_with(|| {
                let (metrics, mask) = self.font.rasterize(c, self.font_px);
                Arc::new(Glyph { metrics, mask })
            })
            .clone()
    }
    fn layout(&mut self, content: &[LyricLine], width: u32, karaoke: bool) {
        if self.width == width && self.karaoke == karaoke && self.content == content {
            return;
        }
        self.clear_images();
        self.lines.clear();
        self.content = content.to_vec();
        self.width = width;
        self.karaoke = karaoke;
        let px = (self.cell.1 as f32 * if karaoke { 2.15 } else { 1.95 }).clamp(26.0, 60.0);
        if px != self.font_px {
            self.font_px = px;
            self.glyphs.clear();
        }
        let row_height = (px * 1.45).ceil();
        let mut offset = 0.0;
        for line in content {
            let mut glyphs = Vec::new();
            let mut words = Vec::new();
            let mut row_widths = Vec::new();
            let mut x = 0.0f32;
            let mut y = 0.0f32;
            let parts: Vec<_> = if line.words.is_empty() {
                vec![(line.text.as_str(), None)]
            } else {
                line.words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| (w.text.as_str(), Some(i)))
                    .collect()
            };
            for (text, word) in parts {
                let word_width: f32 = text
                    .chars()
                    .map(|c| self.glyph(c).metrics.advance_width)
                    .sum();
                if x > 0.0
                    && word_width > 0.0
                    && word_width <= width as f32
                    && x + word_width > width as f32
                {
                    row_widths.push(x);
                    x = 0.0;
                    y += row_height;
                }
                let mut part_x = x;
                for c in text.chars() {
                    if c == '\r' {
                        continue;
                    }
                    let glyph = self.glyph(c);
                    let advance = glyph.metrics.advance_width;
                    if c == '\n' || (x + advance > width as f32 && x > 0.0) {
                        if let Some(index) = word {
                            if x > part_x {
                                words.push(WordBox {
                                    x: part_x,
                                    y,
                                    width: x - part_x,
                                    height: row_height,
                                    index,
                                });
                            }
                        }
                        row_widths.push(x);
                        x = 0.0;
                        part_x = 0.0;
                        y += row_height;
                        if c == '\n' {
                            continue;
                        }
                    }
                    let top = y + px - glyph.metrics.height as f32 - glyph.metrics.ymin as f32;
                    glyphs.push(PlacedGlyph {
                        x: x.round() as i32 + glyph.metrics.xmin,
                        y: top.round() as i32,
                        row_y: y,
                        glyph,
                        word,
                    });
                    x += advance;
                }
                if let Some(index) = word {
                    if x > part_x {
                        words.push(WordBox {
                            x: part_x,
                            y,
                            width: x - part_x,
                            height: row_height,
                            index,
                        });
                    }
                }
            }
            row_widths.push(x);
            for glyph in &mut glyphs {
                let row = (glyph.row_y / row_height) as usize;
                if karaoke {
                    glyph.x += ((width as f32 - row_widths[row.min(row_widths.len() - 1)]) / 2.0)
                        .floor() as i32;
                }
            }
            for region in &mut words {
                let row = (region.y / row_height) as usize;
                if karaoke {
                    region.x +=
                        ((width as f32 - row_widths[row.min(row_widths.len() - 1)]) / 2.0).floor();
                }
            }
            let height = (y + row_height).ceil() as u32;
            self.lines.push(RasterLine {
                glyphs,
                words,
                height,
                offset,
                style: Vec::new(),
                uploaded: false,
                visible: false,
            });
            offset += height as f32 + row_height * 0.75;
        }
    }
    pub fn prepare(
        &mut self,
        area: Rect,
        content: &[LyricLine],
        scroll: f32,
        position: f32,
        palette: Palette,
        karaoke: bool,
    ) -> Result<Vec<Hit>> {
        if area.width < 4 || area.height < 2 || content.is_empty() {
            self.hide();
            return Ok(Vec::new());
        }
        let width = u32::from(area.width - 2) * u32::from(self.cell.0);
        self.layout(content, width, karaoke);
        anyhow::ensure!(
            self.lines
                .iter()
                .all(|line| u64::from(width) * u64::from(line.height) <= 4_000_000),
            "Lyric raster exceeds size limit"
        );
        let scroll = scroll.clamp(0.0, self.lines.len().saturating_sub(1) as f32);
        let anchor = scroll.floor() as usize;
        let current = self.lines[anchor].offset;
        let next = self
            .lines
            .get(anchor + 1)
            .map(|line| line.offset)
            .unwrap_or(current);
        let offset = current + (next - current) * scroll.fract();
        let pane_y = u32::from(area.y) * u32::from(self.cell.1);
        let pane_height = u32::from(area.height) * u32::from(self.cell.1);
        let center = pane_height as f32 * 0.30 - self.font_px * 0.6;
        let mut hits = Vec::new();
        for (i, lyric) in content.iter().enumerate() {
            let id = self.id(i);
            let line = &mut self.lines[i];
            let top = (center + line.offset - offset).round() as i32;
            let clip = clip_line(top, line.height, pane_height);
            let Some((crop_y, height, destination)) = clip else {
                if line.visible {
                    write!(self.pending, "\x1b_Ga=d,d=i,i={id},q=2\x1b\\")?;
                    line.visible = false;
                }
                continue;
            };
            let edge = ((top + line.height as i32) as f32 / (pane_height as f32 * 0.22))
                .min((pane_height as f32 - top as f32) / (pane_height as f32 * 0.22))
                .clamp(0.0, 1.0);
            let emphasis = (1.0 - (i as f32 - scroll).abs() * 0.3).clamp(0.16, 1.0)
                * edge
                * edge
                * (3.0 - 2.0 * edge);
            let opacity = (emphasis * 32.0).round() as u8;
            let style = line_style(lyric, position, palette, opacity, &line.words);
            if !line.uploaded || line.style != style {
                let png = paint_line(line, lyric, width, position, palette, opacity)?;
                transmit(&mut self.pending, id, &png)?;
                line.style = style;
                line.uploaded = true;
            }
            let y = pane_y + destination;
            let x = u32::from(area.x + 1) * u32::from(self.cell.0);
            write!(self.pending,"\x1b[{};{}H\x1b_Ga=p,i={id},p=1,x=0,y={crop_y},w={width},h={height},X={},Y={},C=1,z=2,q=2\x1b\\",y/u32::from(self.cell.1)+1,x/u32::from(self.cell.0)+1,x%u32::from(self.cell.0),y%u32::from(self.cell.1))?;
            line.visible = true;
            for word in &line.words {
                let region = pixel_hit(
                    area,
                    self.cell,
                    word.x,
                    top as f32 + word.y,
                    word.width,
                    word.height,
                );
                if let Some(region) = region {
                    hits.push(Hit {
                        area: region,
                        line: i,
                        timestamp: lyric.words[word.index].start,
                    });
                }
            }
            if let Some(region) = pixel_hit(
                area,
                self.cell,
                0.0,
                top as f32,
                width as f32,
                line.height as f32,
            ) {
                hits.push(Hit {
                    area: region,
                    line: i,
                    timestamp: lyric.start,
                });
            }
        }
        Ok(hits)
    }
    pub fn flush(&mut self, output: &mut impl Write) -> io::Result<()> {
        if !self.pending.is_empty() {
            output.write_all(b"\x1b7")?;
            output.write_all(&self.pending)?;
            output.write_all(b"\x1b8")?;
            output.flush()?;
            self.pending.clear();
        }
        Ok(())
    }
    pub fn cleanup(&mut self, output: &mut impl Write) -> io::Result<()> {
        self.pending.clear();
        self.clear_images();
        self.lines.clear();
        self.content.clear();
        self.width = 0;
        self.flush(output)
    }
}
fn rgb(c: Color) -> [u8; 3] {
    if let Color::Rgb(r, g, b) = c {
        [r, g, b]
    } else {
        [230, 235, 230]
    }
}
fn line_style(
    line: &LyricLine,
    position: f32,
    palette: Palette,
    opacity: u8,
    words: &[WordBox],
) -> Vec<u8> {
    let mut style = vec![opacity];
    for c in [palette.fg, palette.muted, palette.accent] {
        style.extend(rgb(c));
    }
    for (i, w) in line.words.iter().enumerate() {
        let width = words
            .iter()
            .filter(|r| r.index == i)
            .map(|r| r.width)
            .sum::<f32>();
        let progress = progress(w.start, w.end, position);
        style.extend_from_slice(&((progress * width).floor() as u32).to_le_bytes());
        style.push(u8::from(position >= w.end));
    }
    style
}
fn progress(start: f32, end: f32, position: f32) -> f32 {
    if end <= start {
        0.0
    } else {
        ((position - start) / (end - start)).clamp(0.0, 1.0)
    }
}
fn paint_line(
    line: &RasterLine,
    lyric: &LyricLine,
    width: u32,
    position: f32,
    palette: Palette,
    opacity: u8,
) -> Result<Vec<u8>> {
    let mut pixels = vec![0u8; width as usize * line.height as usize * 4];
    for glyph in &line.glyphs {
        let g = &glyph.glyph;
        let mut color = rgb(palette.fg);
        let mut boundary = None;
        if let Some(wi) = glyph.word {
            let w = &lyric.words[wi];
            if position < w.end {
                color = rgb(palette.muted);
                if position >= w.start {
                    let regions: Vec<_> = line.words.iter().filter(|r| r.index == wi).collect();
                    let mut fill = progress(w.start, w.end, position)
                        * regions.iter().map(|r| r.width).sum::<f32>();
                    for region in regions {
                        if glyph.row_y >= region.y && glyph.row_y < region.y + region.height {
                            boundary = Some(region.x + fill.min(region.width));
                            break;
                        }
                        fill = (fill - region.width).max(0.0);
                    }
                }
            }
        }
        for gy in 0..g.metrics.height {
            let y = glyph.y + gy as i32;
            if y < 0 || y >= line.height as i32 {
                continue;
            }
            for gx in 0..g.metrics.width {
                let x = glyph.x + gx as i32;
                if x < 0 || x >= width as i32 {
                    continue;
                }
                let alpha = g.mask[gy * g.metrics.width + gx];
                if alpha == 0 {
                    continue;
                }
                let color = if let Some(edge) = boundary.filter(|edge| (x as f32) < *edge) {
                    rgb(crate::theme::mix(
                        palette.accent,
                        palette.fg,
                        ((edge - x as f32) / 4.0).clamp(0.0, 1.0),
                    ))
                } else {
                    color
                };
                let index = (y as usize * width as usize + x as usize) * 4;
                pixels[index..index + 3].copy_from_slice(&color);
                pixels[index + 3] = ((u16::from(alpha) * u16::from(opacity)) / 32) as u8;
            }
        }
    }
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Sub).write_image(
        &pixels,
        width,
        line.height,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}
fn transmit(output: &mut Vec<u8>, id: u32, png: &[u8]) -> io::Result<()> {
    let encoded = STANDARD.encode(png);
    let mut chunks = encoded.as_bytes().chunks(4096).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(output, "\x1b_Ga=t,f=100,i={id},q=2,m={more};")?;
            first = false;
        } else {
            write!(output, "\x1b_Gq=2,m={more};")?;
        }
        output.extend_from_slice(chunk);
        output.extend_from_slice(b"\x1b\\");
    }
    Ok(())
}
fn clip_line(top: i32, height: u32, pane_height: u32) -> Option<(u32, u32, u32)> {
    let crop = top.saturating_neg().max(0) as u32;
    let dest = top.max(0) as u32;
    let height = height
        .saturating_sub(crop)
        .min(pane_height.saturating_sub(dest));
    (height > 0).then_some((crop, height, dest))
}
fn pixel_hit(
    area: Rect,
    cell: (u16, u16),
    x: f32,
    y: f32,
    width: f32,
    height: f32,
) -> Option<Rect> {
    let left = (x / cell.0 as f32).floor().max(0.0) as u16 + area.x + 1;
    let right = ((x + width) / cell.0 as f32).ceil().max(0.0) as u16 + area.x + 1;
    let top = (y / cell.1 as f32).floor().max(0.0) as u16 + area.y;
    let bottom = ((y + height) / cell.1 as f32).ceil().max(0.0) as u16 + area.y;
    let left = left.max(area.x + 1).min(area.right());
    let right = right.min(area.right().saturating_sub(1));
    let bottom = bottom.min(area.bottom());
    (right > left && bottom > top).then_some(Rect::new(
        left,
        top,
        right.saturating_sub(left),
        bottom.saturating_sub(top),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crop_preserves_pixel_offsets_and_viewport_bounds() {
        assert_eq!(clip_line(-7, 40, 300), Some((7, 33, 0)));
        assert_eq!(clip_line(283, 40, 300), Some((0, 17, 283)));
        assert_eq!(clip_line(-50, 40, 300), None);
        assert_eq!(clip_line(301, 40, 300), None);
    }
    #[test]
    fn pixel_hits_follow_fractional_cell_movement_and_are_clipped() {
        let area = Rect::new(40, 5, 50, 20);
        let a = pixel_hit(area, (10, 20), 20.0, 37.0, 60.0, 30.0).unwrap();
        assert_eq!(a, Rect::new(43, 6, 6, 3));
        assert!(pixel_hit(area, (10, 20), 20.0, -50.0, 30.0, 10.0).is_none());
    }
    #[test]
    fn chunks_are_bounded_quiet_and_reuse_one_image_id() {
        let mut out = Vec::new();
        transmit(&mut out, 77, &vec![0; 8000]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("\x1b_Ga=t,f=100,i=77,q=2,m=1;"));
        assert!(text.contains("\x1b_Gq=2,m=0;"));
        for part in text.split("\x1b_G").skip(1) {
            let payload = part.split_once(';').unwrap().1.trim_end_matches("\x1b\\");
            assert!(payload.len() <= 4096);
        }
    }
    #[test]
    fn cached_lines_move_in_pixels_without_retransmitting_and_overlays_release_placements() {
        let mut r = PixelLyrics::system_font((10, 20))
            .expect("Pixel renderer tests need fontconfig and a sans font");
        let lyrics = crate::lyrics::parse_lrc("[00:00]Привет мир\n[00:03]Another lyric line");
        let area = Rect::new(30, 4, 70, 8);
        assert!(r.supports(&lyrics));
        r.prepare(area, &lyrics, 0.0, 0.0, Palette::default(), false)
            .unwrap();
        let mut first = Vec::new();
        r.flush(&mut first).unwrap();
        assert!(String::from_utf8_lossy(&first).contains("a=t,"));
        r.begin_frame();
        r.prepare(area, &lyrics, 0.01, 0.0, Palette::default(), false)
            .unwrap();
        let mut moved = Vec::new();
        r.flush(&mut moved).unwrap();
        let moved = String::from_utf8(moved).unwrap();
        assert!(!moved.contains("a=t,"));
        assert!(moved.contains("a=p,"));
        let placements = |text: &str| {
            text.split("\x1b_Ga=p,")
                .skip(1)
                .map(|p| p.split("\x1b\\").next().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_ne!(
            placements(&String::from_utf8_lossy(&first)),
            placements(&moved),
            "A sub-cell scroll must change pixel placements"
        );
        r.begin_frame();
        r.hide();
        let mut hidden = Vec::new();
        r.flush(&mut hidden).unwrap();
        assert!(String::from_utf8_lossy(&hidden).contains("a=d,d=i"));
        r.begin_frame();
        r.prepare(area, &lyrics, 0.01, 0.0, Palette::default(), false)
            .unwrap();
        let mut shown = Vec::new();
        r.flush(&mut shown).unwrap();
        assert!(!String::from_utf8_lossy(&shown).contains("a=t,"));
        let mut cleanup = Vec::new();
        r.cleanup(&mut cleanup).unwrap();
        assert!(String::from_utf8_lossy(&cleanup).contains("a=d,d=I"));
    }
    #[test]
    fn pixel_word_hits_preserve_real_timestamps_across_font_and_viewport_resize() {
        let mut r = PixelLyrics::system_font((10, 20)).expect("Install fontconfig and a sans font");
        let lyrics =
            crate::lyrics::parse_lrc("[00:01]<00:01>Привет <00:02>мир<00:03>\n[00:05]Next line");
        for (cell, area) in [
            ((10, 20), Rect::new(40, 3, 60, 20)),
            ((14, 28), Rect::new(0, 2, 36, 15)),
        ] {
            r.set_cell_size(cell);
            r.begin_frame();
            let hits = r
                .prepare(area, &lyrics, 0.2, 1.5, Palette::default(), false)
                .unwrap();
            assert!(hits.iter().any(|h| h.timestamp == 2.0));
            for h in hits {
                assert!(
                    h.area.x >= area.x
                        && h.area.right() <= area.right()
                        && h.area.y >= area.y
                        && h.area.bottom() <= area.bottom()
                );
            }
        }
        let complex = crate::lyrics::parse_lrc("[00:00]👩‍💻 e\u{301}");
        assert!(!r.supports(&complex));
    }
}

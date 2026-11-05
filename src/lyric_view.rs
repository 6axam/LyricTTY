//! Fractional-cell scrolling: distribute each text row over its two neighbouring
//! terminal rows with complementary intensity. Glyphs remain terminal-native.
use crate::{
    lyrics::{active_word, LyricLine},
    theme::Palette,
};
use ratatui::{
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Default, Debug)]
pub struct ScrollState {
    pub current: f32,
    target: f32,
    origin: f32,
    elapsed: f32,
    velocity: f32,
    initial_velocity: f32,
}
impl ScrollState {
    pub fn animating(&self) -> bool {
        (self.current - self.target).abs() > 0.001 || self.velocity.abs() > 0.001
    }
    pub fn target(&mut self, target: f32) {
        if self.target != target {
            self.origin = self.current;
            self.initial_velocity = self.velocity;
            self.target = target;
            self.elapsed = 0.0;
        }
    }
    pub fn advance(&mut self, dt: f32, duration: f32) {
        if !self.animating() {
            self.current = self.target;
            return;
        }
        self.elapsed += dt.max(0.0);
        let t = if duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / duration).clamp(0.0, 1.0)
        };
        if t >= 1.0 {
            self.current = self.target;
            self.velocity = 0.0;
            return;
        }
        // Minimum-jerk interpolation with the incoming velocity retained when a
        // fast lyric or manual scroll retargets an unfinished transition.
        let distance = self.target - self.origin;
        let eased = t * t * t * (10.0 + t * (-15.0 + 6.0 * t));
        let tangent = t + t * t * t * (-6.0 + t * (8.0 - 3.0 * t));
        self.current = self.origin + distance * eased + self.initial_velocity * duration * tangent;
        self.velocity = distance * (30.0 * t * t * (1.0 - t) * (1.0 - t)) / duration
            + self.initial_velocity * (1.0 + t * t * (-18.0 + t * (32.0 - 15.0 * t)));
    }
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub area: Rect,
    pub line: usize,
    pub timestamp: f32,
}
pub fn hit_test(hits: &[Hit], x: u16, y: u16) -> Option<&Hit> {
    hits.iter().find(|h| h.area.contains(Position::new(x, y)))
}
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => {
            let f =
                |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)).round() as u8;
            Color::Rgb(f(ar, br), f(ag, bg), f(ab, bb))
        }
        _ => b,
    }
}
pub fn render(
    f: &mut Frame,
    area: Rect,
    lines: &[LyricLine],
    scroll: f32,
    position: f32,
    palette: Palette,
) -> Vec<Hit> {
    let mut hits = Vec::new();
    if area.width < 2 || area.height == 0 {
        return hits;
    }
    let width = area.width.saturating_sub(2) as usize;
    if width == 0 || lines.is_empty() {
        return hits;
    }
    let wrapped: Vec<_> = lines
        .iter()
        .map(|l| wrap_line(l, width, position, palette))
        .collect();
    let mut offsets = vec![0.0];
    for rows in &wrapped {
        offsets.push(offsets.last().unwrap() + ((rows.len() * 2 + 1).max(3)) as f32);
    }
    let scroll = scroll.clamp(0.0, lines.len().saturating_sub(1) as f32);
    let anchor = scroll.floor() as usize;
    let offset = offsets[anchor]
        + (offsets[(anchor + 1).min(lines.len())] - offsets[anchor]) * scroll.fract();
    let center = area.y as f32 + (area.height as f32 * 0.14).floor();
    for (i, (line, rows)) in lines.iter().zip(wrapped.iter()).enumerate() {
        for (ri, glyphs) in rows.iter().enumerate() {
            let y = center + offsets[i] - offset + ri as f32 * 2.0;
            let floor = y.floor();
            let fraction = y - floor;
            for (row, weight) in [(floor, 1.0 - fraction), (floor + 1.0, fraction)] {
                if row < area.y as f32 || row >= area.bottom() as f32 || weight < 0.015 {
                    continue;
                }
                let distance = (i as f32 - scroll).abs();
                let edge = ((row - area.y as f32 + 1.0) / (area.height as f32 * 0.22))
                    .min((area.bottom() as f32 - row) / (area.height as f32 * 0.22))
                    .clamp(0.0, 1.0);
                let emphasis =
                    (1.0 - distance * 0.38).clamp(0.22, 1.0) * edge * edge * (3.0 - 2.0 * edge);
                let style = Style::default().add_modifier(Modifier::BOLD);
                let spans = glyphs
                    .iter()
                    .map(|g| {
                        Span::styled(
                            g.text.clone(),
                            style.fg(mix(palette.bg, g.color, weight * emphasis)),
                        )
                    })
                    .collect::<Vec<_>>();
                let row_width = glyphs.iter().map(|g| g.width).sum::<usize>() as u16;
                let x = area.x + 1;
                let rect = Rect::new(x, row as u16, row_width.min(area.width - 2), 1);
                f.render_widget(Paragraph::new(Line::from(spans)), rect);
                if weight >= 0.5 {
                    // Word regions precede line regions, so their exact timestamp wins.
                    let mut gx = x;
                    for glyph in glyphs {
                        if let Some(timestamp) = glyph.timestamp {
                            if glyph.width > 0 {
                                hits.push(Hit {
                                    area: Rect::new(gx, row as u16, glyph.width as u16, 1),
                                    line: i,
                                    timestamp,
                                });
                            }
                        }
                        gx += glyph.width as u16;
                    }
                    hits.push(Hit {
                        area: rect,
                        line: i,
                        timestamp: line.start,
                    });
                }
            }
        }
    }
    hits
}
struct Glyph {
    text: String,
    width: usize,
    color: Color,
    timestamp: Option<f32>,
}
fn wrap_line(line: &LyricLine, width: usize, position: f32, palette: Palette) -> Vec<Vec<Glyph>> {
    let fill_color = crate::theme::lyric_fill(palette);
    let mut rows = vec![Vec::new()];
    let mut used = 0;
    let active = active_word(line, position);
    let mut push = |text: &str, color: Color, timestamp: Option<f32>| {
        for g in text.graphemes(true) {
            let size = g.width();
            if g == "\n" || (used + size > width && used > 0) {
                rows.push(Vec::new());
                used = 0;
            }
            if g != "\n" && size <= width {
                rows.last_mut().unwrap().push(Glyph {
                    text: g.to_owned(),
                    width: size,
                    color,
                    timestamp,
                });
                used += size;
            }
        }
    };
    if line.words.is_empty() {
        push(&line.text, palette.fg, None);
    } else {
        for (wi, word) in line.words.iter().enumerate() {
            let count = word
                .text
                .graphemes(true)
                .map(|g| g.width())
                .sum::<usize>()
                .max(1);
            let progress = if word.end > word.start {
                ((position - word.start) / (word.end - word.start)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut filled_width = 0;
            for g in word.text.graphemes(true) {
                let size = g.width().max(1);
                let color = if word.end <= word.start {
                    palette.fg
                } else if position >= word.end {
                    fill_color
                } else if Some(wi) == active {
                    mix(
                        palette.fg,
                        fill_color,
                        ((progress * count as f32 - filled_width as f32) / size as f32)
                            .clamp(0.0, 1.0),
                    )
                } else {
                    palette.fg
                };
                push(g, color, (word.end > word.start).then_some(word.start));
                filled_width += size;
            }
        }
    }
    rows
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn easing_moves_fractionally_and_settles() {
        let mut s = ScrollState::default();
        s.target(1.0);
        s.advance(0.1, 0.8);
        assert!(s.current > 0.0 && s.current < 0.5);
        s.advance(0.7, 0.8);
        assert_eq!(s.current, 1.0);
    }
    #[test]
    fn retarget_does_not_jump() {
        let mut s = ScrollState::default();
        s.target(4.0);
        s.advance(0.4, 0.8);
        let old = s.current;
        s.target(8.0);
        assert_eq!(s.current, old);
    }
    #[test]
    fn retarget_preserves_velocity_and_uninterrupted_motion_is_monotonic() {
        let mut s = ScrollState::default();
        s.target(1.0);
        let mut previous = 0.0;
        for _ in 0..20 {
            s.advance(0.02, 0.8);
            assert!(s.current >= previous);
            previous = s.current;
        }
        let old_velocity = s.velocity;
        let position = s.current;
        s.target(2.0);
        assert_eq!(s.current, position);
        s.advance(0.0001, 0.8);
        assert!((s.velocity - old_velocity).abs() < 0.01);
        s.advance(1.0, 0.8);
        assert_eq!(s.current, 2.0);
        assert!(!s.animating());
    }
    #[test]
    fn hit_testing_accounts_for_pane_and_scrolled_line() {
        let h = [Hit {
            area: Rect::new(42, 8, 40, 1),
            line: 27,
            timestamp: 140.0,
        }];
        assert_eq!(hit_test(&h, 50, 8).unwrap().line, 27);
        assert_eq!(hit_test(&h, 50, 8).unwrap().timestamp, 140.0);
        assert!(hit_test(&h, 5, 8).is_none());
        assert!(hit_test(&h, 50, 9).is_none());
    }
    #[test]
    fn word_highlighting_and_unicode_wrapping_use_real_timestamps() {
        use crate::lyrics::LyricWord;
        let l = LyricLine {
            start: 0.0,
            end: 4.0,
            text: "Привет 世界".into(),
            words: vec![
                LyricWord {
                    text: "Привет ".into(),
                    start: 0.0,
                    end: 2.0,
                },
                LyricWord {
                    text: "世界".into(),
                    start: 2.0,
                    end: 4.0,
                },
            ],
        };
        let rows = wrap_line(&l, 8, 2.5, Palette::default());
        assert!(rows.len() > 1);
        assert_eq!(
            rows.iter()
                .flatten()
                .map(|g| g.text.as_str())
                .collect::<String>(),
            l.text
        );
        assert_eq!(rows[0][0].color, Palette::default().fg);
        assert!(rows
            .iter()
            .flatten()
            .any(|g| g.timestamp == Some(2.0) && g.color != Palette::default().muted));
    }
}

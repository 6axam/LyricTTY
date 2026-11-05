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

#[derive(Default, Debug)]
pub struct ScrollState {
    pub current: f32,
    target: f32,
    origin: f32,
    elapsed: f32,
}
impl ScrollState {
    pub fn target(&mut self, target: f32) {
        if self.target != target {
            self.origin = self.current;
            self.target = target;
            self.elapsed = 0.0;
        }
    }
    pub fn advance(&mut self, dt: f32, duration: f32) {
        self.elapsed += dt.max(0.0);
        let t = if duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / duration).clamp(0.0, 1.0)
        };
        let eased = t * t * (3.0 - 2.0 * t);
        self.current = self.origin + (self.target - self.origin) * eased;
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
    let center = area.y as f32 + area.height as f32 * 0.45;
    // Three terminal rows per lyric leaves room for interpolation without collisions.
    for (i, line) in lines.iter().enumerate() {
        let y = center + (i as f32 - scroll) * 3.0;
        let floor = y.floor();
        let fraction = y - floor;
        for (row, weight) in [(floor, 1.0 - fraction), (floor + 1.0, fraction)] {
            if row < area.y as f32 || row >= area.bottom() as f32 || weight < 0.015 {
                continue;
            }
            let distance = (i as f32 - scroll).abs();
            let emphasis = (1.0 - distance * 0.38).clamp(0.22, 1.0);
            let color = mix(palette.bg, palette.fg, weight * emphasis);
            let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            let words = active_word(line, position);
            let spans = if line.words.is_empty() {
                vec![Span::styled(line.text.clone(), style)]
            } else {
                line.words
                    .iter()
                    .enumerate()
                    .map(|(wi, w)| {
                        let target = if Some(wi) == words {
                            palette.accent
                        } else if position >= w.end {
                            palette.fg
                        } else {
                            palette.muted
                        };
                        Span::styled(w.text.clone(), style.fg(mix(palette.bg, target, weight)))
                    })
                    .collect()
            };
            let rect = Rect::new(area.x + 1, row as u16, area.width - 2, 1);
            f.render_widget(Paragraph::new(Line::from(spans)), rect);
            if weight >= 0.5 {
                hits.push(Hit {
                    area: rect,
                    line: i,
                    timestamp: line.start,
                });
            }
        }
    }
    hits
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
}

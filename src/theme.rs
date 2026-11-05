use ratatui::style::Color;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
}
impl Default for Palette {
    fn default() -> Self {
        from_artwork((90, 130, 80))
    }
}
pub fn luminance(color: Color) -> f32 {
    let c = |v: u8| {
        let x = v as f32 / 255.0;
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    };
    if let Color::Rgb(r, g, b) = color {
        0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b)
    } else {
        0.0
    }
}
pub fn contrast(a: Color, b: Color) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub fn from_artwork((r, g, b): (u8, u8, u8)) -> Palette {
    let dark = |v: u8, base: f32| (base + v as f32 * 0.075).round() as u8;
    let bg = Color::Rgb(dark(r, 7.0), dark(g, 9.0), dark(b, 10.0));
    let panel = Color::Rgb(dark(r, 12.0), dark(g, 14.0), dark(b, 15.0));
    let lift = |v: u8| (105.0 + v as f32 * 0.5) as u8;
    let mut accent = Color::Rgb(lift(r), lift(g), lift(b));
    if contrast(accent, bg) < 4.5 {
        accent = Color::Rgb(190, 205, 190);
    }
    Palette {
        bg,
        panel,
        accent,
        fg: Color::Rgb(234, 240, 234),
        muted: Color::Rgb(151, 166, 157),
    }
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
pub struct ThemeTransition {
    from: Palette,
    to: Palette,
    elapsed: f32,
    duration: f32,
}
impl Default for ThemeTransition {
    fn default() -> Self {
        Self {
            from: Palette::default(),
            to: Palette::default(),
            elapsed: 1.0,
            duration: 0.8,
        }
    }
}
impl ThemeTransition {
    pub fn set(&mut self, palette: Palette, duration: f32) {
        self.from = self.palette();
        self.to = palette;
        self.elapsed = 0.0;
        self.duration = duration;
    }
    pub fn advance(&mut self, dt: f32) {
        self.elapsed += dt.max(0.0);
    }
    pub fn animating(&self) -> bool {
        self.elapsed < self.duration
    }
    pub fn palette(&self) -> Palette {
        let t = if self.duration <= 0.0 {
            1.0
        } else {
            (self.elapsed / self.duration).clamp(0.0, 1.0)
        };
        let t = t * t * t * (10.0 + t * (-15.0 + 6.0 * t));
        Palette {
            bg: mix(self.from.bg, self.to.bg, t),
            panel: mix(self.from.panel, self.to.panel, t),
            fg: mix(self.from.fg, self.to.fg, t),
            muted: mix(self.from.muted, self.to.muted, t),
            accent: mix(self.from.accent, self.to.accent, t),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foregrounds_have_wcag_contrast_over_artwork_colors() {
        for r in (0..=255).step_by(51) {
            for g in (0..=255).step_by(51) {
                for b in (0..=255).step_by(51) {
                    let p = from_artwork((r, g, b));
                    assert!(contrast(p.fg, p.bg) > 7.0);
                    assert!(contrast(p.accent, p.bg) >= 4.5);
                    assert!(contrast(p.muted, p.bg) >= 4.5);
                }
            }
        }
    }
    #[test]
    fn transition_continuity_and_endpoints() {
        let mut t = ThemeTransition::default();
        let start = t.palette();
        let end = from_artwork((255, 0, 80));
        t.set(end, 1.0);
        assert_eq!(t.palette(), start);
        t.advance(0.5);
        assert_ne!(t.palette(), end);
        t.advance(0.5);
        assert_eq!(t.palette(), end);
    }
}

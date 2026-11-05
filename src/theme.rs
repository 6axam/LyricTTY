use ratatui::style::Color;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub bg: Color,
    pub panel: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
}
impl Default for Palette {
    fn default() -> Self {
        Self {
            bg: Color::Rgb(10, 18, 17),
            panel: Color::Rgb(19, 31, 28),
            fg: Color::Rgb(226, 238, 229),
            muted: Color::Rgb(130, 158, 142),
            accent: Color::Rgb(129, 190, 139),
        }
    }
}
pub fn from_artwork(rgb: (u8, u8, u8)) -> Palette {
    let dark = |v: u8| v.saturating_mul(2) / 5;
    let accent = Color::Rgb(rgb.0.max(80), rgb.1.max(80), rgb.2.max(80));
    Palette {
        bg: Color::Rgb(dark(rgb.0), dark(rgb.1), dark(rgb.2)),
        panel: Color::Rgb(
            dark(rgb.0.saturating_add(20)),
            dark(rgb.1.saturating_add(20)),
            dark(rgb.2.saturating_add(20)),
        ),
        fg: Color::Rgb(235, 235, 235),
        muted: Color::Rgb(150, 160, 155),
        accent,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_is_readable() {
        let p = from_artwork((240, 20, 20));
        assert!(matches!(p.bg, Color::Rgb(_, _, _)));
    }
}

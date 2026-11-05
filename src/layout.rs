use ratatui::layout::Rect;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Compact,
    Standard,
    Wide,
}
pub fn tier(area: Rect) -> Tier {
    if area.width < 85 || area.height < 22 {
        Tier::Compact
    } else if area.width >= 145 {
        Tier::Wide
    } else {
        Tier::Standard
    }
}
#[derive(Debug)]
pub struct Areas {
    pub header: Rect,
    pub metadata: Rect,
    pub lyrics: Rect,
    pub progress: Rect,
    pub controls: Rect,
    pub status: Rect,
}
pub fn areas(a: Rect, karaoke: bool) -> Areas {
    let header_h = if a.height >= 10 { 2 } else { 1 };
    let footer_h = if a.height >= 10 {
        4
    } else {
        2.min(a.height.saturating_sub(header_h))
    };
    let body = Rect::new(
        a.x,
        a.y + header_h,
        a.width,
        a.height.saturating_sub(header_h + footer_h),
    );
    let (metadata, lyrics) = if karaoke {
        (Rect::default(), body)
    } else {
        match tier(a) {
            Tier::Compact => {
                let h = if a.height >= 26 && a.width >= 44 {
                    8
                } else {
                    3.min(body.height / 3)
                };
                (
                    Rect::new(body.x, body.y, body.width, h),
                    Rect::new(body.x, body.y + h, body.width, body.height - h),
                )
            }
            t => {
                let w = if t == Tier::Wide {
                    a.width * 27 / 100
                } else {
                    a.width * 32 / 100
                };
                (
                    Rect::new(body.x, body.y, w, body.height),
                    Rect::new(
                        body.x + w + 1,
                        body.y,
                        body.width.saturating_sub(w + 1),
                        body.height,
                    ),
                )
            }
        }
    };
    let footer = a.bottom().saturating_sub(footer_h);
    Areas {
        header: Rect::new(a.x, a.y, a.width, header_h),
        metadata,
        lyrics,
        progress: Rect::new(a.x, footer, a.width, u16::from(footer_h > 0)),
        controls: Rect::new(
            a.x,
            footer + u16::from(footer_h > 0),
            a.width,
            u16::from(footer_h >= 2),
        ),
        status: if footer_h >= 3 {
            Rect::new(a.x, footer + 2, a.width, footer_h - 2)
        } else {
            Rect::default()
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tiers() {
        assert_eq!(tier(Rect::new(0, 0, 60, 20)), Tier::Compact);
        assert_eq!(tier(Rect::new(0, 0, 100, 30)), Tier::Standard);
        assert_eq!(tier(Rect::new(0, 0, 160, 40)), Tier::Wide);
    }
    #[test]
    fn areas_fit_even_tiny_terminal() {
        for w in 1..180 {
            for h in 1..50 {
                let a = Rect::new(0, 0, w, h);
                let l = areas(a, false);
                for r in [
                    l.header, l.metadata, l.lyrics, l.progress, l.controls, l.status,
                ] {
                    if !r.is_empty() {
                        assert!(r.right() <= w && r.bottom() <= h, "{w}x{h}: {r:?}");
                    }
                }
            }
        }
    }
}

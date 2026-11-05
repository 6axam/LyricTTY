use ratatui::layout::Rect;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Compact,
    Standard,
    Wide,
}
pub fn tier(area: Rect) -> Tier {
    if area.width < 60 || area.height < 22 {
        Tier::Compact
    } else if area.width >= 145 {
        Tier::Wide
    } else {
        Tier::Standard
    }
}
#[derive(Debug)]
pub struct Areas {
    pub cover: Rect,
    pub panel: Rect,
    pub header: Rect,
    pub metadata: Rect,
    pub lyrics: Rect,
    pub progress: Rect,
    pub controls: Rect,
    pub status: Rect,
}
pub fn areas(a: Rect, karaoke: bool) -> Areas {
    areas_with_cell(a, karaoke, (8, 16))
}
pub fn areas_with_cell(a: Rect, karaoke: bool, cell: (u16, u16)) -> Areas {
    if !karaoke && tier(a) != Tier::Compact {
        let margin = (a.width * 4 / 100).max(2);
        let gap = (a.width * 3 / 100).max(3);
        let usable = a.width.saturating_sub(margin * 2 + gap);
        let available_h = a.height * 76 / 100;
        let aspect = cell.0.max(1) as f32 / cell.1.max(1) as f32;
        let mut left = usable * 53 / 100;
        let mut artwork_h = (left as f32 * aspect).round() as u16;
        let mut panel_h = ((artwork_h as f32 * 0.34).round() as u16).max(8);
        if artwork_h + panel_h + 1 > available_h {
            artwork_h = ((available_h.saturating_sub(1) as f32 / 1.34).floor() as u16)
                .min(available_h.saturating_sub(9))
                .max(3);
            panel_h = ((artwork_h as f32 * 0.34).round() as u16).max(8);
            left = left.min((artwork_h as f32 / aspect).round() as u16);
        }
        let group_h = artwork_h + panel_h + 1;
        let y = a.y + a.height.saturating_sub(group_h) / 2;
        let x = a.x + margin;
        let panel_y = y + artwork_h + 1;
        let progress_y = panel_y + panel_h * 53 / 100;
        return Areas {
            cover: Rect::new(x, y, left, artwork_h),
            panel: Rect::new(x, panel_y, left, panel_h),
            header: Rect::default(),
            metadata: Rect::new(x, y, left, progress_y - y),
            lyrics: Rect::new(
                x + left + gap,
                y,
                a.right().saturating_sub(margin + x + left + gap),
                group_h,
            ),
            progress: Rect::new(x + 2, progress_y, left.saturating_sub(4), 1),
            controls: Rect::new(
                x + 2,
                panel_y + panel_h * 78 / 100,
                left.saturating_sub(4),
                1,
            ),
            status: Rect::new(
                a.x + margin,
                a.bottom() - 1,
                a.width.saturating_sub(margin * 2),
                1,
            ),
        };
    }
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
    let mut progress = Rect::new(
        a.x,
        a.bottom().saturating_sub(footer_h),
        a.width,
        u16::from(footer_h > 0),
    );
    let mut controls = Rect::new(
        a.x,
        progress.y + u16::from(footer_h > 0),
        a.width,
        u16::from(footer_h >= 2),
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
                    a.width * 35 / 100
                } else {
                    a.width * 40 / 100
                };
                progress = Rect::new(
                    a.x + 1,
                    a.bottom().saturating_sub(footer_h),
                    w.saturating_sub(2),
                    u16::from(footer_h > 0),
                );
                controls = Rect::new(
                    progress.x,
                    progress.y + u16::from(footer_h > 0),
                    progress.width,
                    u16::from(footer_h >= 2),
                );
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
        cover: Rect::default(),
        panel: Rect::default(),
        header: Rect::new(a.x, a.y, a.width, header_h),
        metadata,
        lyrics,
        progress,
        controls,
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

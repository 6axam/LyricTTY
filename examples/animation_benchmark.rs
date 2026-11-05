//! Offline renderer-only benchmark; does not include terminal/GPU presentation.
use lyrictty::{lyric_view::ScrollState, lyrics, pixel_lyrics::PixelLyrics, theme::Palette};
use ratatui::layout::Rect;
use std::time::Instant;
fn main() -> anyhow::Result<()> {
    let lines = lyrics::parse_lrc(include_str!("demo.lrc"));
    let mut renderer = PixelLyrics::system_font((12, 26))?;
    let mut scroll = ScrollState::default();
    let mut output = Vec::new();
    let mut samples = Vec::new();
    let mut bytes = 0;
    for frame in 0..600 {
        let position = frame as f32 / 60.0 + 4.0;
        let active = lyrics::active_line(&lines, position).unwrap_or(0);
        scroll.target(active as f32);
        scroll.advance(1.0 / 60.0, 0.85);
        let start = Instant::now();
        renderer.begin_frame();
        renderer.prepare(
            Rect::new(0, 0, 80, 25),
            &lines,
            scroll.current,
            position,
            Palette::default(),
            false,
        )?;
        renderer.flush(&mut output)?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        bytes += output.len();
        output.clear();
    }
    samples.sort_by(f64::total_cmp);
    println!("600 simulated 60 FPS frames: median {:.3} ms, p95 {:.3} ms, local graphics data {:.1} KiB/s",samples[300],samples[570],bytes as f64/1024.0/10.0);
    Ok(())
}

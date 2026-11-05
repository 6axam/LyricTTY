use crate::{
    lyric_view::{self, Hit, ScrollState},
    lyrics::{self, LyricLine},
    theme::Palette,
};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
    Frame,
};
use std::{process::Command, time::Instant};

pub struct App {
    pub should_quit: bool,
    playing: bool,
    position: f32,
    duration: f32,
    volume: u8,
    debug: bool,
    lines: Vec<LyricLine>,
    palette: Palette,
    scroll: ScrollState,
    hits: Vec<Hit>,
    last: Instant,
    status: String,
}
impl App {
    pub fn new(debug: bool) -> Self {
        Self { should_quit:false, playing:false, position:0.0, duration:238.0, volume:70, debug, lines: lyrics::parse_lrc("[00:00.00]Welcome to LyricTTY\n[00:04.00]Play music from your terminal\n[00:09.00]Connect a browser through MPRIS\n[00:15.00]Every line can seek the song\n[00:22.00]Instrumental · · ·\n[00:30.00]Your lyrics stay alive"), palette: crate::theme::from_artwork((32,65,43)), scroll:ScrollState::default(), hits: Vec::new(), last:Instant::now(), status:"Demo mode · press ? for help".into() }
    }
    pub fn tick(&mut self) {
        let dt = self.last.elapsed().as_secs_f32();
        self.last = Instant::now();
        if self.playing {
            self.position = (self.position + dt).min(self.duration);
        }
        if let Some(i) = lyrics::active_line(&self.lines, self.position) {
            self.scroll.target(i as f32);
        }
        self.scroll.advance(dt, 0.8);
    }
    pub fn handle_key(&mut self, k: KeyEvent) {
        match (k.code, k.modifiers) {
            (KeyCode::Char('q'), _)
            | (KeyCode::Esc, _)
            | (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,
            (KeyCode::Char(' '), _) => self.playing = !self.playing,
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                self.position = (self.position + 5.0).min(self.duration)
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                self.position = (self.position - 5.0).max(0.0)
            }
            (KeyCode::Up, _) => self.volume = (self.volume + 5).min(100),
            (KeyCode::Down, _) => self.volume = self.volume.saturating_sub(5),
            (KeyCode::Char('?'), _) => {
                self.status =
                    "Space play/pause · arrows seek/volume · q quit · click lyrics to seek".into()
            }
            _ => {}
        }
    }
    pub fn handle_mouse(&mut self, m: MouseEvent) {
        if let MouseEventKind::Down(_) = m.kind {
            if let Some(hit) = lyric_view::hit_test(&self.hits, m.column, m.row) {
                self.position = hit.timestamp;
                self.status = format!("Seeked to line {}", hit.line + 1);
            }
        }
    }
    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(
            Block::default().style(Style::default().bg(self.palette.bg)),
            area,
        );
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Min(8),
                    Constraint::Length(5),
                ]
                .as_ref(),
            )
            .split(area);
        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                "  LYRICTTY",
                Style::default()
                    .fg(self.palette.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   ·   NOW PLAYING   ·   "),
            Span::styled(
                if self.playing { "PLAYING" } else { "PAUSED" },
                Style::default().fg(self.palette.accent),
            ),
        ]))
        .block(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(self.palette.muted)),
        );
        f.render_widget(header, chunks[0]);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(32), Constraint::Percentage(68)].as_ref())
            .split(chunks[1]);
        self.draw_meta(f, body[0]);
        self.draw_lyrics(f, body[1]);
        let ratio = (self.position / self.duration).clamp(0.0, 1.0);
        let footer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Length(3)].as_ref())
            .split(chunks[2]);
        f.render_widget(
            Gauge::default()
                .block(Block::default().title(format!(
                    "  {} / {}  ",
                    fmt(self.position),
                    fmt(self.duration)
                )))
                .gauge_style(Style::default().fg(self.palette.accent))
                .ratio(ratio as f64),
            footer[0],
        );
        f.render_widget(
            Paragraph::new(format!(
                "  ◀   {}   ▶       Volume {:>3}%       {}",
                if self.playing { "❚❚" } else { "▶" },
                self.volume,
                if self.debug {
                    format!(
                        "{} · {:.2}s · {}×{}",
                        self.status, self.position, area.width, area.height
                    )
                } else {
                    self.status.clone()
                }
            ))
            .style(Style::default().fg(self.palette.muted)),
            footer[1],
        );
    }
    fn draw_meta(&self, f: &mut Frame, a: Rect) {
        let art = "      ╭────────╮\n      │  ♫ ♫   │\n      │  ♫ ♫   │\n      ╰────────╯";
        let p = Paragraph::new(vec![
            Line::from(art),
            Line::from(""),
            Line::from(Span::styled(
                "LyricTTY Session",
                Style::default()
                    .fg(self.palette.fg)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from("Terminal music center"),
            Line::from(""),
            Line::from(Span::styled(
                "SOURCE",
                Style::default().fg(self.palette.muted),
            )),
            Line::from("● Native demo / MPRIS Auto"),
        ])
        .block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(Style::default().fg(self.palette.panel)),
        )
        .wrap(Wrap { trim: false });
        f.render_widget(p, a);
    }
    fn draw_lyrics(&mut self, f: &mut Frame, a: Rect) {
        let block = Block::default()
            .title("  LYRICS · click a line to seek ")
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(self.palette.accent));
        let inner = block.inner(a);
        f.render_widget(block, a);
        self.hits = lyric_view::render(
            f,
            inner,
            &self.lines,
            self.scroll.current,
            self.position,
            self.palette,
        );
    }
}
fn fmt(s: f32) -> String {
    format!("{}:{:02}", (s as u32) / 60, (s as u32) % 60)
}
pub fn doctor() -> Result<()> {
    println!("LyricTTY doctor\n✓ Rust runtime\n✓ terminal UI\n{} mpv\n{} yt-dlp\n{} MPRIS/playerctl\n! YouTube Music account not configured",check("mpv"),check("yt-dlp"),check("playerctl"));
    Ok(())
}
fn check(x: &str) -> &'static str {
    if Command::new(x).arg("--version").output().is_ok() {
        "✓"
    } else {
        "!"
    }
}

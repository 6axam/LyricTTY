use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lyrictty::{
    artwork::Artwork,
    config::Config,
    content::LyricsContent,
    instrumental::InstrumentalState,
    layout::{self, Tier},
    lyric_view::{self, Hit, ScrollState},
    lyrics,
    pixel_lyrics::PixelLyrics,
    playback::{
        select_auto,
        service::{Event, PlaybackService},
        AutoSelection, PlaybackCommand, PlaybackSnapshot, PlaybackStatus,
    },
    theme::{self, ThemeTransition},
};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol, Resize, StatefulImage};
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overlay {
    None,
    Sources,
    Open,
    Help,
    Settings,
}
pub struct App {
    pub should_quit: bool,
    pub config: Config,
    pub dirty: bool,
    pub service: Option<PlaybackService>,
    pub content: LyricsContent,
    pub asset_request: Option<lyrictty::assets::AssetService>,
    sources: Vec<PlaybackSnapshot>,
    manual_source: Option<String>,
    active_source: Option<String>,
    track_key: String,
    asset_key: String,
    debug: bool,
    scroll: ScrollState,
    hits: Vec<Hit>,
    theme: ThemeTransition,
    artwork: Option<Artwork>,
    pub picker: Option<Picker>,
    cover_protocol: Option<StatefulProtocol>,
    pixel_lyrics: Option<PixelLyrics>,
    pixel_active: bool,
    panel_renderer: Option<lyrictty::player_panel::PanelRenderer>,
    panel_protocol: Option<StatefulProtocol>,
    panel_key: String,
    graphical_panel: bool,
    instrumental: InstrumentalState,
    manual_line: Option<usize>,
    plain_scroll: u16,
    manual_until: Instant,
    overlay: Overlay,
    selection: usize,
    url_input: String,
    status: String,
    karaoke: bool,
    last: Instant,
    started: Instant,
    lyric_rect: Rect,
    progress_rect: Rect,
    controls_rect: Rect,
    overlay_rect: Rect,
    startup_url: Option<String>,
    local_lyrics: Option<std::path::PathBuf>,
    demo: bool,
}
impl App {
    pub fn new(config: Config, debug: bool) -> Self {
        Self {
            should_quit: false,
            config,
            dirty: true,
            service: None,
            content: LyricsContent::default(),
            asset_request: None,
            sources: Vec::new(),
            manual_source: None,
            active_source: None,
            track_key: String::new(),
            asset_key: String::new(),
            debug,
            scroll: ScrollState::default(),
            hits: Vec::new(),
            theme: ThemeTransition::default(),
            artwork: None,
            picker: None,
            cover_protocol: None,
            pixel_lyrics: None,
            pixel_active: false,
            panel_renderer: None,
            panel_protocol: None,
            panel_key: String::new(),
            graphical_panel: false,
            instrumental: InstrumentalState::default(),
            manual_line: None,
            plain_scroll: 0,
            manual_until: Instant::now(),
            overlay: Overlay::None,
            selection: 0,
            url_input: String::new(),
            status: "Discovering music sources… · o open URL · s sources · ? help".into(),
            karaoke: false,
            last: Instant::now(),
            started: Instant::now(),
            lyric_rect: Rect::default(),
            progress_rect: Rect::default(),
            controls_rect: Rect::default(),
            overlay_rect: Rect::default(),
            startup_url: None,
            local_lyrics: None,
            demo: false,
        }
    }
    pub fn setup_pixel_lyrics(&mut self) {
        self.panel_renderer = self
            .picker
            .as_ref()
            .filter(|p| p.protocol_type() == ratatui_image::picker::ProtocolType::Kitty)
            .and_then(|_| lyrictty::player_panel::PanelRenderer::new().ok());
        self.pixel_lyrics = self
            .picker
            .as_ref()
            .filter(|p| {
                p.protocol_type() == ratatui_image::picker::ProtocolType::Kitty
                    && std::env::var_os("TMUX").is_none()
                    && std::env::var_os("TMUX").is_none()
            })
            .and_then(|p| PixelLyrics::system_font(p.font_size()).ok());
    }
    pub fn flush_graphics(&mut self, output: &mut impl std::io::Write) -> std::io::Result<()> {
        if let Some(renderer) = &mut self.pixel_lyrics {
            renderer.flush(output)?;
        }
        Ok(())
    }
    pub fn resize_graphics(&mut self) {
        if let (Some(renderer), Ok(size)) =
            (&mut self.pixel_lyrics, crossterm::terminal::window_size())
        {
            if size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0 {
                renderer.set_cell_size((size.width / size.columns, size.height / size.rows));
            }
        }
        self.dirty = true;
    }
    pub fn cleanup_graphics(&mut self, output: &mut impl std::io::Write) -> std::io::Result<()> {
        if self.panel_protocol.is_some() {
            let id = 0x5000_0000 | (std::process::id() & 0x00ff_ffff);
            output.write_all(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes())?;
        }
        if let Some(renderer) = &mut self.pixel_lyrics {
            renderer.cleanup(output)?;
        }
        Ok(())
    }
    pub fn startup(&mut self, url: Option<String>, lyrics: Option<std::path::PathBuf>, demo: bool) {
        self.startup_url = url;
        self.local_lyrics = lyrics;
        self.demo = demo;
    }
    fn source(&self) -> Option<&PlaybackSnapshot> {
        self.active_source
            .as_ref()
            .and_then(|id| self.sources.iter().find(|s| &s.source_id == id))
    }
    fn position(&self) -> f32 {
        self.source()
            .map(|s| s.position_now() as f32)
            .unwrap_or(0.0)
    }
    fn lyric_position(&self) -> f32 {
        (self.position() + self.config.lyrics_offset_ms as f32 / 1000.0).max(0.0)
    }
    fn command(&mut self, c: PlaybackCommand) {
        if let (Some(id), Some(service)) = (&self.active_source, &self.service) {
            service.command(id, c);
        } else {
            self.status = "Choose an available source with s".into();
        }
        self.dirty = true;
    }
    fn seek(&mut self, timestamp: f32) {
        if self.source().is_some_and(|s| s.capabilities.seek) {
            self.command(PlaybackCommand::SetPosition(timestamp.max(0.0) as f64));
        } else {
            self.status = "Seeking is unavailable for this source".into();
        }
    }
    fn update_sources(&mut self, sources: Vec<PlaybackSnapshot>) {
        let disconnected = self
            .active_source
            .as_ref()
            .is_some_and(|id| !sources.iter().any(|s| &s.source_id == id));
        self.sources = sources;
        if disconnected {
            self.status = "Source disconnected · choose a source with s".into();
            self.manual_source = None;
            self.active_source = None;
        }
        if let Some(id) = &self.manual_source {
            self.active_source = Some(id.clone());
        } else {
            match select_auto(&self.sources) {
                AutoSelection::One(id) => self.active_source = Some(id),
                AutoSelection::Ambiguous(_) => {
                    if self.overlay == Overlay::None {
                        self.overlay = Overlay::Sources;
                        self.selection = 0;
                    }
                    self.status = "Multiple sources are playing · select one".into();
                }
                AutoSelection::None => {
                    if self.active_source.is_none() {
                        self.active_source = self
                            .sources
                            .iter()
                            .find(|s| s.source_id != "native")
                            .or_else(|| self.sources.first())
                            .map(|s| s.source_id.clone());
                    }
                }
            }
        }
        if self.sources.iter().any(|s| s.source_id == "native") {
            if let Some(url) = self.startup_url.take() {
                self.open_native(url);
            }
        }
        let key = self
            .source()
            .map(|s| {
                serde_json::to_string(&(
                    &s.source_id,
                    &s.track.id,
                    &s.track.artist,
                    &s.track.title,
                    &s.track.album,
                ))
                .unwrap_or_default()
            })
            .unwrap_or_default();
        if key != self.track_key {
            self.track_key = key.clone();
            self.content = LyricsContent::default();
            self.artwork = None;
            self.cover_protocol = None;
            self.manual_line = None;
            self.plain_scroll = 0;
            self.scroll = ScrollState::default();
            self.theme.set(
                theme::Palette::default(),
                self.config.theme_transition_ms as f32 / 1000.0,
            );
        }
        // Browsers and extractors can publish artwork/duration after the title.
        // Refresh assets without resetting the lyric viewport for the same song.
        let asset_key = self
            .source()
            .map(|s| {
                serde_json::to_string(&(
                    &key,
                    s.track.duration.round() as i64,
                    &s.track.artwork_url,
                ))
                .unwrap_or_default()
            })
            .unwrap_or_default();
        if asset_key != self.asset_key {
            self.asset_key = asset_key.clone();
            if let Some(s) = self.source() {
                if !s.track.title.is_empty() {
                    if let Some(worker) = &self.asset_request {
                        worker.request(
                            asset_key,
                            s.track.clone(),
                            self.local_lyrics.clone(),
                            self.demo,
                        );
                    }
                }
            }
        }
        self.dirty = true;
    }
    fn open_native(&mut self, url: String) {
        if let Some(service) = &self.service {
            self.manual_source = Some("native".into());
            self.active_source = Some("native".into());
            service.command("native", PlaybackCommand::Open(url));
            self.status = "Opening media in mpv…".into();
        }
    }
    pub fn tick(&mut self) {
        let dt = self.last.elapsed().as_secs_f32();
        self.last = Instant::now();
        let mut events = Vec::new();
        if let Some(service) = &self.service {
            while let Ok(event) = service.events.try_recv() {
                events.push(event);
            }
        }
        for event in events {
            match event {
                Event::Sources(s) => self.update_sources(s),
                Event::Error(e) => {
                    self.status = e;
                    self.dirty = true;
                }
            }
        }
        let mut assets = Vec::new();
        if let Some(worker) = &self.asset_request {
            while let Ok(event) = worker.events.try_recv() {
                assets.push(event);
            }
        }
        for event in assets {
            match event {
                lyrictty::assets::AssetEvent::Lyrics { key, content } if key == self.asset_key => {
                    self.status = format!("{} · s sources · o open · ? help", content.quality);
                    self.content = content;
                    self.dirty = true;
                }
                lyrictty::assets::AssetEvent::Artwork { key, artwork } if key == self.asset_key => {
                    self.theme.set(
                        artwork.palette,
                        self.config.theme_transition_ms as f32 / 1000.0,
                    );
                    self.artwork = Some(artwork);
                    self.cover_protocol = self.picker.as_ref().and_then(|picker| {
                        self.artwork.as_ref().map(|art| {
                            let width = art
                                .image
                                .width()
                                .min(art.image.height().saturating_mul(2))
                                .max(1);
                            let height = art.image.height().min(width / 2).max(1);
                            let cropped = image::imageops::crop_imm(
                                &art.image,
                                (art.image.width() - width) / 2,
                                (art.image.height() - height) / 2,
                                width,
                                height,
                            )
                            .to_image();
                            let mut cover = image::DynamicImage::ImageRgb8(cropped).into_rgba8();
                            let radius = (height as f32 * 0.075).max(1.0);
                            for (x, y, pixel) in cover.enumerate_pixels_mut() {
                                let dx = (radius
                                    - (x as f32 + 0.5).min(width as f32 - x as f32 - 0.5))
                                .max(0.0);
                                let dy = (radius
                                    - (y as f32 + 0.5).min(height as f32 - y as f32 - 0.5))
                                .max(0.0);
                                pixel.0[3] = ((radius + 0.5 - (dx * dx + dy * dy).sqrt())
                                    .clamp(0.0, 1.0)
                                    * 255.0) as u8;
                            }
                            picker.new_resize_protocol(image::DynamicImage::ImageRgba8(cover))
                        })
                    });
                    self.dirty = true;
                }
                _ => {}
            }
        }
        if self.manual_line.is_some() && Instant::now() >= self.manual_until {
            self.manual_line = None;
            self.dirty = true;
        }
        let target = self
            .manual_line
            .or_else(|| lyrics::active_line(&self.content.lines, self.lyric_position()))
            .or_else(|| {
                self.content
                    .lines
                    .iter()
                    .rposition(|line| line.start <= self.lyric_position())
            })
            .unwrap_or(0);
        self.scroll.target(target as f32);
        self.scroll
            .advance(dt, self.config.scroll_duration_ms as f32 / 1000.0);
        self.theme.advance(dt);
        let duration = self
            .source()
            .map(|s| s.track.duration as f32)
            .unwrap_or(0.0);
        self.instrumental.update(
            &self.content.lines,
            self.lyric_position(),
            duration,
            self.config.instrumental_threshold_secs,
            self.config.instrumental_fade_secs,
            self.content.instrumental,
        );
        self.dirty |= self.scroll.animating()
            || self.theme.animating()
            || self
                .source()
                .is_some_and(|s| s.status == PlaybackStatus::Playing);
    }
    pub fn animating(&self) -> bool {
        self.scroll.animating()
            || self.theme.animating()
            || self.source().is_some_and(|s| {
                s.status == PlaybackStatus::Playing
                    && (!self.content.lines.is_empty() || self.instrumental.intensity > 0.0)
            })
    }
    fn manual_scroll(&mut self, delta: isize) {
        if self.content.lines.is_empty() {
            self.plain_scroll = self.plain_scroll.saturating_add_signed(delta as i16);
            return;
        }
        let current = self
            .manual_line
            .unwrap_or(self.scroll.current.round().max(0.0) as usize);
        self.manual_line = Some(
            current
                .saturating_add_signed(delta)
                .min(self.content.lines.len() - 1),
        );
        self.manual_until =
            Instant::now() + Duration::from_secs(self.config.manual_follow_timeout_secs as u64);
    }
    pub fn handle_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        self.dirty = true;
        if self.overlay != Overlay::None {
            self.overlay_key(k);
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char(' ') => self.command(PlaybackCommand::PlayPause),
            KeyCode::Char('n') => self.command(PlaybackCommand::Next),
            KeyCode::Char('p') => self.command(PlaybackCommand::Previous),
            KeyCode::Char('R') => self.command(PlaybackCommand::Repeat),
            KeyCode::Char('x') => self.command(PlaybackCommand::Shuffle),
            KeyCode::Right | KeyCode::Char('l') => self.command(PlaybackCommand::Seek(5.0)),
            KeyCode::Left | KeyCode::Char('h') => self.command(PlaybackCommand::Seek(-5.0)),
            KeyCode::Up | KeyCode::Char('k') => self.volume(0.05),
            KeyCode::Down | KeyCode::Char('j') => self.volume(-0.05),
            KeyCode::PageUp => self.manual_scroll(-1),
            KeyCode::PageDown => self.manual_scroll(1),
            KeyCode::Enter => {
                if let Some(line) = self.manual_line.and_then(|i| self.content.lines.get(i)) {
                    self.seek(line.start);
                }
            }
            KeyCode::Char('f') => self.manual_line = None,
            KeyCode::Char('s') => {
                self.overlay = Overlay::Sources;
                self.selection = 0;
            }
            KeyCode::Char('o') | KeyCode::Char('/') => {
                self.overlay = Overlay::Open;
                self.url_input.clear();
            }
            KeyCode::Char('L') => self.karaoke = !self.karaoke,
            KeyCode::Esc => self.karaoke = false,
            KeyCode::Char(',') => {
                self.overlay = Overlay::Settings;
                self.selection = 0;
            }
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }
    pub fn handle_paste(&mut self, text: &str) {
        if self.overlay == Overlay::Open {
            self.url_input
                .extend(text.chars().filter(|c| !c.is_control()));
            self.dirty = true;
        }
    }
    fn volume(&mut self, delta: f64) {
        let volume = self.source().map(|s| s.volume).unwrap_or(0.7);
        self.command(PlaybackCommand::Volume((volume + delta).clamp(0.0, 1.0)));
    }
    fn overlay_key(&mut self, k: KeyEvent) {
        if k.code == KeyCode::Esc {
            self.overlay = Overlay::None;
            return;
        }
        match self.overlay {
            Overlay::Open => match k.code {
                KeyCode::Char(c) => self.url_input.push(c),
                KeyCode::Backspace => {
                    self.url_input.pop();
                }
                KeyCode::Enter => {
                    let url = self.url_input.trim().to_owned();
                    if !url.is_empty() {
                        self.open_native(url);
                        self.overlay = Overlay::None;
                    }
                }
                _ => {}
            },
            Overlay::Sources => match k.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selection = self.selection.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selection = (self.selection + 1).min(self.sources.len())
                }
                KeyCode::Enter => self.select_source(),
                _ => {}
            },
            Overlay::Settings => match k.code {
                KeyCode::Up => self.selection = self.selection.saturating_sub(1),
                KeyCode::Down => self.selection = (self.selection + 1).min(6),
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Enter => {
                    let sign = if k.code == KeyCode::Left { -1 } else { 1 };
                    match self.selection {
                        0 => {
                            self.config.fps = match self.config.fps {
                                10..=15 => 30,
                                16..=30 => 60,
                                _ => 15,
                            }
                        }
                        1 => {
                            self.config.scroll_duration_ms =
                                (self.config.scroll_duration_ms as i32 + sign * 100).clamp(0, 2000)
                                    as u32
                        }
                        2 => {
                            self.config.theme_transition_ms =
                                (self.config.theme_transition_ms as i32 + sign * 100).clamp(0, 2000)
                                    as u32
                        }
                        3 => self.config.show_artwork = !self.config.show_artwork,
                        4 => {
                            self.config.lyrics_offset_ms =
                                (self.config.lyrics_offset_ms + sign * 100).clamp(-10000, 10000)
                        }
                        5 => self.config.low_data = !self.config.low_data,
                        6 => self.config.pixel_lyrics = !self.config.pixel_lyrics,
                        _ => {}
                    }
                    self.status = if self.config.save(&lyrictty::config::config_path()).is_ok() {
                        "Settings saved · network changes apply on restart".into()
                    } else {
                        "Could not save settings".into()
                    };
                }
                _ => {}
            },
            Overlay::Help => {
                if matches!(k.code, KeyCode::Enter | KeyCode::Char('?')) {
                    self.overlay = Overlay::None;
                }
            }
            Overlay::None => {}
        }
    }
    fn select_source(&mut self) {
        if self.selection == 0 {
            self.manual_source = None;
            self.active_source = None;
        } else if let Some(s) = self.sources.get(self.selection - 1) {
            self.manual_source = Some(s.source_id.clone());
            self.active_source = self.manual_source.clone();
        }
        self.overlay = Overlay::None;
        self.update_sources(self.sources.clone());
    }
    pub fn handle_mouse(&mut self, m: MouseEvent) {
        self.dirty = true;
        if self.overlay != Overlay::None {
            if self.overlay == Overlay::Sources
                && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                && self.overlay_rect.contains((m.column, m.row).into())
                && m.row > self.overlay_rect.y
                && m.row < self.overlay_rect.bottom().saturating_sub(1)
                && m.column > self.overlay_rect.x
                && m.column < self.overlay_rect.right().saturating_sub(1)
            {
                let row = m.row.saturating_sub(self.overlay_rect.y + 1) as usize;
                if row <= self.sources.len() {
                    self.selection = row;
                    self.select_source();
                }
            }
            return;
        }
        let in_lyrics = self.lyric_rect.contains((m.column, m.row).into());
        match m.kind {
            MouseEventKind::ScrollUp => {
                if in_lyrics {
                    self.manual_scroll(-1)
                } else {
                    self.volume(0.05)
                }
            }
            MouseEventKind::ScrollDown => {
                if in_lyrics {
                    self.manual_scroll(1)
                } else {
                    self.volume(-0.05)
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.progress_rect.contains((m.column, m.row).into()) {
                    let ratio = (m.column.saturating_sub(self.progress_rect.x)) as f32
                        / self.progress_rect.width.saturating_sub(1).max(1) as f32;
                    if let Some(s) = self.source() {
                        self.seek(ratio * s.track.duration as f32);
                    }
                } else if let Some(hit) = lyric_view::hit_test(&self.hits, m.column, m.row) {
                    let t = hit.timestamp;
                    self.seek(t);
                } else if self.controls_rect.contains((m.column, m.row).into()) {
                    if self.graphical_panel {
                        let slot = m.column.saturating_sub(self.controls_rect.x) as u32 * 5
                            / self.controls_rect.width.max(1) as u32;
                        self.command(match slot {
                            0 => PlaybackCommand::Shuffle,
                            1 => PlaybackCommand::Previous,
                            2 => PlaybackCommand::PlayPause,
                            3 => PlaybackCommand::Next,
                            _ => PlaybackCommand::Repeat,
                        });
                        return;
                    }
                    match m.column.saturating_sub(self.controls_rect.x) {
                        0..=2 => self.command(PlaybackCommand::Shuffle),
                        3..=5 => self.command(PlaybackCommand::Previous),
                        6..=9 => self.command(PlaybackCommand::PlayPause),
                        10..=12 => self.command(PlaybackCommand::Next),
                        13..=15 => self.command(PlaybackCommand::Repeat),
                        _ => {
                            self.manual_line = None;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    pub fn draw(&mut self, f: &mut Frame) {
        if let Some(renderer) = &mut self.pixel_lyrics {
            renderer.begin_frame();
        }
        self.pixel_active = false;
        let a = f.area();
        let p = self.theme.palette();
        let cell = self
            .pixel_lyrics
            .as_ref()
            .map(|r| r.cell_size())
            .or_else(|| self.picker.as_ref().map(|p| p.font_size()))
            .unwrap_or((8, 16));
        let areas = layout::areas_with_cell(a, self.karaoke, cell);
        self.lyric_rect = areas.lyrics;
        self.progress_rect = areas.progress;
        self.controls_rect = areas.controls;
        f.render_widget(
            Block::default().style(Style::default().bg(p.bg).fg(p.fg)),
            a,
        );
        // Quiet vertical gradient, kept dark enough for text over every row.
        for y in a.y..a.bottom() {
            let color = theme::mix(
                p.bg,
                p.panel,
                (y - a.y) as f32 / a.height.max(1) as f32 * 0.45,
            );
            f.render_widget(
                Block::default().style(Style::default().bg(color)),
                Rect::new(a.x, y, a.width, 1),
            );
        }
        self.graphical_panel = self.draw_player_panel(f, areas.panel, cell);
        if !areas.panel.is_empty() && !self.graphical_panel {
            f.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(theme::mix(p.panel, p.muted, 0.12)))
                    .style(Style::default().bg(p.panel)),
                areas.panel,
            );
        }
        let source = self.source();
        let source_name = source
            .map(|s| s.source_name.as_str())
            .unwrap_or("No source");
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  LYRICTTY", Style::default().fg(p.accent).bold()),
                Span::styled(
                    format!(
                        "   {} · {}",
                        if self.manual_source.is_none() {
                            "AUTO"
                        } else {
                            "SOURCE"
                        },
                        source_name
                    ),
                    Style::default().fg(p.muted),
                ),
            ])),
            areas.header,
        );
        if !areas.cover.is_empty() {
            if self.config.show_artwork {
                self.draw_artwork(f, areas.cover);
            }
            if !self.graphical_panel {
                self.draw_meta(
                    f,
                    Rect::new(
                        areas.panel.x + 2,
                        areas.panel.y + 1,
                        areas.panel.width.saturating_sub(4),
                        3,
                    ),
                    Tier::Compact,
                );
            }
        } else if !areas.metadata.is_empty() {
            self.draw_meta(f, areas.metadata, tier_for(a));
        }
        self.hits.clear();
        let title = if self.manual_line.is_some() {
            "  LYRICS · LIVE ↓ [f] "
        } else {
            "  ♪ Текст песни  ·  LIVE "
        };
        let block = if tier_for(a) == Tier::Compact || self.karaoke {
            Block::default()
                .title(title)
                .style(Style::default().fg(p.muted))
        } else {
            Block::default().style(Style::default().fg(p.muted))
        };
        let inner = block.inner(areas.lyrics);
        f.render_widget(block, areas.lyrics);
        if self.content.lines.is_empty() {
            let message = if !self.content.plain.is_empty() {
                self.content.plain.clone()
            } else if self.content.instrumental {
                String::new()
            } else if self.source().is_none() {
                "Play music in your browser, or press o to open a URL.\n\nLyricTTY will discover MPRIS players automatically.".into()
            } else {
                "No synchronized lyrics available\n\nLocal word timing: lyrictty --lyrics song.lrc"
                    .into()
            };
            f.render_widget(
                Paragraph::new(message)
                    .wrap(Wrap { trim: false })
                    .scroll((self.plain_scroll, 0))
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(p.muted)),
                padded(inner, 2),
            );
        } else {
            let mut faded = p;
            faded.fg = theme::mix(p.fg, p.bg, self.instrumental.intensity * 0.93);
            faded.muted = theme::mix(p.muted, p.bg, self.instrumental.intensity * 0.93);
            faded.accent = theme::mix(p.accent, p.bg, self.instrumental.intensity * 0.93);
            let position = self.lyric_position();
            if self.config.pixel_lyrics
                && self.overlay == Overlay::None
                && self.instrumental.intensity < 0.995
            {
                if let Some(renderer) = &mut self.pixel_lyrics {
                    if renderer.supports(&self.content.lines) {
                        if let Ok(hits) = renderer.prepare(
                            inner,
                            &self.content.lines,
                            self.scroll.current,
                            position,
                            faded,
                            self.karaoke,
                        ) {
                            self.hits = hits;
                            self.pixel_active = true;
                        }
                    }
                }
            }
            if !self.pixel_active {
                self.hits = lyric_view::render(
                    f,
                    inner,
                    &self.content.lines,
                    self.scroll.current,
                    position,
                    faded,
                );
            }
        }
        if !self.pixel_active {
            if let Some(renderer) = &mut self.pixel_lyrics {
                renderer.hide();
            }
        }
        if self.instrumental.intensity > 0.01 {
            self.draw_instrumental(f, inner);
        }
        let position = self.position();
        let duration = self
            .source()
            .map(|s| s.track.duration as f32)
            .unwrap_or(0.0);
        let ratio = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let bar_width = areas.progress.width.saturating_sub(18) as usize;
        let done = ((bar_width.saturating_sub(1)) as f32 * ratio).round() as usize;
        self.progress_rect = Rect::new(
            areas.progress.x + 8.min(areas.progress.width),
            areas.progress.y,
            bar_width as u16,
            areas.progress.height,
        );
        let bar = Line::from(vec![
            Span::styled(
                format!(" {:>5}  ", fmt(position)),
                Style::default().fg(p.muted),
            ),
            Span::styled("━".repeat(done), Style::default().fg(p.accent)),
            Span::styled(
                if bar_width > 0 { "●" } else { "" },
                Style::default().fg(p.fg),
            ),
            Span::styled(
                "─".repeat(bar_width.saturating_sub(done + 1)),
                Style::default().fg(theme::mix(p.panel, p.muted, 0.3)),
            ),
            Span::styled(format!("  {}", fmt(duration)), Style::default().fg(p.muted)),
        ]);
        if !self.graphical_panel {
            f.render_widget(Paragraph::new(bar), areas.progress);
        }
        let playing = self
            .source()
            .is_some_and(|s| s.status == PlaybackStatus::Playing);
        let volume = self
            .source()
            .map(|s| (s.volume * 100.0).round() as u32)
            .unwrap_or(0);
        let control_text = format!(" ⇄  ◀  {}  ▶  ↻", if playing { "❚❚" } else { "▶ " });
        let control_width = 16.min(areas.controls.width);
        self.controls_rect = Rect::new(
            areas.controls.x + areas.controls.width.saturating_sub(control_width) / 2,
            areas.controls.y,
            control_width,
            areas.controls.height,
        );
        if self.graphical_panel {
            self.progress_rect = Rect::new(
                areas.panel.x + areas.panel.width * 12 / 100,
                areas.panel.y + areas.panel.height * 54 / 100,
                areas.panel.width * 74 / 100,
                2,
            );
            self.controls_rect = Rect::new(
                areas.panel.x + areas.panel.width * 30 / 100,
                areas.panel.y + areas.panel.height * 70 / 100,
                areas.panel.width * 40 / 100,
                (areas.panel.height * 23 / 100).max(2),
            );
        } else {
            f.render_widget(
                Paragraph::new(control_text).style(Style::default().fg(p.fg).bg(p.panel)),
                self.controls_rect,
            );
        }
        if !self.graphical_panel && areas.controls.width >= 30 {
            f.render_widget(
                Paragraph::new(format!("◖ {volume}%"))
                    .style(Style::default().fg(p.muted).bg(p.panel)),
                Rect::new(areas.controls.x, areas.controls.y, 8, areas.controls.height),
            );
        }
        let status = if self.debug {
            format!(
                "  {} · {:?} · {:.2}s · {}×{} · {}",
                self.status,
                tier_for(a),
                position,
                a.width,
                a.height,
                if self.pixel_active {
                    "pixel"
                } else {
                    "terminal"
                }
            )
        } else {
            format!("  {}", self.status)
        };
        if self.debug || tier_for(a) == Tier::Compact || self.karaoke {
            f.render_widget(
                Paragraph::new(status).style(Style::default().fg(p.muted)),
                areas.status,
            );
        }
        if self.overlay != Overlay::None {
            self.draw_overlay(f);
        }
        self.dirty = false;
    }
    fn draw_player_panel(&mut self, f: &mut Frame, area: Rect, cell: (u16, u16)) -> bool {
        if area.is_empty() || self.panel_renderer.is_none() || self.picker.is_none() {
            return false;
        }
        let width = u32::from(area.width) * u32::from(cell.0);
        let height = u32::from(area.height) * u32::from(cell.1);
        if width < 100 || height < 60 || u64::from(width) * u64::from(height) > 4_000_000 {
            return false;
        }
        let source = self.source();
        let title = source
            .map(|s| s.track.title.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Your music, alive.");
        let artist = source
            .map(|s| s.track.artist.as_str())
            .unwrap_or("LyricTTY");
        let position = (self.position() * 2.0).floor() / 2.0;
        let duration = source.map(|s| s.track.duration as f32).unwrap_or(0.0);
        let playing = source.is_some_and(|s| s.status == PlaybackStatus::Playing);
        let volume = source.map(|s| s.volume as f32).unwrap_or(1.0);
        let palette = self.theme.palette();
        let key = format!("{width}:{height}:{title}:{artist}:{position}:{duration}:{playing}:{volume}:{palette:?}");
        if self.panel_key != key {
            let image = self.panel_renderer.as_ref().unwrap().render(
                width,
                height,
                &lyrictty::player_panel::PanelData {
                    title,
                    artist,
                    position,
                    duration,
                    playing,
                    volume,
                    palette,
                },
            );
            let source = ratatui_image::protocol::ImageSource::new(
                image::DynamicImage::ImageRgba8(image),
                cell,
                image::Rgba([0, 0, 0, 0]),
            );
            let protocol = ratatui_image::protocol::StatefulProtocolType::Kitty(
                ratatui_image::protocol::kitty::StatefulKitty::new(
                    0x5000_0000 | (std::process::id() & 0x00ff_ffff),
                    false,
                ),
            );
            self.panel_protocol = Some(StatefulProtocol::new(source, cell, protocol));
            self.panel_key = key;
        }
        if let Some(protocol) = &mut self.panel_protocol {
            f.render_stateful_widget(
                StatefulImage::default().resize(Resize::Scale(None)),
                area,
                protocol,
            );
            true
        } else {
            false
        }
    }
    fn draw_meta(&mut self, f: &mut Frame, a: Rect, tier: Tier) {
        let p = self.theme.palette();
        let source = self.source();
        let title = source
            .map(|s| s.track.title.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("Your music, alive.")
            .to_owned();
        let artist = source
            .map(|s| s.track.artist.as_str())
            .unwrap_or("Native + browser playback")
            .to_owned();
        if tier == Tier::Compact {
            let mut metadata = a;
            if a.height >= 7 && self.config.show_artwork {
                let art_rect = Rect::new(a.x + 2, a.y, 12, a.height.saturating_sub(1).min(6));
                self.draw_artwork(f, art_rect);
                metadata = Rect::new(
                    a.x + 17,
                    a.y + 1,
                    a.width.saturating_sub(19),
                    a.height.saturating_sub(2),
                );
            }
            f.render_widget(
                Paragraph::new(vec![
                    Line::styled(title, Style::default().fg(p.fg).bold()),
                    Line::from(""),
                    Line::styled(artist, Style::default().fg(p.muted)),
                ])
                .wrap(Wrap { trim: true }),
                metadata,
            );
            return;
        }
        let mut y = a.y;
        if self.config.show_artwork {
            let width = a.width;
            let height = a.height.saturating_sub(5);
            let rect = Rect::new(a.x, y, width, height);
            self.draw_artwork(f, rect);
            y = y.saturating_add(height + 2);
        } else {
            y = a.bottom().saturating_sub(3);
        }
        if y >= a.bottom() {
            return;
        }
        f.render_widget(
            Paragraph::new(vec![
                Line::styled(title, Style::default().fg(p.fg).bold()),
                Line::styled(artist, Style::default().fg(p.muted)),
            ])
            .wrap(Wrap { trim: true }),
            Rect::new(a.x + 2, y, a.width.saturating_sub(4), a.bottom() - y),
        );
    }
    fn draw_artwork(&mut self, f: &mut Frame, area: Rect) {
        if area.is_empty() {
            return;
        }
        if let Some(protocol) = &mut self.cover_protocol {
            f.render_stateful_widget(
                StatefulImage::default()
                    .resize(Resize::Scale(Some(image::imageops::FilterType::Lanczos3))),
                area,
                protocol,
            );
        } else if let Some(art) = &self.artwork {
            draw_cover(f, area, art);
        } else {
            let p = self.theme.palette();
            f.render_widget(
                Paragraph::new("\n\n     ♫\n\n  LyricTTY")
                    .style(Style::default().fg(p.accent).bg(p.panel))
                    .alignment(Alignment::Center),
                area,
            );
        }
    }
    fn draw_instrumental(&self, f: &mut Frame, a: Rect) {
        if a.width < 10 || a.height < 5 {
            return;
        }
        let p = self.theme.palette();
        let t = self.started.elapsed().as_secs_f32();
        let glyphs = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
        let bars: String = (0..17)
            .map(|i| {
                let v = ((t * 1.7 + i as f32 * 0.6).sin() * 0.5 + 0.5) * 6.0;
                glyphs[v as usize]
            })
            .collect();
        let rect = Rect::new(a.x, a.y + a.height / 2 - 1, a.width, 3);
        f.render_widget(
            Paragraph::new(format!("{bars}\n\nINSTRUMENTAL · ambient"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(theme::mix(
                    p.bg,
                    p.accent,
                    self.instrumental.intensity,
                ))),
            rect,
        );
    }
    fn draw_overlay(&mut self, f: &mut Frame) {
        let a = f.area();
        let width = a.width.saturating_sub(4).min(78);
        let height = a
            .height
            .saturating_sub(2)
            .min(if self.overlay == Overlay::Sources {
                self.sources.len() as u16 + 6
            } else {
                17
            });
        let rect = Rect::new(
            a.x + (a.width - width) / 2,
            a.y + (a.height - height) / 2,
            width,
            height,
        );
        self.overlay_rect = rect;
        f.render_widget(Clear, rect);
        let p = self.theme.palette();
        let title = match self.overlay {
            Overlay::Sources => " Sources ",
            Overlay::Open => " Open URL / local audio ",
            Overlay::Settings => " Settings · saved automatically ",
            _ => " Help ",
        };
        let block = Block::bordered()
            .title(title)
            .style(Style::default().bg(p.panel).fg(p.fg));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let lines: Vec<Line> = match self.overlay {
            Overlay::Sources => std::iter::once("Auto · select active source".to_owned())
                .chain(
                    self.sources
                        .iter()
                        .map(|s| format!("{} · {:?} · {}", s.source_name, s.status, s.track.title)),
                )
                .enumerate()
                .map(|(i, t)| {
                    Line::styled(
                        format!(" {} {}", if self.selection == i { "●" } else { "○" }, t),
                        Style::default().fg(if self.selection == i {
                            p.accent
                        } else {
                            p.muted
                        }),
                    )
                })
                .collect(),
            Overlay::Open => vec![
                Line::from("Paste a YouTube / YouTube Music / media URL or local path."),
                Line::from(""),
                Line::styled(
                    format!("> {}▏", self.url_input),
                    Style::default().fg(p.accent),
                ),
                Line::from(""),
                Line::from("Enter play · Esc cancel"),
            ],
            Overlay::Settings => [
                format!("FPS {}", self.config.fps),
                format!("Lyric transition {} ms", self.config.scroll_duration_ms),
                format!("Theme transition {} ms", self.config.theme_transition_ms),
                format!("Artwork {}", self.config.show_artwork),
                format!("Lyrics offset {} ms", self.config.lyrics_offset_ms),
                format!("Low data {}", self.config.low_data),
                format!("Pixel lyrics (Kitty) {}", self.config.pixel_lyrics),
            ]
            .iter()
            .enumerate()
            .map(|(i, t)| {
                Line::styled(
                    format!(" {} {}", if self.selection == i { "›" } else { " " }, t),
                    Style::default().fg(if self.selection == i {
                        p.accent
                    } else {
                        p.muted
                    }),
                )
            })
            .chain([
                Line::from(""),
                Line::from("↑↓ select · ←→ change · Esc close"),
            ])
            .collect(),
            _ => [
                "Space play/pause   n/p next/previous",
                "x shuffle         R repeat track",
                "←→ or h/l seek     ↑↓ or k/j volume",
                "s sources         o or / open URL",
                "L karaoke         , settings",
                "Wheel / PgUp/PgDn browse lyrics",
                "Enter seek selected lyric · f follow live",
                "Click lyrics/progress/transport to control",
                "q / Ctrl+C exit   Esc close overlay",
                "",
                "Word timing requires local enhanced LRC / Lyricsfile.",
                "LRCLIB falls back to line sync, plain text, or none.",
            ]
            .into_iter()
            .map(Line::from)
            .collect(),
        };
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}
fn tier_for(a: Rect) -> Tier {
    layout::tier(a)
}
fn padded(a: Rect, n: u16) -> Rect {
    Rect::new(
        a.x + n.min(a.width / 2),
        a.y + n.min(a.height / 2),
        a.width.saturating_sub(n * 2),
        a.height.saturating_sub(n * 2),
    )
}
fn draw_cover(f: &mut Frame, a: Rect, art: &Artwork) {
    if a.is_empty() {
        return;
    }
    let width = art
        .image
        .width()
        .min(art.image.height().saturating_mul(2))
        .max(1);
    let height = art.image.height().min(width / 2).max(1);
    let crop = image::imageops::crop_imm(
        &art.image,
        (art.image.width() - width) / 2,
        (art.image.height() - height) / 2,
        width,
        height,
    )
    .to_image();
    let im = image::imageops::resize(
        &crop,
        a.width as u32,
        a.height as u32 * 2,
        image::imageops::FilterType::Lanczos3,
    );
    for y in 0..a.height {
        let line = Line::from(
            (0..a.width)
                .map(|x| {
                    let top = im.get_pixel(x as u32, y as u32 * 2).0;
                    let bottom = im.get_pixel(x as u32, y as u32 * 2 + 1).0;
                    Span::styled(
                        "▀",
                        Style::default()
                            .fg(Color::Rgb(top[0], top[1], top[2]))
                            .bg(Color::Rgb(bottom[0], bottom[1], bottom[2])),
                    )
                })
                .collect::<Vec<_>>(),
        );
        f.render_widget(Paragraph::new(line), Rect::new(a.x, a.y + y, a.width, 1));
    }
}
fn fmt(s: f32) -> String {
    format!("{}:{:02}", s.max(0.0) as u32 / 60, s.max(0.0) as u32 % 60)
}
pub fn doctor() -> Result<()> {
    println!("LyricTTY doctor (development)");
    for name in ["mpv", "yt-dlp"] {
        let ok = Command::new(name)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        println!("{} {name}", if ok { "✓" } else { "!" });
    }
    match lyrictty::playback::mpris::MprisBackend::new() {
        Ok(mut backend) => {
            use lyrictty::playback::PlaybackBackend;
            let sources = backend.refresh()?;
            println!("✓ D-Bus / MPRIS ({} sources)", sources.len());
            for s in sources {
                println!(
                    "  {} · {:?} · seek {}",
                    s.source_name, s.status, s.capabilities.seek
                );
            }
        }
        Err(_) => println!("! D-Bus session unavailable"),
    }
    println!(
        "{} Kitty terminal (ANSI artwork fallback enabled)",
        if std::env::var_os("KITTY_WINDOW_ID").is_some() {
            "✓"
        } else {
            "!"
        }
    );
    println!(
        "{} PipeWire (ambient fallback; no capture backend)",
        if Command::new("pw-cli")
            .arg("info")
            .arg("0")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            "✓"
        } else {
            "!"
        }
    );
    println!("✓ Lyrics: local enhanced LRC / Lyricsfile + LRCLIB\n! YouTube Music account integration unavailable; public URLs use yt-dlp\n✓ No account required");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    #[test]
    fn responsive_ui_handles_tiny_compact_standard_wide_and_resize() {
        let mut app = App::new(Config::default(), true);
        for (w, h) in [(1, 1), (12, 4), (60, 18), (110, 30), (170, 50), (45, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
        }
    }
    #[test]
    fn source_disconnection_clears_stale_track() {
        let mut app = App::new(Config::default(), false);
        app.update_sources(vec![PlaybackSnapshot {
            source_id: "mock".into(),
            track: lyrictty::playback::Track {
                id: "t".into(),
                title: "song".into(),
                ..Default::default()
            },
            status: PlaybackStatus::Playing,
            ..Default::default()
        }]);
        assert!(app.source().is_some());
        app.update_sources(Vec::new());
        assert!(app.source().is_none());
        assert!(app.content.lines.is_empty());
    }
    #[test]
    fn auto_does_not_jump_between_multiple_playing_sources() {
        let mut app = App::new(Config::default(), false);
        app.update_sources(vec![
            PlaybackSnapshot {
                source_id: "a".into(),
                status: PlaybackStatus::Playing,
                ..Default::default()
            },
            PlaybackSnapshot {
                source_id: "b".into(),
                status: PlaybackStatus::Playing,
                ..Default::default()
            },
        ]);
        assert!(app.active_source.is_none());
        assert!(app.overlay == Overlay::Sources);
    }
    #[test]
    fn rendered_hit_regions_send_actual_backend_seek_after_resize() {
        use lyrictty::playback::{Capabilities, PlaybackBackend};
        use std::sync::{Arc, Mutex};
        let log = Arc::new(Mutex::new(Vec::new()));
        let source = PlaybackSnapshot {
            source_id: "mock".into(),
            track: lyrictty::playback::Track {
                id: "track".into(),
                title: "test".into(),
                ..Default::default()
            },
            capabilities: Capabilities {
                seek: true,
                ..Default::default()
            },
            ..Default::default()
        };
        struct Fake {
            log: Arc<Mutex<Vec<PlaybackCommand>>>,
            source: PlaybackSnapshot,
        }
        impl PlaybackBackend for Fake {
            fn refresh(&mut self) -> Result<Vec<PlaybackSnapshot>> {
                Ok(vec![self.source.clone()])
            }
            fn command(&mut self, _: &str, c: PlaybackCommand) -> Result<()> {
                self.log.lock().unwrap().push(c);
                Ok(())
            }
            fn changed(&mut self) -> bool {
                false
            }
        }
        let worker_log = log.clone();
        let worker_source = source.clone();
        let mut app = App::new(Config::default(), false);
        app.service = Some(PlaybackService::start(move || {
            (
                vec![Box::new(Fake {
                    log: worker_log,
                    source: worker_source,
                })],
                Vec::new(),
            )
        }));
        app.update_sources(vec![source]);
        app.content.lines = lyrics::parse_lrc("[00:00]zero\n[00:10]one\n[00:20]two\n[00:30]three");
        app.scroll.target(2.0);
        app.scroll.advance(1.0, 0.8);
        for (w, h, pixel) in [
            (120, 32, false),
            (52, 17, false),
            (120, 32, true),
            (52, 17, true),
        ] {
            app.pixel_lyrics = if pixel {
                Some(
                    PixelLyrics::system_font((10, 20)).expect("Install fontconfig and a sans font"),
                )
            } else {
                None
            };
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| app.draw(f)).unwrap();
            assert_eq!(app.pixel_active, pixel);
            let hit = app.hits.iter().find(|hit| hit.line == 2).unwrap().clone();
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: hit.area.x,
                row: hit.area.y,
                modifiers: KeyModifiers::NONE,
            });
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while log.lock().unwrap().len() < 4 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .filter(|c| matches!(c,PlaybackCommand::SetPosition(p)if (*p-20.0).abs()<0.01))
                .count()
                == 4
        );
        assert_eq!(
            app.position(),
            0.0,
            "Seek must not fake acknowledgement locally"
        );
    }
    #[test]
    fn pasted_urls_only_modify_open_dialog() {
        let mut app = App::new(Config::default(), false);
        app.handle_paste("q");
        assert!(!app.should_quit);
        assert!(app.url_input.is_empty());
        app.overlay = Overlay::Open;
        app.handle_paste("https://example.test/q\n");
        assert_eq!(app.url_input, "https://example.test/q");
    }
    #[test]
    fn late_artwork_invalidates_assets_without_clearing_existing_lyrics() {
        let mut app = App::new(Config::default(), false);
        let mut source = PlaybackSnapshot {
            source_id: "mock".into(),
            track: lyrictty::playback::Track {
                id: "track".into(),
                title: "song".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        app.update_sources(vec![source.clone()]);
        let old_asset_key = app.asset_key.clone();
        app.content.lines = lyrics::parse_lrc("[00:00]existing lyrics");
        source.track.artwork_url = Some("https://art.invalid/cover.png".into());
        source.track.duration = 120.0;
        app.update_sources(vec![source]);
        assert_ne!(old_asset_key, app.asset_key);
        assert_eq!(app.content.lines.len(), 1);
    }
}

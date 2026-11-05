use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::time::Instant;
pub mod demo;
pub mod mpris;
pub mod native;
pub mod service;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub artwork_url: Option<String>,
    pub url: Option<String>,
    pub duration: f64,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaybackStatus {
    Playing,
    Paused,
    #[default]
    Stopped,
}
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    pub play: bool,
    pub pause: bool,
    pub seek: bool,
    pub next: bool,
    pub previous: bool,
    pub volume: bool,
}
#[derive(Clone, Debug)]
pub struct PlaybackSnapshot {
    pub source_id: String,
    pub source_name: String,
    pub track: Track,
    pub status: PlaybackStatus,
    pub position: f64,
    pub rate: f64,
    pub volume: f64,
    pub capabilities: Capabilities,
    pub updated_at: Instant,
}
impl Default for PlaybackSnapshot {
    fn default() -> Self {
        Self {
            source_id: String::new(),
            source_name: String::new(),
            track: Track::default(),
            status: PlaybackStatus::Stopped,
            position: 0.0,
            rate: 1.0,
            volume: 1.0,
            capabilities: Capabilities::default(),
            updated_at: Instant::now(),
        }
    }
}
impl PlaybackSnapshot {
    pub fn position_now(&self) -> f64 {
        let p = self.position
            + if self.status == PlaybackStatus::Playing {
                self.updated_at.elapsed().as_secs_f64() * self.rate
            } else {
                0.0
            };
        if self.track.duration > 0.0 {
            p.clamp(0.0, self.track.duration)
        } else {
            p.max(0.0)
        }
    }
}
#[derive(Clone, Debug)]
pub enum PlaybackCommand {
    Play,
    Pause,
    PlayPause,
    Next,
    Previous,
    Repeat,
    Shuffle,
    Seek(f64),
    SetPosition(f64),
    Volume(f64),
    Open(String),
}
/// Synchronous methods run on an I/O worker, never the terminal thread.
pub trait PlaybackBackend: Send {
    fn refresh(&mut self) -> Result<Vec<PlaybackSnapshot>>;
    fn command(&mut self, source_id: &str, command: PlaybackCommand) -> Result<()>;
    /// Discover actual subsequent queue entries without changing playback.
    fn upcoming(&mut self, _source_id: &str, _limit: usize) -> Result<Vec<Track>> {
        Ok(vec![])
    }
    /// True after an observed backend event, not a network poll.
    fn changed(&mut self) -> bool;
}
#[derive(Debug, PartialEq, Eq)]
pub enum AutoSelection {
    None,
    One(String),
    Ambiguous(Vec<String>),
}
pub fn select_auto(sources: &[PlaybackSnapshot]) -> AutoSelection {
    let playing: Vec<_> = sources
        .iter()
        .filter(|s| s.status == PlaybackStatus::Playing)
        .map(|s| s.source_id.clone())
        .collect();
    match playing.len() {
        0 => {
            if sources.len() == 1 {
                AutoSelection::One(sources[0].source_id.clone())
            } else {
                AutoSelection::None
            }
        }
        1 => AutoSelection::One(playing[0].clone()),
        _ => AutoSelection::Ambiguous(playing),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_requires_choice_for_simultaneous_playback() {
        let a = PlaybackSnapshot {
            source_id: "a".into(),
            status: PlaybackStatus::Playing,
            ..Default::default()
        };
        let b = PlaybackSnapshot {
            source_id: "b".into(),
            status: PlaybackStatus::Playing,
            ..Default::default()
        };
        assert!(matches!(select_auto(&[a, b]), AutoSelection::Ambiguous(_)));
    }
    #[test]
    fn paused_position_does_not_advance() {
        let a = PlaybackSnapshot {
            position: 12.0,
            status: PlaybackStatus::Paused,
            ..Default::default()
        };
        assert_eq!(a.position_now(), 12.0);
    }
}

//! Explicit offline fixture for UI checks; never used in normal playback.
use super::*;
pub struct DemoBackend {
    state: PlaybackSnapshot,
}
impl Default for DemoBackend {
    fn default() -> Self {
        Self {
            state: PlaybackSnapshot {
                source_id: "demo".into(),
                source_name: "Demo · authored timing fixture".into(),
                track: Track {
                    id: "terminal-skyline".into(),
                    title: "Terminal Skyline".into(),
                    artist: "LyricTTY contributors".into(),
                    album: "Offline UI demonstration · no audio".into(),
                    duration: 70.0,
                    ..Default::default()
                },
                status: PlaybackStatus::Playing,
                capabilities: Capabilities {
                    play: true,
                    pause: true,
                    seek: true,
                    next: false,
                    previous: false,
                    volume: true,
                },
                ..Default::default()
            },
        }
    }
}
impl PlaybackBackend for DemoBackend {
    fn refresh(&mut self) -> Result<Vec<PlaybackSnapshot>> {
        Ok(vec![self.state.clone()])
    }
    fn changed(&mut self) -> bool {
        false
    }
    fn command(&mut self, _: &str, c: PlaybackCommand) -> Result<()> {
        self.state.position = self.state.position_now();
        self.state.updated_at = Instant::now();
        match c {
            PlaybackCommand::Play => self.state.status = PlaybackStatus::Playing,
            PlaybackCommand::Pause => self.state.status = PlaybackStatus::Paused,
            PlaybackCommand::PlayPause => {
                self.state.status = if self.state.status == PlaybackStatus::Playing {
                    PlaybackStatus::Paused
                } else {
                    PlaybackStatus::Playing
                }
            }
            PlaybackCommand::SetPosition(p) => self.state.position = p.clamp(0.0, 70.0),
            PlaybackCommand::Seek(d) => {
                self.state.position = (self.state.position + d).clamp(0.0, 70.0)
            }
            PlaybackCommand::Volume(v) => self.state.volume = v,
            PlaybackCommand::Repeat | PlaybackCommand::Shuffle => {
                anyhow::bail!("Unavailable in the silent demo")
            }
            _ => {}
        }
        Ok(())
    }
}

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub audio_alignment: bool,
    pub prepare_before_playback: bool,
    pub fps: u16,
    pub pixel_lyrics: bool,
    pub scroll_duration_ms: u32,
    pub theme_transition_ms: u32,
    pub manual_follow_timeout_secs: u32,
    pub instrumental_threshold_secs: f32,
    pub instrumental_fade_secs: f32,
    pub show_artwork: bool,
    pub low_data: bool,
    pub lyrics_offset_ms: i32,
    pub online_lyrics: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            audio_alignment: true,
            prepare_before_playback: true,
            fps: 60,
            pixel_lyrics: true,
            scroll_duration_ms: 850,
            theme_transition_ms: 900,
            manual_follow_timeout_secs: 8,
            instrumental_threshold_secs: 8.0,
            instrumental_fade_secs: 2.0,
            show_artwork: true,
            low_data: false,
            lyrics_offset_ms: 0,
            online_lyrics: true,
        }
    }
}
impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let mut config: Self = toml::from_str(&text).context("Invalid LyricTTY configuration")?;
        config.fps = config.fps.clamp(10, 60);
        config.instrumental_threshold_secs = config.instrumental_threshold_secs.clamp(4.0, 120.0);
        config.instrumental_fade_secs = config.instrumental_fade_secs.clamp(0.2, 5.0);
        Ok(config)
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lyrictty/config.toml")
}
pub fn cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("lyrictty")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_config() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("config.toml");
        let c = Config {
            fps: 15,
            ..Default::default()
        };
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap().fps, 15);
    }
}

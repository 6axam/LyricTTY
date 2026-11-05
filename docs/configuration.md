# Configuration

Location: `$XDG_CONFIG_HOME/lyrictty/config.toml`, normally `~/.config/lyrictty/config.toml`. The settings dialog (`,`) edits FPS, lyric/theme transition durations, artwork visibility, lyric offset and low-data mode. Other options are file-editable. Unknown fields and malformed TOML produce an explicit startup error.

```toml
fps = 60
pixel_lyrics = true
scroll_duration_ms = 850
theme_transition_ms = 900
manual_follow_timeout_secs = 8
instrumental_threshold_secs = 8.0
instrumental_fade_secs = 2.0
show_artwork = true
low_data = false
lyrics_offset_ms = 0
online_lyrics = true
```

FPS is clamped to 10–60. `pixel_lyrics` enables pixel-positioned text in compatible Kitty sessions. Glyph masks and line images are cached; unchanged lines are moved without re-uploading images. Fontconfig chooses a bold sans font, or `LYRICTTY_LYRICS_FONT` can point to a local TTF/OTF. Complex scripts and unsupported graphemes preserve the terminal renderer's shaping/fallback instead. Disable the option for the original cell renderer. Existing explicit FPS settings are preserved; choose 60 in `,` to use the smoother preset.

Positive `lyrics_offset_ms` advances highlighting without changing playback; negative values delay it. `scroll_duration_ms = 0` is instant. Low-data mode skips artwork retrieval; setting `online_lyrics = false` disables remote lyrics while preserving explicit local files. Network-related setting changes take effect on restart.

Caches live under `$XDG_CACHE_HOME/lyrictty/{lyrics,artwork}`. Lyrics keys are hashed, positive cache TTL is seven days and missing records use a shorter TTL. Provider errors are not cached as definitive misses. File-backed local lyrics are reread on track changes. Artwork is bounded in download and decoded dimensions; no account secrets are stored.

Native queue/source/position persistence is not yet implemented. The application owns only the mpv process it starts, and terminates/reaps it when exiting; it never terminates external MPRIS players.

# Changelog

## Unreleased — development / pre-release

- Prefer synchronized matching lyrics over a plain-text result, and prepare acoustic word timings for text without line timestamps.
- Render untimed lyrics at the normal large size; broaden bounded audio search and ignore album/presentation decorations while retaining recording-version and duration checks.
- Refreshed the project identity, README, current demo screenshot and word-sync guide.
- Buffered complete terminal frames and alternated Kitty image slots so uploads keep the previous card and lyric lines visible.
- Added local stable-ts / multilingual Whisper CPU alignment, persistent word cues and preparation of up to three known upcoming tracks.
- Rebalanced the player around square artwork and a rounded graphical control card.
- Added real CAVA spectrum output during instrumental passages, captured from the playback monitor.
- Added actual pixel-positioned lyrics in Kitty, cached glyph/line images, larger type and preserved word/line hit regions.
- Improved animation pacing, minimum-jerk movement and retarget velocity continuity; defaults now use 60 FPS while animated.

- Fixed warnings-as-errors CI; no global dead-code suppression.
- Added direct, signal-driven MPRIS discovery/control and source selection.
- Added owned mpv JSON IPC playback, yt-dlp URLs, metadata, playlist transport and cleanup.
- Added cached LRCLIB and local Lyricsfile/enhanced-LRC providers, actual word highlighting and Unicode wrapping.
- Added eased fractional-row lyric rendering, render-derived click regions and manual follow.
- Added artwork caching, Kitty Graphics, readable adaptive palettes and smooth color transitions.
- Added instrumental phases, responsive layouts, karaoke, settings and installation packaging.
- Added isolated MPRIS/mpv/HTTP integration tests, optional real mpv tests, and live verification tools.

The package version remains 0.1.0 for compatibility. No v0.1.0 release/tag has been created.

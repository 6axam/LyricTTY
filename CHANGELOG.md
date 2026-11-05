# Changelog

## Unreleased — development / pre-release

- Added actual pixel-positioned lyrics in Kitty, cached glyph/line images, larger type and preserved word/line hit regions.
- Improved animation pacing, minimum-jerk movement and retarget velocity continuity; defaults now use 60 FPS while animated.

- Fixed warnings-as-errors CI; no global dead-code suppression.
- Added direct, signal-driven MPRIS discovery/control and source selection.
- Added owned mpv JSON IPC playback, yt-dlp URLs, metadata, playlist transport and cleanup.
- Added cached LRCLIB and local Lyricsfile/enhanced-LRC providers, actual word highlighting and Unicode wrapping.
- Added eased fractional-row lyric rendering, render-derived click regions and manual follow.
- Added artwork caching, Kitty Graphics, readable adaptive palettes and smooth color transitions.
- Added instrumental phases and ambient fallback, responsive layouts, karaoke, settings and installation packaging.
- Added isolated MPRIS/mpv/HTTP integration tests, optional real mpv tests, and live verification tools.

The package version remains 0.1.0 for compatibility. No v0.1.0 release/tag has been created.

# Architecture

The TUI consumes source snapshots and sends backend-neutral commands. Playback I/O and asset retrieval run on separate workers. A lyric/artwork failure does not stop playback.

```text
Ratatui UI / App / hit regions
  ├── PlaybackService → PlaybackBackend
  │                     ├── MprisBackend (zbus, session D-Bus)
  │                     └── NativeBackend (mpv Unix JSON IPC)
  └── AssetService
       ├── LyricsService → LyricsProvider → LocalLyrics / LRCLIB
       ├── AlignmentHandle → separate audio → Vosk word cues / prepared timeline cache
       └── ArtworkProvider → bounded decode / disk cache → ThemeTransition
```

`PlaybackSnapshot` contains metadata, capabilities, position, rate and an `Instant` anchor. The UI extrapolates position only while playing. Backend commands do not optimistically pretend to succeed. MPRIS uses filtered owner/property/seek signals, cached metadata and position-only resync; unique owners prevent replacement races. Native IPC correlates request IDs, consumes property/file events and seeds a new file's properties once; it never parses stdout. Private socket directories are `0700`, sockets `0600`, and owned children/readers are cleaned up.

`lyrics` preserves exact source word starts/ends and UTF-8 text. `providers` understands ordinary/enhanced LRC and [Lyricsfile](https://github.com/tranxuanthang/lyricsfile), plus the current [LRCLIB API](https://lrclib.net/docs). Explicit local files take precedence. Bounded identified HTTP requests have timeouts and rate-limit cooldowns. Hashed cache keys include normalized metadata, album, duration and provider identity. Provider failures are distinct from definitive no-lyrics results.

`lyric_view` uses minimum-jerk interpolation and retains velocity when a new target arrives mid-transition. Its portable renderer calculates wrapped row offsets and splits each row's intensity between adjacent terminal rows. Word progress is grapheme-safe. Hit regions are generated from the same visible layout, including each word's real start, so resize/manual scroll cannot cause row-to-index mistakes.

In Kitty, `pixel_lyrics` uses cached fontdue glyph masks and PNG line images. Ordinary scroll frames send placement commands with within-cell pixel offsets; line images are re-uploaded only when their opacity, palette or word fill changes. Source crops clip lines at pane boundaries, and pixel rectangles produce mouse hit regions. Overlays remove placements without discarding image caches; exit/track/layout changes free owned image IDs. Complex scripts and combining graphemes fall back to the existing terminal renderer. Font/cell-size changes rebuild layout.

`FramePacer` schedules frame starts from deadlines using nanosecond periods, subtracting render time from the next wait and dropping missed frames. Input does not reset the cadence. Kitty synchronized updates batch terminal and pixel changes into one presentation; static idle screens remain event-driven.

Artwork is cached by hashed URL. Quantized, saturation-weighted color bins select a dominant color; dark palette generation checks foreground/accent contrast. `ThemeTransition` interpolates from the current intermediate palette. Kitty uses a cached image protocol widget; other terminals can use ANSI halfblocks.

`InstrumentalState` exposes Lyrics → EnteringInstrumental → Instrumental → LeavingInstrumental → Lyrics. Missing lyrics are not evidence of an instrumental track. Explicit word ends and lyric gaps drive the fade; plain LRC requires an estimate. The current animation is intentionally labeled ambient, ready for a future audio-analysis source.

`layout` computes bounded Compact/Standard/Wide regions; the renderer rebuilds hit regions on every draw. `config` owns TOML settings. `--demo` selects an explicit silent fixture backend and authored assets; normal startup never injects demo metadata.

Audio preparation runs in a cancellable process group on a separate worker. It resolves and validates the recording, decodes mono audio, and aligns known lyric sequences before publishing a completed timeline. Progress and completion events control the optional playback hold; source generations prevent stale jobs from resuming a different song. Process output is drained through EOF before completion, and temporary audio is removed on success, failure, cancellation or exit. UI frames use only playback position and saved word intervals. Both lyric renderers wrap visible words independently of timing/syllable boundaries.

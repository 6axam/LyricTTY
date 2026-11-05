# Configuration

Location: `$XDG_CONFIG_HOME/lyrictty/config.toml`, normally `~/.config/lyrictty/config.toml`. The settings dialog (`,`) edits FPS, lyric/theme transition durations, artwork visibility, lyric offset and low-data mode. Other options are file-editable. Unknown fields and malformed TOML produce an explicit startup error.

```toml
audio_alignment = true
alignment_threads = 0
prefetch_tracks = 3
prepare_before_playback = false
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

Caches live under `$XDG_CACHE_HOME/lyrictty/{lyrics,artwork,stable-words-v1}`. Lyrics keys are hashed, positive cache TTL is seven days and missing records use a shorter TTL. Provider errors are not cached as definitive misses. File-backed local lyrics are reread on track changes. Artwork is bounded in download and decoded dimensions; no account secrets are stored.

Native queue/source/position persistence is not yet implemented. The application owns only the mpv process it starts, and terminates/reaps it when exiting; it never terminates external MPRIS players.

`audio_alignment` aligns existing lyrics against a separately loaded audio file with stable-ts `align_words()` and the multilingual Whisper base model. Install once using `scripts/setup-alignment.sh base`; `ru`/`en` remain aliases for the same multilingual setup. Runtime/models live under `$XDG_DATA_HOME/lyrictty/alignment`, with the checkpoint at `models/stable-ts/base.pt`. A fully prepared cache starts no model. Temporary decoded audio is deleted after analysis; nothing is uploaded or recorded from the microphone.

`prepare_before_playback = false` is the default: music continues while alignment runs independently. Complete lines are displayed as they become ready. The full cache is marked prepared only with 100% original lexical-word coverage and exact text reconstruction; timing accuracy is not guaranteed. Failed lines are retried, and unresolved words produce a visible error instead of fabricated timings. Explicit point cues for short particles are supported separately from words with missing timing; no uniform duration is manufactured. Explicitly set `prepare_before_playback = true` to pause until preparation; Space overrides waiting and failure releases playback. Source audio requires `yt-dlp`, `ffmpeg`, and `ffprobe` for public URLs; fallback search validates recording identity and duration.

`alignment_threads = 0` uses all CPUs available to the process for a burst of alignment work. Positive values cap the thread budget; the process stays at low scheduling priority. Torch intra-operation threads share this budget, while inter-operation parallelism stays at one to avoid nested oversubscription. No single-core affinity restriction remains.

`prefetch_tracks` is clamped to 0–3 (default 3, 0 disables it). Queue metadata and lyric fetching happen off the playback/UI workers; heavy alignment remains sequential and gives active-song requests priority. Queue changes cancel obsolete speculative work. Completed timeline caches are shared with subsequent active playback. Discovery requires an actual native playlist or an MPRIS TrackList (and predictable order); unavailable browser queues are not inferred. Online lyrics must be enabled for automatic future-track lyric lookup.

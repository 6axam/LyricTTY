# Word sync

[← README](../README.md) · [Configuration](configuration.md)

LyricTTY uses supplied word cues when available. For missing cues, optional local alignment fits the existing lyrics to audio; it does not replace them with a fresh transcription.

## Setup

```bash
./scripts/setup-alignment.sh base
```

The script installs stable-ts and CPU PyTorch in a private environment and downloads the original multilingual Whisper base model. It uses `uv` when available, otherwise Python's `venv` and pip. Runtime and model files live under `$XDG_DATA_HOME/lyrictty/alignment` (normally `~/.local/share/lyrictty/alignment`). This is a one-time runtime/model download; no GPU is used.

Audio preparation requires `ffmpeg` and `ffprobe`, plus `yt-dlp` for public remote sources. Local files and direct source URLs are preferred. If a browser provides no source URL, a bounded public lookup checks artist, title, recording version and duration; uncertain recordings are rejected.

## While you listen

`prepare_before_playback = false` is the default. Playback continues while a background worker prepares cues, with line lyrics available in the meantime. Completed lines appear incrementally. Opt into waiting with `prepare_before_playback = true`; Space overrides the wait, and preparation failure releases playback.

Stable-ts `align_words()` uses existing line timestamps as anchors. Original words, spelling, whitespace and punctuation are preserved. Failed lines are retried independently. Unresolved words produce a visible coverage error instead of uniform fabricated timestamps. A collapsed cue for a short particle can remain an explicit point cue between timed neighbors; no duration is invented.

When the catalog supplies only plain text, LyricTTY first checks matching releases for synchronized lyrics. Otherwise it shows readable, manually scrollable text and aligns the existing words against the full audio with stable-ts `align()`. Complete acoustic word cues restore the original line breaks and enable following and seeking; the resulting timeline is cached. No evenly spaced line or word timestamps are synthesized.

A full result is marked prepared only when all original lexical words have valid intervals or supported explicit point cues and the text reconstructs exactly. **100% text coverage does not establish exact acoustic timing.** Singing, accompaniment and inaccurate line boundaries can still impair alignment.

## CPU and cache

`alignment_threads = 0` uses all CPU threads available to the process at low scheduling priority. Set a positive value to limit the budget. Only one model task runs at once, using float32 on CPU.

Prepared timelines are stored under `$XDG_CACHE_HOME/lyrictty/stable-words-v1` (normally `~/.cache/lyrictty/stable-words-v1`). Reusing a prepared timeline does not load the model. Recording metadata and lyric content allow reuse between queued and active tracks; older URL-specific entries remain readable. Old Vosk results are kept separate and are not accepted as complete stable-ts results.

## Upcoming tracks

`prefetch_tracks = 3` prepares lyrics and timelines for up to three real upcoming tracks; set 0 to disable it. The active track takes priority and interrupts speculative analysis when you switch songs. Completed work is reused.

Discovery requires mpv's actual playlist or a standard MPRIS TrackList with predictable order. Browsers exposing only the current track cannot provide an upcoming queue. MPRIS shuffle disables speculative preparation because TrackList does not predict shuffle order. Missing native queue metadata is resolved separately with ffprobe/yt-dlp; tracks are never played or skipped to discover the queue.

## Data and controls

Audio is loaded separately for analysis. Temporary audio is deleted afterward; nothing is uploaded or captured from the microphone. No browser cookies or account credentials are used. Set `audio_alignment = false` to disable analysis. See the [configuration reference](configuration.md) for all settings.

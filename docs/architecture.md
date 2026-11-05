# Architecture

The UI owns rendering and input. `App` owns transient playback position and presentation state. `lyrics` is a pure timing/model module; `theme` maps artwork colors to dark readable terminal colors. Future `PlaybackBackend`, `LyricsProvider`, `ArtworkProvider`, and `AudioVisualizer` implementations should communicate with `App` through state/events rather than direct UI calls.

Native playback will use `mpv` IPC with `yt-dlp`. Browser playback will discover `org.mpris.MediaPlayer2.*` names dynamically (no browser-specific branching).

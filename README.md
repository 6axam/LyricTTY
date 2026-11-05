# LyricTTY

Spotify-like synced lyrics and adaptive music UI for your terminal.

LyricTTY is a lightweight Rust TUI foundation for a terminal music center. It provides a dark, lyric-first interface, smooth active-line following, click-to-seek lyrics, keyboard controls, a color palette engine, and a dependency diagnostic command. The current 0.1.0 slice is deliberately honest: playback adapters are being added behind the UI rather than simulated as completed integrations.

## Run

```bash
cargo run --release
# or
./install.sh && lyrictty
```

Press `Space` to play/pause the demo clock, arrows or `h`/`l` to seek, `k`/`j` to change volume, click a lyric line to seek, `?` for help, and `q` to exit.

## Diagnostics

```bash
lyrictty doctor
lyrictty --debug
```

`doctor` checks optional local dependencies (`mpv`, `yt-dlp`, and `playerctl`) without printing credentials or personal paths.

## Architecture

The project is split into a TUI/application layer, lyric timing model, and theme engine. The lyric model supports line timing and word timing (`LyricWord`) so provider adapters can add LRCLIB/Lyricsfile data without coupling the renderer to a website. Planned adapters include native mpv IPC and dynamic MPRIS discovery.

See [docs/architecture.md](docs/architecture.md) and [docs/configuration.md](docs/configuration.md).

## Development

```bash
cargo fmt --all
cargo test
cargo build --release
```

MIT licensed. See [CHANGELOG.md](CHANGELOG.md).
